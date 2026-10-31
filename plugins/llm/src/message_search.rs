//! The `search_messages` tool: Discord's message search, through the bot

use std::fmt::Write;

use globibot_core::{
    rpc::{self, MessageSearch, MessageSearchResults},
    serenity::all::{ChannelId, GuildId, Message, MessageId, UserId},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::context::clamp_chars;

const DEFAULT_LIMIT: u8 = 10;
const MAX_LIMIT: u8 = 25;
/// Discord's own limit
const MAX_OFFSET: u32 = 9_975;
const MAX_CONTENT_CHARS: usize = 200;

pub fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "search_messages",
            "description": "Search the server's messages, like Discord's search bar. Gives how many messages match in \
                total, so it can count things (e.g. how many times someone said a word: use limit 1), and the \
                matching messages with a link to each. Only searches channels the person asking can read, and \
                when asked in a channel, only channels everyone can read.",
            "parameters": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "Words to look for" },
                    "author_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Only messages from these users, as Discord user ids"
                    },
                    "mentions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Only messages that mention these users, as Discord user ids"
                    },
                    "channel_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Only in these channels, as Discord channel ids"
                    },
                    "has": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["image", "sound", "video", "file", "sticker", "embed", "link", "poll"]
                        },
                        "description": "Only messages with these"
                    },
                    "after": { "type": "string", "description": "Only messages from this UTC day on, as YYYY-MM-DD" },
                    "before": { "type": "string", "description": "Only messages before this UTC day, as YYYY-MM-DD" },
                    "oldest_first": { "type": "boolean", "description": "Default: newest first" },
                    "limit": {
                        "type": "integer",
                        "description": format!("Messages to show. Default {DEFAULT_LIMIT}, at most {MAX_LIMIT}")
                    },
                    "offset": { "type": "integer", "description": "Skip this many matches, to see the next ones" }
                }
            }
        }
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SearchArgs {
    content: Option<String>,
    author_ids: Vec<String>,
    mentions: Vec<String>,
    channel_ids: Vec<String>,
    has: Vec<String>,
    after: Option<String>,
    before: Option<String>,
    oldest_first: bool,
    limit: Option<u8>,
    offset: Option<u32>,
}

/// Searches `guild_id` as `reader` and describes the results for the model. `public_only` when
/// the answer goes to a channel instead of the reader's DMs.
pub async fn run(
    rpc: &rpc::ProtocolClient,
    guild_id: GuildId,
    reader: UserId,
    public_only: bool,
    args: SearchArgs,
) -> String {
    let search = match to_search(args) {
        Ok(search) => search,
        Err(why) => return why,
    };
    let offset = search.offset;

    let found = rpc
        .search_guild_messages(
            rpc::context::current(),
            guild_id,
            reader,
            public_only,
            search,
        )
        .await;
    match found {
        Ok(Ok(results)) => describe(&results, guild_id, offset),
        Ok(Err(err)) => format!("The search failed: {}", err.0),
        Err(err) => format!("The search failed: {err}"),
    }
}

fn to_search(args: SearchArgs) -> Result<MessageSearch, String> {
    let ids = |raw: &[String]| {
        raw.iter()
            .map(|id| parse_id(id).ok_or_else(|| format!("{id}: not a Discord id")))
            .collect::<Result<Vec<_>, _>>()
    };
    let day_id = |day: &Option<String>, field: &str| match day {
        Some(day) => day_start_id(day)
            .map(Some)
            .ok_or_else(|| format!("{field}: {day} isn't a YYYY-MM-DD day since 2015")),
        None => Ok(None),
    };

    Ok(MessageSearch {
        content: args.content.filter(|content| !content.trim().is_empty()),
        author_ids: ids(&args.author_ids)?
            .into_iter()
            .map(UserId::new)
            .collect(),
        mentions: ids(&args.mentions)?.into_iter().map(UserId::new).collect(),
        channel_ids: ids(&args.channel_ids)?
            .into_iter()
            .map(ChannelId::new)
            .collect(),
        has: args.has,
        // Ids are exclusive bounds: one before the day's first possible id
        min_id: day_id(&args.after, "after")?
            .map(|id| MessageId::new(id.get().saturating_sub(1).max(1))),
        max_id: day_id(&args.before, "before")?,
        oldest_first: args.oldest_first,
        limit: args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        offset: args.offset.unwrap_or(0).min(MAX_OFFSET),
    })
}

fn describe(results: &MessageSearchResults, guild_id: GuildId, offset: u32) -> String {
    let mut text = format!(
        "{} messages match (Discord's count, it can be slightly off).",
        results.total_results
    );
    if results.messages.is_empty() {
        return text;
    }

    write!(
        text,
        " Matches {} to {}:",
        offset + 1,
        offset as usize + results.messages.len()
    )
    .ok();
    for message in &results.messages {
        write!(text, "\n- {}", describe_message(message, guild_id)).ok();
    }
    text
}

fn describe_message(message: &Message, guild_id: GuildId) -> String {
    // RFC 3339, cut to the minute
    let time = message.timestamp.to_string().replacen('T', " ", 1);
    let time = time.get(..16).unwrap_or(&time);
    let content = message.content.trim().replace('\n', " / ");
    let content = if content.is_empty() {
        "[no text]".to_string()
    } else {
        clamp_chars(&content, MAX_CONTENT_CHARS)
    };

    format!(
        "{time} UTC, {} (<@{}>) in <#{}>: {content} https://discord.com/channels/{guild_id}/{}/{}",
        message.author.display_name(),
        message.author.id,
        message.channel_id,
        message.channel_id,
        message.id
    )
}

/// Accepts "123", and mentions like "<@123>", "<@!123>" or "<#123>" in case the model copies one
pub fn parse_id(id: &str) -> Option<u64> {
    id.trim()
        .trim_start_matches(['<', '@', '!', '#'])
        .trim_end_matches('>')
        .parse::<u64>()
        .ok()
        .filter(|&id| id != 0)
}

/// The smallest message id of a "YYYY-MM-DD" UTC day. `None` before Discord existed.
fn day_start_id(day: &str) -> Option<MessageId> {
    /// 2015-01-01, the start of Discord ids
    const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

    let mut parts = day.trim().splitn(3, '-').map(str::parse::<i64>);
    let (Some(Ok(year)), Some(Ok(month)), Some(Ok(day))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let ms = days_from_civil(year, month, day) * 86_400_000 - DISCORD_EPOCH_MS;
    (ms > 0).then(|| MessageId::new((ms as u64) << 22))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`)
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_since_the_unix_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2015, 1, 1), 16_436);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(2026, 10, 31), 20_757);
    }

    #[test]
    fn days_become_message_ids() {
        // 2015-01-02 is a day after Discord's epoch
        assert_eq!(
            day_start_id("2015-01-02"),
            Some(MessageId::new(86_400_000 << 22))
        );
        assert_eq!(day_start_id("2015-01-01"), None);
        assert_eq!(day_start_id("2025-13-01"), None);
        assert_eq!(day_start_id("yesterday"), None);
    }

    #[test]
    fn ids_and_mentions_parse() {
        assert_eq!(parse_id("123"), Some(123));
        assert_eq!(parse_id(" <@!123> "), Some(123));
        assert_eq!(parse_id("<#456>"), Some(456));
        assert_eq!(parse_id("0"), None);
        assert_eq!(parse_id("alice"), None);
    }

    #[test]
    fn arguments_become_a_search() {
        let args: SearchArgs = serde_json::from_value(json!({
            "content": "pizza",
            "author_ids": ["<@1>"],
            "after": "2015-01-02",
            "limit": 99
        }))
        .unwrap();
        let search = to_search(args).unwrap();
        assert_eq!(search.content.as_deref(), Some("pizza"));
        assert_eq!(search.author_ids, vec![UserId::new(1)]);
        assert_eq!(search.min_id, Some(MessageId::new((86_400_000 << 22) - 1)));
        assert_eq!(search.max_id, None);
        assert_eq!(search.limit, MAX_LIMIT);
        assert!(search.channel_ids.is_empty());

        let bad: SearchArgs = serde_json::from_value(json!({ "author_ids": ["alice"] })).unwrap();
        assert_eq!(to_search(bad).unwrap_err(), "alice: not a Discord id");
    }

    #[test]
    fn results_count_and_link() {
        let message: Message = serde_json::from_value(json!({
            "id": "30", "channel_id": "20", "content": "I love\npizza",
            "timestamp": "2025-10-31T14:02:03.000000+00:00", "edited_timestamp": null,
            "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
            "attachments": [], "embeds": [], "pinned": false, "type": 0,
            "author": { "id": "1", "username": "alice", "global_name": "Alice", "avatar": null, "discriminator": "0" }
        }))
        .unwrap();
        let results = MessageSearchResults {
            total_results: 42,
            messages: vec![message],
        };
        assert_eq!(
            describe(&results, GuildId::new(10), 5),
            "42 messages match (Discord's count, it can be slightly off). Matches 6 to 6:\n\
            - 2025-10-31 14:02 UTC, Alice (<@1>) in <#20>: I love / pizza https://discord.com/channels/10/20/30"
        );
    }
}
