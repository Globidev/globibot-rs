//! Per-channel chat context: recent messages, plus a rolling summary of older ones.
//!
//! Messages are dropped in chunks rather than one by one, so the start of the prompt stays the
//! same between drops and the provider's prompt cache keeps hitting. Each dropped chunk is merged
//! into the summary, which is sent ahead of the messages.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use globibot_core::{
    serenity::all::{Channel, MessageId},
    storage::{self, StorageValue},
};
use itertools::Itertools;

use crate::{
    fun::transcript_line,
    openrouter::{self, ContentPart, Message, Role, TextContentPart},
    usage::UsageKind,
};

/// Share of the window kept after a drop
const KEEP_AFTER_DROP_PERCENT: usize = 70;

/// Hard limit on the summary, in case the model ignores the length asked for
const MAX_SUMMARY_CHARS: usize = 6_000;

pub const SUMMARY_TAG: &str = "[Summary of earlier messages in this channel]";

/// How far back a ping can look for an image when it has none of its own
const RECENT_IMAGE_MAX_MESSAGES: u64 = 10;
const RECENT_IMAGE_MAX_AGE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextMessage {
    /// The Discord message, so its image captions can be added once ready. `None` for the
    /// bot's answers, and for messages saved before IDs were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<MessageId>,
    #[serde(flatten)]
    pub message: Message,
}

/// The latest images posted in a channel. Not saved: their links expire anyway.
#[derive(Debug, Clone)]
pub struct PostedImages {
    pub author_name: String,
    pub urls: Vec<String>,
    posted_at: Instant,
    /// The channel's message count when they were posted
    position: u64,
}

#[derive(Debug)]
pub struct ChannelContext {
    pub channel: Channel,
    pub summary: Option<String>,
    pub messages: VecDeque<ContextMessage>,
    /// Messages pushed since startup
    pushed: u64,
    latest_images: Option<PostedImages>,
}

impl ChannelContext {
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            summary: None,
            messages: VecDeque::new(),
            pushed: 0,
            latest_images: None,
        }
    }

    /// Adds a message. Past `max_size` messages, drops the oldest ones down to 70% of it and
    /// returns them, to be merged into the summary.
    pub fn push(&mut self, message: ContextMessage, max_size: usize) -> Option<Vec<Message>> {
        self.messages.push_back(message);
        self.pushed += 1;
        if self.messages.len() <= max_size {
            return None;
        }

        let keep = max_size * KEEP_AFTER_DROP_PERCENT / 100;
        let drop_count = self.messages.len() - keep;
        Some(
            self.messages
                .drain(..drop_count)
                .map(|context_message| context_message.message)
                .collect(),
        )
    }

    /// Replaces the `placeholder` text of a message, e.g. with its images' caption. False if the
    /// message or placeholder is gone.
    pub fn replace_text(&mut self, id: MessageId, placeholder: &str, text: String) -> bool {
        let part = self
            .messages
            .iter_mut()
            .rev()
            .filter(|context_message| context_message.id == Some(id))
            .flat_map(|context_message| context_message.message.content.iter_mut())
            .find(|part| matches!(part, ContentPart::Text(part) if part.text == placeholder));
        match part {
            Some(part) => {
                *part = ContentPart::Text(TextContentPart { text });
                true
            }
            None => false,
        }
    }

    /// Remembers images just posted. Call after pushing their message.
    pub fn set_latest_images(&mut self, author_name: String, urls: Vec<String>) {
        self.latest_images = Some(PostedImages {
            author_name,
            urls,
            posted_at: Instant::now(),
            position: self.pushed,
        });
    }

    /// The latest images, if posted at most 10 messages and 5 minutes ago
    pub fn recent_images(&self, now: Instant) -> Option<&PostedImages> {
        self.latest_images.as_ref().filter(|images| {
            self.pushed - images.position < RECENT_IMAGE_MAX_MESSAGES
                && now.duration_since(images.posted_at) <= RECENT_IMAGE_MAX_AGE
        })
    }

    /// What's sent to the model: the summary first, then the messages.
    pub fn prompt_messages(&self) -> Vec<Message> {
        let summary = self
            .summary
            .as_ref()
            .map(|summary| Message::text(Role::User, format!("{SUMMARY_TAG}\n{summary}")));
        summary
            .into_iter()
            .chain(
                self.messages
                    .iter()
                    .map(|context_message| context_message.message.clone()),
            )
            .collect()
    }

    pub fn to_saved(&self) -> SavedContext {
        SavedContext {
            summary: self.summary.clone(),
            messages: self.messages.clone(),
        }
    }

    pub fn from_saved(channel: Channel, saved: SavedContext) -> Self {
        Self {
            summary: saved.summary,
            messages: saved.messages,
            ..Self::new(channel)
        }
    }
}

/// A channel's context in Redis, keyed by channel ID. The channel itself is fetched again on load.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedContext {
    summary: Option<String>,
    messages: VecDeque<ContextMessage>,
}

impl StorageValue for SavedContext {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_context";
}

/// Merges `dropped` messages into the `previous` summary. Neutral on purpose, not in character:
/// summaries are merged again and again, a personality would compound.
pub async fn summarize(
    client: &openrouter::Client,
    model: &str,
    previous: Option<String>,
    dropped: &[Message],
) -> anyhow::Result<String> {
    const SYSTEM_PROMPT: &str = "You maintain a running summary of a Discord channel for a chat \
        bot named Globibot, so it remembers older conversation. You get the current summary (if \
        any) and the messages that came after it. Write the updated summary: who said what when \
        it matters, ongoing topics, running jokes, facts people shared about themselves, and what \
        Globibot said or promised. Older events can be condensed, recent ones keep more detail. \
        Keep users' names and their <@id> mentions as given. Plain text, at most 600 words, no \
        preamble.";

    let transcript = dropped.iter().map(transcript_line).join("\n");
    let prompt = format!(
        "Current summary:\n{}\n\nMessages after it:\n{transcript}",
        previous.as_deref().unwrap_or("(none yet)")
    );

    let summary = client
        .complete(
            UsageKind::Summary,
            model,
            None,
            SYSTEM_PROMPT.to_string(),
            [Message::text(Role::User, prompt)],
        )
        .await?;
    let summary = summary.trim();
    anyhow::ensure!(!summary.is_empty(), "The model returned an empty summary");

    Ok(clamp_chars(summary, MAX_SUMMARY_CHARS))
}

fn clamp_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut clamped = text.chars().take(max_chars - 1).collect::<String>();
    clamped.push('…');
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrouter::ContentPart;

    fn text_of(message: &Message) -> &str {
        match &message.content[0] {
            ContentPart::Text(part) => &part.text,
            ContentPart::Image(_) => panic!("expected text"),
        }
    }

    fn user_message(id: u64, text: &str) -> ContextMessage {
        ContextMessage {
            id: Some(MessageId::new(id + 1)),
            message: Message::text(Role::User, text.to_string()),
        }
    }

    fn context_with(count: usize) -> (ChannelContext, Option<Vec<Message>>) {
        let channel = serde_json::from_value(serde_json::json!({
            "id": "1", "type": 0, "guild_id": "2", "name": "general"
        }))
        .unwrap();
        let mut context = ChannelContext::new(channel);
        let mut dropped = None;
        for i in 0..count {
            dropped = context.push(user_message(i as u64, &i.to_string()), 10);
        }
        (context, dropped)
    }

    #[test]
    fn drops_in_chunks() {
        let (context, dropped) = context_with(10);
        assert!(dropped.is_none());
        assert_eq!(context.messages.len(), 10);

        // The 11th message drops the 4 oldest, keeping 7
        let (context, dropped) = context_with(11);
        let dropped = dropped.unwrap();
        assert_eq!(
            dropped.iter().map(text_of).collect_vec(),
            ["0", "1", "2", "3"]
        );
        assert_eq!(text_of(&context.messages[0].message), "4");
        assert_eq!(context.messages.len(), 7);

        // Then nothing is dropped until the window is full again
        let (context, dropped) = context_with(14);
        assert!(dropped.is_none());
        assert_eq!(context.messages.len(), 10);
    }

    #[test]
    fn summary_comes_first() {
        let (mut context, _) = context_with(2);
        assert_eq!(context.prompt_messages().len(), 2);

        context.summary = Some("they talked".to_string());
        let messages = context.prompt_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(text_of(&messages[0]), format!("{SUMMARY_TAG}\nthey talked"));
        assert_eq!(text_of(&messages[1]), "0");
    }

    #[test]
    fn clamps_long_summaries() {
        assert_eq!(clamp_chars("short", 10), "short");
        assert_eq!(clamp_chars("abcdefghijkl", 5), "abcd…");
    }

    #[test]
    fn replaces_placeholders_by_message_id() {
        let (mut context, _) = context_with(3);
        context.messages[1]
            .message
            .content
            .push(ContentPart::Text(TextContentPart {
                text: "[Image]".to_string(),
            }));

        assert!(!context.replace_text(MessageId::new(1), "[Image]", "x".to_string()));
        assert!(context.replace_text(MessageId::new(2), "[Image]", "[Image: a cat]".to_string()));
        assert_eq!(
            transcript_line(&context.messages[1].message),
            "1 [Image: a cat]"
        );
        // Already replaced
        assert!(!context.replace_text(MessageId::new(2), "[Image]", "x".to_string()));
    }

    #[test]
    fn recent_images_expire_by_messages_and_time() {
        let (mut context, _) = context_with(1);
        context.set_latest_images("globi".to_string(), vec!["url".to_string()]);
        let now = Instant::now();
        assert!(context.recent_images(now).is_some());

        // 9 more messages: still recent, the 10th after it is too far
        for i in 0..9 {
            context.push(user_message(100 + i, "later"), 1000);
        }
        assert!(context.recent_images(now).is_some());
        context.push(user_message(200, "later"), 1000);
        assert!(context.recent_images(now).is_none());

        context.set_latest_images("globi".to_string(), vec!["url".to_string()]);
        assert!(
            context
                .recent_images(now + Duration::from_secs(4 * 60))
                .is_some()
        );
        assert!(
            context
                .recent_images(now + Duration::from_secs(6 * 60))
                .is_none()
        );
    }

    #[test]
    fn loads_contexts_saved_without_message_ids() {
        let saved = serde_json::from_value::<SavedContext>(serde_json::json!({
            "summary": null,
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }]
        }))
        .unwrap();
        assert!(saved.messages[0].id.is_none());
        assert_eq!(text_of(&saved.messages[0].message), "hi");

        // And IDs round-trip
        let json = serde_json::to_value(ContextMessage {
            id: Some(MessageId::new(5)),
            message: Message::text(Role::User, "hi".to_string()),
        })
        .unwrap();
        assert_eq!(json["id"], "5");
        assert_eq!(json["role"], "user");
    }
}
