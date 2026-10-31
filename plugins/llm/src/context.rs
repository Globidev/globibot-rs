//! Per-channel chat context: recent messages, plus a rolling summary of older ones.
//!
//! Messages are dropped in chunks rather than one by one, so the start of the prompt stays the
//! same between drops and the provider's prompt cache keeps hitting. Each dropped chunk is merged
//! into the summary, which is sent ahead of the messages.

use std::collections::VecDeque;

use globibot_core::{
    serenity::all::Channel,
    storage::{self, StorageValue},
};
use itertools::Itertools;

use crate::{
    fun::transcript_line,
    openrouter::{self, Message, Role},
    usage::UsageKind,
};

/// Share of the window kept after a drop
const KEEP_AFTER_DROP_PERCENT: usize = 70;

/// Hard limit on the summary, in case the model ignores the length asked for
const MAX_SUMMARY_CHARS: usize = 6_000;

pub const SUMMARY_TAG: &str = "[Summary of earlier messages in this channel]";

#[derive(Debug)]
pub struct ChannelContext {
    pub channel: Channel,
    pub summary: Option<String>,
    pub messages: VecDeque<Message>,
}

impl ChannelContext {
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            summary: None,
            messages: VecDeque::new(),
        }
    }

    /// Adds a message. Past `max_size` messages, drops the oldest ones down to 70% of it and
    /// returns them, to be merged into the summary.
    pub fn push(&mut self, message: Message, max_size: usize) -> Option<Vec<Message>> {
        self.messages.push_back(message);
        if self.messages.len() <= max_size {
            return None;
        }

        let keep = max_size * KEEP_AFTER_DROP_PERCENT / 100;
        let drop_count = self.messages.len() - keep;
        Some(self.messages.drain(..drop_count).collect())
    }

    /// What's sent to the model: the summary first, then the messages.
    pub fn prompt_messages(&self) -> Vec<Message> {
        let summary = self
            .summary
            .as_ref()
            .map(|summary| Message::text(Role::User, format!("{SUMMARY_TAG}\n{summary}")));
        summary
            .into_iter()
            .chain(self.messages.iter().cloned())
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
            channel,
            summary: saved.summary,
            messages: saved.messages,
        }
    }
}

/// A channel's context in Redis, keyed by channel ID. The channel itself is fetched again on load.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct SavedContext {
    summary: Option<String>,
    messages: VecDeque<Message>,
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

    fn context_with(count: usize) -> (ChannelContext, Option<Vec<Message>>) {
        let channel = serde_json::from_value(serde_json::json!({
            "id": "1", "type": 0, "guild_id": "2", "name": "general"
        }))
        .unwrap();
        let mut context = ChannelContext::new(channel);
        let mut dropped = None;
        for i in 0..count {
            dropped = context.push(Message::text(Role::User, i.to_string()), 10);
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
        assert_eq!(text_of(&context.messages[0]), "4");
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
}
