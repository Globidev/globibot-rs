//! Joining a conversation without being pinged, in one channel (`LLM_CHIME_IN_CHANNEL_ID`).
//!
//! Free rules pick the candidate messages, a small model call on the last few messages decides,
//! and limits keep it rare: at most 3 an hour and 15 a day.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use globibot_core::serenity::all::ChannelId;
use itertools::Itertools;
use parking_lot::Mutex;

use crate::{
    fun::transcript_line,
    openrouter::{self, Message, Role},
    usage::UsageKind,
};

const MAX_PER_HOUR: usize = 3;
const MAX_PER_DAY: usize = 15;
const HOUR: Duration = Duration::from_secs(3_600);
const DAY: Duration = Duration::from_secs(86_400);
/// A message this soon after the bot spoke may be for it
const FOLLOW_UP_WITHIN: Duration = Duration::from_secs(5 * 60);
/// Messages the classifier sees, the candidate included
const CLASSIFIER_MESSAGES: usize = 15;

const CLASSIFIER_PROMPT: &str = "You decide whether Globibot, a bot in a Discord chat, should \
    join the conversation on its own, without being pinged. You get the latest messages, the \
    last one being the one to decide on.

    Answer YES when that last message invites the bot in: someone talks to it or about it \
    and would welcome an answer, asks for its take, follows up on what it just said, or asks the \
    room a question nobody is answering and the bot can help. Also answer YES when the bot has \
    something useful or funny to add that the people talking would likely enjoy. Answer NO when \
    people are talking among themselves privately, or when a reply would interrupt.

    Reply with YES or NO, then a short reason on the same line.";

/// Added to the message being answered: the model otherwise assumes it was pinged
pub const ANSWER_NOTE: &str =
    "[Not addressed to you directly: you're joining in on your own. Keep it short.]";

#[derive(Debug)]
pub struct ChimeIn {
    pub channel_id: ChannelId,
    /// Lowercase names the bot answers to
    pub bot_names: Vec<String>,
    /// When it chimed in, latest last. ponytail: not saved, a restart resets the limits.
    posts: Mutex<VecDeque<Instant>>,
    /// Answers being written in the channel. The context then still shows the ping as
    /// unanswered, so the classifier would say YES and the bot would answer it twice.
    answering: AtomicUsize,
}

/// Counts an answer in progress until dropped
pub struct Answering<'a>(&'a AtomicUsize);

impl Drop for Answering<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What the rules look at
pub struct Candidate<'a> {
    pub text: &'a str,
    /// Lowercase names the bot answers to
    pub bot_names: &'a [String],
    pub replies_to_bot: bool,
    pub since_bot_spoke: Option<Duration>,
}

impl ChimeIn {
    pub fn new(channel_id: ChannelId, bot_names: Vec<String>) -> Self {
        Self {
            channel_id,
            bot_names,
            posts: Mutex::default(),
            answering: AtomicUsize::new(0),
        }
    }

    /// Marks an answer in the channel as in progress, until the result is dropped
    pub fn start_answer(&self) -> Answering<'_> {
        self.answering.fetch_add(1, Ordering::Relaxed);
        Answering(&self.answering)
    }

    pub fn is_answering(&self) -> bool {
        self.answering.load(Ordering::Relaxed) > 0
    }

    /// Whether the limits leave room for one more
    pub fn has_room(&self, now: Instant) -> bool {
        let mut posts = self.posts.lock();
        while posts
            .front()
            .is_some_and(|at| now.duration_since(*at) > DAY)
        {
            posts.pop_front();
        }
        let last_hour = posts
            .iter()
            .filter(|at| now.duration_since(**at) <= HOUR)
            .count();
        posts.len() < MAX_PER_DAY && last_hour < MAX_PER_HOUR
    }

    /// Takes a slot if the limits leave one
    pub fn try_take(&self, now: Instant) -> bool {
        let room = self.has_room(now);
        if room {
            self.posts.lock().push_back(now);
        }
        room
    }
}

/// The free rules: the bot is named, replied to or just spoke, or it's a question
pub fn is_candidate(candidate: &Candidate) -> bool {
    let lowercase = candidate.text.to_lowercase();
    let names_bot = lowercase
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "bot" || candidate.bot_names.iter().any(|name| name == word));

    candidate.replies_to_bot
        || candidate
            .since_bot_spoke
            .is_some_and(|since| since <= FOLLOW_UP_WITHIN)
        || names_bot
        || candidate.text.contains('?')
}

/// Asks the model whether to chime in on `candidate`, given the `context` before it. Also
/// returns its reason.
pub async fn should_chime_in(
    llm_client: &openrouter::Client,
    model: &str,
    channel_id: ChannelId,
    context: &[Message],
    candidate: &Message,
) -> anyhow::Result<(bool, String)> {
    let transcript = context
        .iter()
        .skip(context.len().saturating_sub(CLASSIFIER_MESSAGES - 1))
        .chain([candidate])
        .map(transcript_line)
        .join("\n");
    let verdict = llm_client
        .complete(
            UsageKind::ChimeInCheck,
            model,
            format!("chime-in-{channel_id}"),
            CLASSIFIER_PROMPT.to_string(),
            [Message::text(Role::User, transcript)],
        )
        .await?;
    let verdict = verdict.trim().to_string();
    Ok((verdict.to_uppercase().starts_with("YES"), verdict))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(text: &str) -> Candidate<'_> {
        static NAMES: std::sync::LazyLock<Vec<String>> =
            std::sync::LazyLock::new(|| vec!["globibot".to_string()]);
        Candidate {
            text,
            bot_names: &NAMES,
            replies_to_bot: false,
            since_bot_spoke: None,
        }
    }

    #[test]
    fn rules_pick_candidates() {
        assert!(!is_candidate(&candidate("lol same")));
        assert!(is_candidate(&candidate("what does Globibot think")));
        assert!(is_candidate(&candidate("the bot is wrong again")));
        assert!(is_candidate(&candidate("anyone up for a game?")));
        // Words, not substrings
        assert!(!is_candidate(&candidate("robots are cool")));

        let reply = Candidate {
            replies_to_bot: true,
            ..candidate("lol")
        };
        assert!(is_candidate(&reply));

        let soon = Candidate {
            since_bot_spoke: Some(Duration::from_secs(60)),
            ..candidate("lol")
        };
        assert!(is_candidate(&soon));
        let later = Candidate {
            since_bot_spoke: Some(Duration::from_secs(600)),
            ..candidate("lol")
        };
        assert!(!is_candidate(&later));
    }

    #[test]
    fn limits_per_hour_and_day() {
        let chime_in = ChimeIn::new(ChannelId::new(1), Vec::new());
        let start = Instant::now();
        for _ in 0..MAX_PER_HOUR {
            assert!(chime_in.try_take(start));
        }
        assert!(!chime_in.try_take(start + Duration::from_secs(60)));

        // Each hour allows 3 more, until the day's 15
        let mut taken = MAX_PER_HOUR;
        let mut at = start;
        while taken < MAX_PER_DAY {
            at += HOUR + Duration::from_secs(1);
            for _ in 0..MAX_PER_HOUR {
                assert!(chime_in.try_take(at));
                taken += 1;
            }
        }
        assert!(!chime_in.try_take(at + HOUR + Duration::from_secs(1)));
        // A day after the first ones, there's room again
        assert!(chime_in.has_room(start + DAY + HOUR + Duration::from_secs(2)));
    }

    #[test]
    fn answering_until_dropped() {
        let chime_in = ChimeIn::new(ChannelId::new(1), Vec::new());
        let first = chime_in.start_answer();
        let second = chime_in.start_answer();
        drop(first);
        assert!(chime_in.is_answering());
        drop(second);
        assert!(!chime_in.is_answering());
    }
}
