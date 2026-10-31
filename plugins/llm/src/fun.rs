use globibot_core::{
    rpc,
    serenity::all::{CommandInteraction, Message, UserId},
};
use itertools::Itertools;

use crate::{
    LlmPlugin,
    cooldown::cooldown_message,
    openrouter::{ContentPart, Message as LlmMessage, Role, TextContentPart},
    respond_ephemeral,
};

const LOST_TRAIN_OF_THOUGHT: &str = "Sorry, I lost my train of thought.";

#[derive(Debug, Clone, Copy)]
pub enum Vibe {
    Roast,
    Hype,
}

impl LlmPlugin {
    /// One-off completion with the current personality and lore, outside of any chat context
    pub async fn complete_one_shot(&self, prompt: String) -> anyhow::Result<String> {
        let message = LlmMessage {
            role: Role::User,
            content: vec![ContentPart::Text(TextContentPart {
                kind: "text",
                text: prompt,
            })],
        };

        // Build the request before awaiting so the model's lock isn't held across the await
        let completion = self.llm_client.complete(
            &self.llm_model.read(),
            None,
            self.system_prompt(),
            [message],
        );
        let answer = completion.await?;

        Ok(sanitize_llm_output(&answer))
    }

    /// `/roast` and `/hype`
    pub async fn vibe_check(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        target_id: UserId,
        vibe: Vibe,
    ) -> anyhow::Result<()> {
        if let Err(remaining) = self.cooldowns.try_start(interaction.user.id) {
            return respond_ephemeral(&rpc, interaction, cooldown_message(remaining)).await;
        }

        defer_response(&rpc, interaction, false).await?;

        let target_name = resolved_display_name(interaction, target_id);
        let requester_id = interaction.user.id;
        let prompt = match vibe {
            Vibe::Roast => format!(
                "<@{requester_id}> asked you to roast {target_name} (<@{target_id}>). \
                Write a short, playful roast of them: 2 to 4 sentences, PG-13, teasing but never \
                actually hurtful. Mention them once using <@{target_id}>. If the lore says \
                something about them, use it."
            ),
            Vibe::Hype => format!(
                "<@{requester_id}> asked you to hype up {target_name} (<@{target_id}>). \
                Write a short, over-the-top hype message that makes them feel like the main \
                character: 2 to 4 sentences. Mention them once using <@{target_id}>. If the lore \
                says something about them, use it."
            ),
        };

        let answer = self.complete_one_shot(prompt).await.unwrap_or_else(|err| {
            tracing::error!("Failed to get LLM completion for {vibe:?}: {err:?}");
            LOST_TRAIN_OF_THOUGHT.to_string()
        });

        edit_deferred_response(&rpc, interaction, answer).await
    }

    /// `/tldr`: summarizes recent messages from the channel's chat context, only for the caller
    pub async fn tldr(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        messages: Option<i64>,
    ) -> anyhow::Result<()> {
        const DEFAULT_MESSAGE_COUNT: usize = 50;
        const MAX_MESSAGE_COUNT: usize = 200;

        let message_count = messages.map_or(DEFAULT_MESSAGE_COUNT, |count| {
            count.clamp(1, MAX_MESSAGE_COUNT as i64) as usize
        });

        let transcript = {
            let contexts_by_channel = self.contexts_by_channel.lock();
            contexts_by_channel
                .get(&interaction.channel_id)
                .map(|context| {
                    let skip = context.messages.len().saturating_sub(message_count);
                    context
                        .messages
                        .iter()
                        .skip(skip)
                        .map(transcript_line)
                        .join("\n")
                })
                .unwrap_or_default()
        };

        if transcript.trim().is_empty() {
            let content = "I haven't seen any messages in this channel since I last started";
            return respond_ephemeral(&rpc, interaction, content.to_string()).await;
        }

        if let Err(remaining) = self.cooldowns.try_start(interaction.user.id) {
            return respond_ephemeral(&rpc, interaction, cooldown_message(remaining)).await;
        }

        defer_response(&rpc, interaction, true).await?;

        let prompt = format!(
            "Summarize these recent messages from this channel for someone who is catching up. \
            Use a few short bullet points and say who said what when it matters. Stay in \
            character, but clarity comes first.\n\n{transcript}"
        );
        let answer = self.complete_one_shot(prompt).await.unwrap_or_else(|err| {
            tracing::error!("Failed to get LLM completion for /tldr: {err:?}");
            LOST_TRAIN_OF_THOUGHT.to_string()
        });

        edit_deferred_response(&rpc, interaction, answer).await
    }

    /// Replies in character to a new post in the Starboard channel
    pub async fn comment_on_starboard(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
    ) -> anyhow::Result<()> {
        let post = starboard_post_text(message);
        if post.is_empty() {
            return Ok(());
        }

        let prompt = format!(
            "This post just made it onto the server's Starboard, where messages that got enough \
            star reactions are shared:\n\n{post}\n\nReact to it in character in one or two short \
            sentences."
        );
        let answer = self.complete_one_shot(prompt).await?;

        rpc.send_reply(
            rpc::context::current(),
            message.channel_id,
            answer,
            message.id,
        )
        .await??;
        Ok(())
    }
}

/// Makes LLM output safe to post: no mass mentions or role pings, and within Discord's message
/// length limit
pub fn sanitize_llm_output(text: &str) -> String {
    // Discord's limit is 2000 characters, keep some margin for how it counts them
    const MAX_CHARS: usize = 1_990;

    let text = text
        .replace("@everyone", "@\u{200B}everyone")
        .replace("@here", "@\u{200B}here")
        .replace("<@&", "<@\u{200B}&");

    if text.chars().count() <= MAX_CHARS {
        return text;
    }

    let mut truncated = text.chars().take(MAX_CHARS - 1).collect::<String>();
    truncated.push('…');
    truncated
}

/// Acknowledges an interaction with a "thinking…" state, to be completed with
/// [`edit_deferred_response`]. Needed for anything slower than Discord's 3 second response limit
async fn defer_response(
    rpc: &rpc::ProtocolClient,
    interaction: &CommandInteraction,
    ephemeral: bool,
) -> anyhow::Result<()> {
    let flags = if ephemeral { 64 } else { 0 };
    rpc.create_interaction_response(
        rpc::context::current(),
        interaction.id,
        interaction.token.clone(),
        serde_json::json!({
            "type": 5,
            "data": { "flags": flags }
        }),
    )
    .await??;
    Ok(())
}

async fn edit_deferred_response(
    rpc: &rpc::ProtocolClient,
    interaction: &CommandInteraction,
    content: String,
) -> anyhow::Result<()> {
    rpc.edit_interaction_response(
        rpc::context::current(),
        interaction.token.clone(),
        serde_json::json!({
            "content": content,
            // Ping users the answer mentions, never roles or @everyone
            "allowed_mentions": { "parse": ["users"] }
        }),
    )
    .await??;
    Ok(())
}

fn resolved_display_name(interaction: &CommandInteraction, user_id: UserId) -> String {
    let resolved = &interaction.data.resolved;
    resolved
        .members
        .get(&user_id)
        .and_then(|member| member.nick.clone())
        .or_else(|| {
            resolved
                .users
                .get(&user_id)
                .map(|user| user.display_name().to_string())
        })
        .unwrap_or_else(|| format!("<@{user_id}>"))
}

fn transcript_line(message: &LlmMessage) -> String {
    let text = message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text_part) => Some(text_part.text.as_str()),
            ContentPart::Image(_) => None,
        })
        .join(" ");

    // User messages are already formatted as `name (<@id>): content`
    match message.role {
        Role::Assistant => format!("Globibot: {text}"),
        Role::User | Role::System => text,
    }
}

/// The text of a Starboard post: the Starboard bot usually puts the original message in an embed
fn starboard_post_text(message: &Message) -> String {
    let mut parts = Vec::new();

    let content = message.content.trim();
    if !content.is_empty() {
        parts.push(content.to_string());
    }

    for embed in &message.embeds {
        if let Some(author) = &embed.author {
            parts.push(format!("Posted by {}", author.name));
        }
        if let Some(description) = &embed.description
            && !description.trim().is_empty()
        {
            parts.push(description.trim().to_string());
        }
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_defuses_mass_and_role_mentions() {
        let sanitized = sanitize_llm_output("hi @everyone and @here and <@&123> but <@456>");
        assert!(!sanitized.contains("@everyone"));
        assert!(!sanitized.contains("@here"));
        assert!(!sanitized.contains("<@&"));
        assert!(sanitized.contains("<@456>"));
    }

    #[test]
    fn sanitize_truncates_long_output() {
        let sanitized = sanitize_llm_output(&"a".repeat(5_000));
        assert_eq!(sanitized.chars().count(), 1_990);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn sanitize_keeps_short_output() {
        assert_eq!(sanitize_llm_output("hello"), "hello");
    }
}
