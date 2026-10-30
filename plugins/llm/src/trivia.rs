use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use globibot_core::{
    rpc,
    serenity::all::{ChannelId, CommandInteraction, Message, UserId},
};
use parking_lot::Mutex;
use rand::seq::IndexedRandom;

use crate::{LlmPlugin, respond_ephemeral};

const ROUND_DURATION: Duration = Duration::from_secs(60);
const REDACTED: &str = "███";

/// Running `/lore-trivia` rounds, at most one per channel
#[derive(Debug, Default, Clone)]
pub struct TriviaRounds {
    by_channel: Arc<Mutex<HashMap<ChannelId, Round>>>,
    next_round_id: Arc<AtomicU64>,
}

#[derive(Debug)]
struct Round {
    id: u64,
    answer_id: UserId,
    answer_name: String,
}

impl LlmPlugin {
    /// `/lore-trivia`: posts someone's lore with their name hidden, the first to name them wins
    pub async fn start_lore_trivia(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<()> {
        let channel_id = interaction.channel_id;

        let pick = {
            let lore_book = self.lore_book.read();
            let candidates = lore_book
                .lore_by_user
                .values()
                .filter(|user_lore| !user_lore.lore.trim().is_empty())
                .collect::<Vec<_>>();
            candidates.choose(&mut rand::rng()).map(|user_lore| {
                (
                    user_lore.member.user_id,
                    user_lore.member.username.clone(),
                    user_lore.lore.clone(),
                )
            })
        };
        let Some((answer_id, answer_name, lore)) = pick else {
            let content = "Nobody has any lore yet! Add some with /add-user-context";
            return respond_ephemeral(&rpc, interaction, content.to_string()).await;
        };

        let round_id = self.trivia.next_round_id.fetch_add(1, Ordering::Relaxed);
        let started = match self.trivia.by_channel.lock().entry(channel_id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(Round {
                    id: round_id,
                    answer_id,
                    answer_name: answer_name.clone(),
                });
                true
            }
        };
        if !started {
            let content = "A lore trivia round is already running in this channel";
            return respond_ephemeral(&rpc, interaction, content.to_string()).await;
        }

        let clue = redact(&lore, &format!("<@{answer_id}>"), &answer_name);
        rpc.create_interaction_response(
            rpc::context::current(),
            interaction.id,
            interaction.token.clone(),
            serde_json::json!({
                "type": 4,
                "data": {
                    "content": format!(
                        "🔍 **Lore trivia!** Whose lore is this?\n> {clue}\n\
                        Reply with their name or mention them within {} seconds!",
                        ROUND_DURATION.as_secs()
                    ),
                    // Lore can mention people, don't ping anyone with the clue
                    "allowed_mentions": { "parse": [] }
                }
            }),
        )
        .await??;

        let by_channel = Arc::clone(&self.trivia.by_channel);
        tokio::spawn(async move {
            tokio::time::sleep(ROUND_DURATION).await;

            let expired_round = {
                let mut by_channel = by_channel.lock();
                let still_running = by_channel
                    .get(&channel_id)
                    .is_some_and(|round| round.id == round_id);
                if still_running {
                    by_channel.remove(&channel_id)
                } else {
                    None
                }
            };

            if let Some(round) = expired_round {
                let content = format!("⌛ Time's up! That was **{}**'s lore.", round.answer_name);
                let send_result = rpc
                    .send_message(rpc::context::current(), channel_id, content)
                    .await;
                if !matches!(send_result, Ok(Ok(_))) {
                    tracing::warn!("Failed to announce the end of a lore trivia round");
                }
            }
        });

        Ok(())
    }

    /// Checks whether `message` answers the trivia round running in its channel, if any
    pub async fn check_trivia_guess(
        &self,
        rpc: &rpc::ProtocolClient,
        message: &Message,
    ) -> anyhow::Result<()> {
        let solved_round = {
            let mut by_channel = self.trivia.by_channel.lock();
            let is_correct = by_channel.get(&message.channel_id).is_some_and(|round| {
                message.mentions_user_id(round.answer_id)
                    || message
                        .content
                        .trim()
                        .eq_ignore_ascii_case(&round.answer_name)
            });
            if is_correct {
                by_channel.remove(&message.channel_id)
            } else {
                None
            }
        };

        if let Some(round) = solved_round {
            let content = format!(
                "✅ <@{}> got it! That was **{}**'s lore.",
                message.author.id, round.answer_name
            );
            rpc.send_reply(
                rpc::context::current(),
                message.channel_id,
                content,
                message.id,
            )
            .await??;
        }

        Ok(())
    }
}

/// Hides the answer's mention and name from their lore. Very short names are left alone, as
/// redacting them would mangle unrelated words
fn redact(lore: &str, mention: &str, name: &str) -> String {
    const MIN_REDACTED_NAME_LEN: usize = 3;

    let lore = lore.replace(mention, REDACTED);
    if name.len() < MIN_REDACTED_NAME_LEN {
        return lore;
    }

    let mut redacted = String::with_capacity(lore.len());
    let mut rest = lore.as_str();
    while let Some(index) = find_ignore_ascii_case(rest, name) {
        redacted.push_str(&rest[..index]);
        redacted.push_str(REDACTED);
        rest = &rest[index + name.len()..];
    }
    redacted.push_str(rest);
    redacted
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .char_indices()
        .map(|(index, _)| index)
        .find(|&index| {
            haystack
                .get(index..index + needle.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_mentions_and_names_case_insensitively() {
        let redacted = redact(
            "Brian (<@42>) says BRIAN is the best, brian agrees",
            "<@42>",
            "Brian",
        );
        assert_eq!(redacted, "███ (███) says ███ is the best, ███ agrees");
    }

    #[test]
    fn leaves_short_names_alone() {
        assert_eq!(redact("Al is all in", "<@1>", "Al"), "Al is all in");
    }

    #[test]
    fn handles_non_ascii_lore() {
        assert_eq!(
            redact("Zoë loves crêpes", "<@1>", "Zoë"),
            "███ loves crêpes"
        );
    }
}
