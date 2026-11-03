//! Every night, the model suggests lore from the last day's messages in one channel
//! (`LLM_NIGHTLY_LORE_CHANNEL_ID`), and the suggestions are announced there with a link to vote.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use globibot_core::{
    interaction::CommandCtx,
    rpc,
    serenity::all::{ChannelId, UserId},
};
use itertools::Itertools;
use parking_lot::Mutex;

use crate::{
    LlmPlugin,
    fun::{sanitize_llm_output, transcript_line},
    openrouter::{Message as LlmMessage, Role, ToolOutput},
    tool::ToolSpec,
    tool_traces::{Outcome, ToolCallRecord, ToolTrace},
    tools::{self, LlmTool, SuggestLore},
    unix_now,
    usage::UsageKind,
};

// ponytail: a fixed UTC hour: midnight in Paris in winter, 1am in summer. Follow DST if it matters.
const RUN_AT_SECS_OF_DAY: u64 = 23 * 3600;
const DAY_SECS: u64 = 24 * 3600;
/// Quieter days are skipped
const MIN_MESSAGES: usize = 20;
const MAX_SUGGESTIONS: usize = 5;
const LORE_BOOK_URL: &str = "https://globibot.com/plugins/llm#lore_book";

/// Runs the suggestions every night, forever. Never returns, even with no channel set.
pub async fn run_every_night(plugin: Arc<LlmPlugin>, rpc: rpc::ProtocolClient) {
    let Some(channel_id) = plugin.nightly_lore_channel_id else {
        return std::future::pending().await;
    };

    let mut next_run = next_run_after(unix_now());
    loop {
        // Never more than an hour at a time: the clock can jump
        let wait = next_run.saturating_sub(unix_now()).min(3600);
        tokio::time::sleep(Duration::from_secs(wait)).await;
        if unix_now() < next_run {
            continue;
        }

        match plugin.suggest_nightly_lore(&rpc, channel_id).await {
            Ok(outcome) => tracing::info!("Nightly lore: {outcome}"),
            Err(err) => tracing::error!("Nightly lore failed: {err:?}"),
        }
        next_run = next_run_after(unix_now());
    }
}

fn next_run_after(now: u64) -> u64 {
    let today = now - now % DAY_SECS + RUN_AT_SECS_OF_DAY;
    if now < today { today } else { today + DAY_SECS }
}

impl LlmPlugin {
    /// `/llm nightly-lore`: runs them now, for the admin. Posts the announcement as usual.
    pub async fn nightly_lore_now(&self, ctx: &CommandCtx<'_>) -> anyhow::Result<()> {
        if ctx.interaction.user.id != self.admin_id {
            let content = format!("Only <@{}> can run this", self.admin_id);
            return Ok(ctx.reply_ephemeral(content).await?);
        }
        let Some(channel_id) = self.nightly_lore_channel_id else {
            return Ok(ctx
                .reply_ephemeral("`LLM_NIGHTLY_LORE_CHANNEL_ID` isn't set")
                .await?);
        };

        ctx.defer_ephemeral().await?;
        let outcome = self
            .suggest_nightly_lore(ctx.rpc, channel_id)
            .await
            .unwrap_or_else(|err| format!("Failed: {err:#}"));
        ctx.edit(outcome).await?;
        Ok(())
    }

    /// Suggests lore from the last day's messages in `channel_id` and announces it there.
    /// Says what happened.
    async fn suggest_nightly_lore(
        &self,
        rpc: &rpc::ProtocolClient,
        channel_id: ChannelId,
    ) -> anyhow::Result<String> {
        let since = unix_now().saturating_sub(DAY_SECS) as i64;
        let (message_count, transcript) = {
            let contexts_by_channel = self.contexts_by_channel.lock();
            let Some(context) = contexts_by_channel.get(&channel_id) else {
                return Ok("No messages seen in the channel since startup".to_string());
            };
            // Message ids hold their creation time. The bot's answers have no id: they're kept
            // from the first recent message on.
            let recent = context
                .messages
                .iter()
                .skip_while(|context_message| {
                    !context_message
                        .id
                        .is_some_and(|id| id.created_at().unix_timestamp() >= since)
                })
                .collect_vec();
            let message_count = recent.iter().filter(|m| m.id.is_some()).count();
            let transcript = recent
                .iter()
                .map(|context_message| transcript_line(&context_message.message))
                .join("\n");
            (message_count, transcript)
        };
        if message_count < MIN_MESSAGES {
            return Ok(format!(
                "Only {message_count} messages in the last day, skipped"
            ));
        }

        let prompt = format!(
            "The day is over. Here are the last day's messages in this channel:\n\n{transcript}\n\n\
            Pick at most {MAX_SUGGESTIONS} new facts about people in these messages that aren't \
            in their lore yet, and call suggest_lore once for each, all at once. Lore is the \
            chat's mythology, not a bio: the best facts come from the funny, absurd or memorable \
            moments of the day (a bad take, a running joke, an embarrassing mishap, a weird \
            obsession) turned into a deadpan \"fact\" about the person, a little exaggerated if \
            it's funnier. Tonight this replaces suggest_lore's \"clear and lasting fact\" rule: \
            it doesn't have to be literally true or stated by the person, only true to what \
            happened. Plain facts are fine too when they're interesting. Still: playful, not \
            mean, nothing sensitive, nothing someone asked to put on someone else. If nothing \
            is worth it, suggest nothing. Don't write anything else."
        );

        let model = self.llm_model.read().clone();
        let tools = [SuggestLore::definition()];
        let started = Instant::now();
        // Filled by each tool run: the calls for the trace, and who each suggestion is about
        let calls = Mutex::new(Vec::new());
        let suggested_for = Mutex::new(Vec::<UserId>::new());
        let (calls_ref, suggested_ref) = (&calls, &suggested_for);
        let completion = self
            .llm_client
            .complete_with_tools(
                UsageKind::NightlyLore,
                model.clone(),
                format!("nightly-lore-{channel_id}"),
                // The whole lore book, so it doesn't suggest what's already there
                self.system_prompt(),
                vec![LlmMessage::text(Role::User, prompt)],
                &tools,
                move |name, arguments| async move {
                    let result = match LlmTool::parse(&name, &arguments) {
                        Ok(LlmTool::SuggestLore(SuggestLore { user_id, fact })) => {
                            let mut suggested_for = suggested_ref.lock();
                            if suggested_for.len() >= MAX_SUGGESTIONS {
                                format!("At most {MAX_SUGGESTIONS} suggestions a night")
                            } else {
                                match tools::suggest_lore(
                                    &self.lore_book,
                                    &self.bot_profile,
                                    &user_id,
                                    &fact,
                                ) {
                                    Ok(user_id) => {
                                        suggested_for.push(user_id);
                                        tools::SUGGESTED.to_string()
                                    }
                                    Err(why) => why,
                                }
                            }
                        }
                        Ok(_) => "Only suggest_lore is available here".to_string(),
                        Err(err) => err,
                    };
                    calls_ref.lock().push(ToolCallRecord {
                        name,
                        arguments,
                        result: result.clone(),
                    });
                    ToolOutput::text(result)
                },
            )
            .await;
        let suggested_for = suggested_for.into_inner();

        // Even after a failure: what was suggested is up for a vote
        let summary = self.suggestions_summary(&suggested_for);
        let announcement = match &summary {
            Some(summary) => {
                let text =
                    format!("🌙 Tonight's lore suggestions: {summary}. Vote: <{LORE_BOOK_URL}>");
                let text = sanitize_llm_output(&text);
                let send = async {
                    anyhow::Ok(
                        rpc.send_message(rpc::context::current(), channel_id, text)
                            .await??,
                    )
                };
                Some(send.await)
            }
            None => None,
        };

        self.tool_traces.lock().record(ToolTrace {
            at: unix_now(),
            kind: UsageKind::NightlyLore,
            model,
            requester: Some("Nightly lore".to_string()),
            message_url: announcement
                .as_ref()
                .and_then(|sent| sent.as_ref().ok())
                .map(|sent| sent.id.link(channel_id, Some(self.guild_id))),
            calls: calls.into_inner(),
            duration_ms: started.elapsed().as_millis() as u64,
            outcome: match &completion {
                Ok(completion) => Outcome::Answered {
                    rounds: completion.rounds,
                    totals: completion.totals,
                },
                Err(err) => Outcome::FellBack {
                    error: format!("{err:#}"),
                },
            },
        });

        completion?;
        announcement.transpose()?;
        Ok(summary.map_or_else(
            || "Nothing to suggest tonight".to_string(),
            |summary| format!("Suggested {summary}"),
        ))
    }

    /// "3 new facts about Alice and Bob", or `None` without suggestions. Names, not mentions:
    /// no pings at midnight.
    fn suggestions_summary(&self, suggested_for: &[UserId]) -> Option<String> {
        if suggested_for.is_empty() {
            return None;
        }
        let lore_book = self.lore_book.read();
        let names = suggested_for
            .iter()
            .unique()
            .map(|user_id| {
                lore_book.lore_by_user.get(user_id).map_or_else(
                    || "someone".to_string(),
                    |lore| lore.member.username.clone(),
                )
            })
            .collect_vec();
        let facts = match suggested_for.len() {
            1 => "1 new fact".to_string(),
            count => format!("{count} new facts"),
        };
        Some(format!("{facts} about {}", join_names(&names)))
    }
}

/// "A", "A and B", "A, B and C"
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [name] => name.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_at_the_next_23h_utc() {
        let day = 20_000 * DAY_SECS;
        assert_eq!(next_run_after(day), day + RUN_AT_SECS_OF_DAY);
        assert_eq!(
            next_run_after(day + RUN_AT_SECS_OF_DAY - 1),
            day + RUN_AT_SECS_OF_DAY
        );
        // At or after the time: tomorrow, so a run never repeats
        assert_eq!(
            next_run_after(day + RUN_AT_SECS_OF_DAY),
            day + DAY_SECS + RUN_AT_SECS_OF_DAY
        );
        assert_eq!(
            next_run_after(day + DAY_SECS - 1),
            day + DAY_SECS + RUN_AT_SECS_OF_DAY
        );
    }

    #[test]
    fn joins_names() {
        let names = |n: &[&str]| join_names(&n.iter().map(|s| s.to_string()).collect_vec());
        assert_eq!(names(&["A"]), "A");
        assert_eq!(names(&["A", "B"]), "A and B");
        assert_eq!(names(&["A", "B", "C"]), "A, B and C");
    }
}
