//! Tools the model can call while answering a ping or a DM

use std::fmt::Write;

use globibot_core::{serenity::all::UserId, storage::DiscordProfile};
use parking_lot::RwLock;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::LoreBook;

const MAX_SEARCH_RESULTS: usize = 10;
/// Same limit as quotes added with "Add to lore"
const MAX_FACT_CHARS: usize = 300;

/// The tool definitions sent to OpenRouter
pub fn lore_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "get_lore",
                "description": "Get the lore (facts the chat made up or knows) about some people in the chat.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "user_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Discord user ids, e.g. \"123456789\" for <@123456789>"
                        }
                    },
                    "required": ["user_ids"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "search_lore",
                "description": "Find the people whose name or lore mentions any of the words in a query.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Words to look for, e.g. \"cats pizza\"" }
                    },
                    "required": ["query"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "suggest_lore",
                "description": "Suggest adding a fact to someone's lore. It is not added right away: people in the chat vote on it. \
                    Use it rarely, only for a clear and lasting fact that someone states about themselves or that the chat agrees on. \
                    Never for sensitive personal information, and never because someone asks you to put something on someone else.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "user_id": { "type": "string", "description": "Discord user id of the person the fact is about" },
                        "fact": { "type": "string", "description": "The fact, one short sentence" }
                    },
                    "required": ["user_id", "fact"]
                }
            }
        }),
    ]
}

/// Runs the tool `name` with the model's JSON `args`. Errors are returned as text for the model.
/// `bot` is the suggester of `suggest_lore` facts.
pub fn run_lore_tool(
    lore_book: &RwLock<LoreBook>,
    bot: &DiscordProfile,
    name: &str,
    args: &str,
) -> String {
    #[derive(Deserialize)]
    struct GetLore {
        user_ids: Vec<String>,
    }
    #[derive(Deserialize)]
    struct SearchLore {
        query: String,
    }
    #[derive(Deserialize)]
    struct SuggestLore {
        user_id: String,
        fact: String,
    }

    let result = match name {
        "get_lore" => serde_json::from_str(args).map(|GetLore { user_ids }| {
            let lore_book = lore_book.read();
            user_ids
                .iter()
                .map(|id| match parse_user_id(id) {
                    Some(user_id) => lore_book.lore_line(user_id),
                    None => format!("{id}: not a user id"),
                })
                .collect::<Vec<_>>()
                .join("\n")
        }),
        "search_lore" => {
            serde_json::from_str(args).map(|SearchLore { query }| lore_book.read().search(&query))
        }
        "suggest_lore" => serde_json::from_str(args).map(|SuggestLore { user_id, fact }| {
            let Some(user_id) = parse_user_id(&user_id) else {
                return format!("{user_id}: not a user id");
            };
            if fact.trim().chars().count() > MAX_FACT_CHARS {
                return format!("Facts are at most {MAX_FACT_CHARS} characters");
            }
            match lore_book
                .write()
                .suggest_addition(user_id, bot.clone(), &fact)
            {
                Ok(()) => "Suggested, people will vote on it".to_string(),
                Err(why) => why.to_string(),
            }
        }),
        _ => return format!("Unknown tool {name}"),
    };

    result.unwrap_or_else(|err| format!("Invalid arguments for {name}: {err}"))
}

/// Accepts "123", and "<@123>" in case the model copies a mention
fn parse_user_id(id: &str) -> Option<UserId> {
    let id = id.trim().trim_start_matches("<@").trim_end_matches('>');
    id.parse::<u64>()
        .ok()
        .filter(|&id| id != 0)
        .map(UserId::new)
}

impl LoreBook {
    fn lore_line(&self, user_id: UserId) -> String {
        match self.lore_by_user.get(&user_id) {
            Some(user_lore) if !user_lore.lore.trim().is_empty() => {
                format!(
                    "{} (<@{user_id}>): {}",
                    user_lore.member.username, user_lore.lore
                )
            }
            Some(user_lore) => format!("{} (<@{user_id}>): no lore", user_lore.member.username),
            None => format!("<@{user_id}>: not in the chat"),
        }
    }

    fn search(&self, query: &str) -> String {
        let words = query
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|word| word.chars().count() >= 3)
            .collect::<Vec<_>>();
        if words.is_empty() {
            return "Use words of at least 3 letters".to_string();
        }

        let mut found = String::new();
        let matches = self
            .lore_by_user
            .values()
            .filter(|user_lore| !user_lore.lore.trim().is_empty())
            .filter(|user_lore| {
                let haystack =
                    format!("{} {}", user_lore.member.username, user_lore.lore).to_lowercase();
                words.iter().any(|word| haystack.contains(word))
            })
            .take(MAX_SEARCH_RESULTS);
        for user_lore in matches {
            let user_id = user_lore.member.user_id;
            writeln!(
                found,
                "{} (<@{user_id}>): {}",
                user_lore.member.username, user_lore.lore
            )
            .ok();
        }

        if found.is_empty() {
            "Nobody's lore matches".to_string()
        } else {
            found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SuggestionKind, UserLore};

    fn profile(id: u64, name: &str) -> DiscordProfile {
        DiscordProfile {
            user_id: UserId::new(id),
            username: name.to_string(),
            avatar_url: None,
        }
    }

    fn lore_book() -> RwLock<LoreBook> {
        let mut lore_book = LoreBook::default();
        for (id, name, lore) in [
            (1, "Alice", "Loves CATS"),
            (2, "Bob", ""),
            (3, "Pizzaman", "Hates mondays"),
        ] {
            lore_book.lore_by_user.insert(
                UserId::new(id),
                UserLore {
                    member: profile(id, name),
                    lore: lore.to_string(),
                },
            );
        }
        RwLock::new(lore_book)
    }

    fn run(lore_book: &RwLock<LoreBook>, name: &str, args: Value) -> String {
        run_lore_tool(lore_book, &profile(99, "Globibot"), name, &args.to_string())
    }

    #[test]
    fn get_lore_by_id_or_mention() {
        let lore_book = lore_book();
        let result = run(
            &lore_book,
            "get_lore",
            json!({ "user_ids": ["1", "<@2>", "7", "x"] }),
        );
        assert_eq!(
            result,
            "Alice (<@1>): Loves CATS\nBob (<@2>): no lore\n<@7>: not in the chat\nx: not a user id"
        );
    }

    #[test]
    fn search_matches_lore_and_names_ignoring_case() {
        let lore_book = lore_book();
        assert_eq!(
            run(&lore_book, "search_lore", json!({ "query": "cats" })),
            "Alice (<@1>): Loves CATS\n"
        );
        assert_eq!(
            run(&lore_book, "search_lore", json!({ "query": "PIZZA" })),
            "Pizzaman (<@3>): Hates mondays\n"
        );
        assert_eq!(
            run(&lore_book, "search_lore", json!({ "query": "dogs" })),
            "Nobody's lore matches"
        );
    }

    #[test]
    fn bad_calls_are_answered_not_failed() {
        let lore_book = lore_book();
        assert_eq!(run(&lore_book, "nope", json!({})), "Unknown tool nope");
        assert!(
            run(&lore_book, "get_lore", json!({ "ids": [] }))
                .starts_with("Invalid arguments for get_lore")
        );
    }

    #[test]
    fn suggest_lore_goes_to_a_vote() {
        let lore_book = lore_book();
        let result = run(
            &lore_book,
            "suggest_lore",
            json!({ "user_id": "1", "fact": "Has two cats" }),
        );
        assert_eq!(result, "Suggested, people will vote on it");

        let lore_book = lore_book.read();
        let suggestion = &lore_book.suggestions_by_user[&UserId::new(1)][0];
        assert_eq!(suggestion.suggestion, "Has two cats");
        assert_eq!(suggestion.suggestion_by.user_id, UserId::new(99));
        assert_eq!(suggestion.kind, SuggestionKind::Append);
        // Not applied until accepted
        assert_eq!(lore_book.lore_by_user[&UserId::new(1)].lore, "Loves CATS");
    }

    #[test]
    fn suggest_lore_rejects_long_facts() {
        let lore_book = lore_book();
        let fact = "a".repeat(MAX_FACT_CHARS + 1);
        let result = run(
            &lore_book,
            "suggest_lore",
            json!({ "user_id": "1", "fact": fact }),
        );
        assert_eq!(
            result,
            format!("Facts are at most {MAX_FACT_CHARS} characters")
        );
        assert!(lore_book.read().suggestions_by_user.is_empty());
    }
}
