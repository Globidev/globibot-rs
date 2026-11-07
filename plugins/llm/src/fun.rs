use globibot_core::{
    interaction::CommandCtx,
    rpc,
    serenity::all::{
        CommandInteraction, CreateAllowedMentions, EditInteractionResponse, Message, UserId,
    },
};
use itertools::Itertools;

use crate::{
    LlmPlugin,
    cooldown::cooldown_message,
    images::{self, MessageImages},
    openrouter::{ContentPart, Message as LlmMessage, Role},
    usage::UsageKind,
};

const LOST_TRAIN_OF_THOUGHT: &str = "Sorry, I lost my train of thought.";

#[derive(Debug, Clone, Copy)]
pub enum Vibe {
    Roast,
    Hype,
}

impl LlmPlugin {
    /// One-off completion with the current personality and lore, outside of any chat context.
    /// `images` must have their author's consent.
    pub async fn complete_one_shot(
        &self,
        kind: UsageKind,
        prompt: String,
        images: Vec<ContentPart>,
    ) -> anyhow::Result<String> {
        let mut message = LlmMessage::text(Role::User, prompt);
        message.content.extend(images);

        // Build the request before awaiting so the model's lock isn't held across the await
        let completion = self.llm_client.complete(
            kind,
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
        ctx: &CommandCtx<'_>,
        target_id: UserId,
        vibe: Vibe,
    ) -> anyhow::Result<()> {
        if let Err(remaining) = self.cooldowns.try_start(ctx.interaction.user.id) {
            return Ok(ctx.reply_ephemeral(cooldown_message(remaining)).await?);
        }

        ctx.defer().await?;

        let target_name = resolved_display_name(ctx.interaction, target_id);
        let requester_id = ctx.interaction.user.id;
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

        let answer = self
            .complete_one_shot(UsageKind::Command, prompt, vec![])
            .await
            .unwrap_or_else(|err| {
                tracing::error!("Failed to get LLM completion for {vibe:?}: {err:?}");
                LOST_TRAIN_OF_THOUGHT.to_string()
            });

        ctx.edit_with(answer_edit(answer)).await?;
        Ok(())
    }

    /// `/tldr`: summarizes recent messages from the channel's chat context, only for the caller
    pub async fn tldr(&self, ctx: &CommandCtx<'_>, messages: Option<i64>) -> anyhow::Result<()> {
        const DEFAULT_MESSAGE_COUNT: usize = 50;
        const MAX_MESSAGE_COUNT: usize = 200;

        let message_count = messages.map_or(DEFAULT_MESSAGE_COUNT, |count| {
            count.clamp(1, MAX_MESSAGE_COUNT as i64) as usize
        });

        let transcript = {
            let contexts_by_channel = self.contexts_by_channel.lock();
            contexts_by_channel
                .get(&ctx.interaction.channel_id)
                .map(|context| {
                    let skip = context.messages.len().saturating_sub(message_count);
                    context
                        .messages
                        .iter()
                        .skip(skip)
                        .map(|context_message| transcript_line(&context_message.message))
                        .join("\n")
                })
                .unwrap_or_default()
        };

        if transcript.trim().is_empty() {
            let content = "I haven't seen any messages in this channel since I last started";
            return Ok(ctx.reply_ephemeral(content.to_string()).await?);
        }

        if let Err(remaining) = self.cooldowns.try_start(ctx.interaction.user.id) {
            return Ok(ctx.reply_ephemeral(cooldown_message(remaining)).await?);
        }

        ctx.defer_ephemeral().await?;

        let prompt = format!(
            "Summarize these recent messages from this channel for someone who is catching up. \
            Use a few short bullet points and say who said what when it matters. Stay in \
            character, but clarity comes first.\n\n{transcript}"
        );
        let answer = self
            .complete_one_shot(UsageKind::Command, prompt, vec![])
            .await
            .unwrap_or_else(|err| {
                tracing::error!("Failed to get LLM completion for /tldr: {err:?}");
                LOST_TRAIN_OF_THOUGHT.to_string()
            });

        ctx.edit_with(answer_edit(answer)).await?;
        Ok(())
    }

    /// Replies in character to a new post in the Starboard channel
    pub async fn comment_on_starboard(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
    ) -> anyhow::Result<()> {
        let mut post = starboard_post_text(message);
        let images = MessageImages::with_embeds(message);
        if post.is_empty() && images.urls.is_empty() {
            return Ok(());
        }

        // The images are the starred message's author's: they need their consent, and only the
        // role can give it here
        let mut shown_images = Vec::new();
        if !images.urls.is_empty() {
            let author = starboard_author_id(message);
            let has_role = match author {
                Some(author) => self.has_images_role(&rpc, author).await,
                None => false,
            };
            let consent = author.and_then(|author| images::consent(false, false, author, has_role));
            if consent.is_some() {
                shown_images = images::fetch_all(self.llm_client.http(), &images.urls).await;
            }
            if shown_images.is_empty() {
                post = format!("{post}\n{}", images::NOT_SHARED).trim().to_string();
            }
        }

        let prompt = format!(
            "This post just made it onto the server's Starboard, where messages that got enough \
            star reactions are shared:\n\n{post}\n\nReact to it in character in one or two short \
            sentences."
        );
        let answer = self
            .complete_one_shot(UsageKind::Starboard, prompt, shown_images)
            .await?;

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

/// Discord's limit is 2000 characters, with some margin for how it counts them
pub const MAX_MESSAGE_CHARS: usize = 1_990;

/// Makes LLM output safe to post: no mass mentions or role pings, and within Discord's message
/// length limit
pub fn sanitize_llm_output(text: &str) -> String {
    const MAX_CHARS: usize = MAX_MESSAGE_CHARS;

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

/// An LLM answer for a deferred interaction: pings users it mentions, never roles or @everyone
fn answer_edit(answer: String) -> EditInteractionResponse {
    EditInteractionResponse::new()
        .content(answer)
        .allowed_mentions(CreateAllowedMentions::new().all_users(true))
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

pub fn transcript_line(message: &LlmMessage) -> String {
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
        Role::User | Role::System | Role::Tool => text,
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

/// The author of the starred message. Starboard bots don't give their id, but use their avatar
/// as the embed's author icon: `/avatars/<id>/...`, or `/guilds/<guild>/users/<id>/avatars/...`
/// for a server avatar. `None` for a default avatar.
fn starboard_author_id(message: &Message) -> Option<UserId> {
    message.embeds.iter().find_map(|embed| {
        let icon_url = embed.author.as_ref()?.icon_url.as_deref()?;
        let url = reqwest::Url::parse(icon_url).ok()?;
        if url.host_str()? != "cdn.discordapp.com" {
            return None;
        }
        let segments = url.path_segments()?.collect::<Vec<_>>();
        let id = match segments.as_slice() {
            ["avatars", id, _] | ["guilds", _, "users", id, "avatars", _] => *id,
            _ => return None,
        };
        id.parse().ok().map(UserId::new)
    })
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
    fn reads_starboard_author_and_images() {
        let post = |icon_url: &str| -> Message {
            serde_json::from_value(serde_json::json!({
                "id": "1", "channel_id": "2", "type": 0, "content": "⭐ 5", "timestamp": "2026-11-02T00:00:00Z",
                "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
                "pinned": false, "attachments": [],
                "author": { "id": "3", "username": "starboard", "discriminator": "0", "avatar": null },
                "embeds": [{
                    "author": { "name": "globi", "icon_url": icon_url },
                    "description": "look",
                    "image": {
                        "url": "https://cdn.discordapp.com/attachments/4/5/cat.png",
                        "proxy_url": "https://media.discordapp.net/attachments/4/5/cat.png",
                        "width": 2048, "height": 1024
                    }
                }]
            }))
            .unwrap()
        };

        let message = post("https://cdn.discordapp.com/avatars/42/abc.png");
        assert_eq!(starboard_author_id(&message), Some(UserId::new(42)));
        assert_eq!(
            MessageImages::with_embeds(&message).urls,
            [
                "https://media.discordapp.net/attachments/4/5/cat.png?width=1024&height=512&format=webp"
            ]
        );
        assert!(MessageImages::of(&message).urls.is_empty());

        let server_avatar = post("https://cdn.discordapp.com/guilds/7/users/43/avatars/abc.png");
        assert_eq!(starboard_author_id(&server_avatar), Some(UserId::new(43)));
        let default_avatar = post("https://cdn.discordapp.com/embed/avatars/0.png");
        assert_eq!(starboard_author_id(&default_avatar), None);
    }

    #[test]
    fn sanitize_truncates_long_output() {
        let sanitized = sanitize_llm_output(&"a".repeat(5_000));
        assert_eq!(sanitized.chars().count(), 1_990);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn answer_edit_only_pings_users() {
        assert_eq!(
            serde_json::to_value(answer_edit("hi".to_string())).unwrap(),
            serde_json::json!({
                "content": "hi",
                // Serenity adds the empty lists, as in its own requests
                "allowed_mentions": { "parse": ["users"], "users": [], "roles": [] }
            })
        );
    }

    #[test]
    fn sanitize_keeps_short_output() {
        assert_eq!(sanitize_llm_output("hello"), "hello");
    }
}
