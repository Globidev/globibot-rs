//! Tools the model can call while answering a ping or a DM

use std::{collections::BTreeMap, fmt::Write};

use globibot_core::{
    rpc,
    serenity::all::{Message, UserId},
    storage::DiscordProfile,
};
use parking_lot::RwLock;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    LlmPlugin, LoreBook,
    context::clamp_chars,
    message_search::{self, parse_id},
    tool_traces::{self, Outcome, ToolTrace, ToolTraces},
    usage::{self, Totals, UsageKind, UsageStats},
};

const MAX_SEARCH_RESULTS: usize = 10;
/// Same limit as quotes added with "Add to lore"
const MAX_FACT_CHARS: usize = 300;
const DEFAULT_USAGE_DAYS: u64 = 7;
const DEFAULT_ANSWERS: usize = 5;
const MAX_ANSWERS: usize = 20;
/// Per argument or result in `get_recent_answers`, to keep the context small
const MAX_CALL_CHARS: usize = 300;

/// The tool definitions sent to OpenRouter
pub fn tool_definitions() -> Vec<Value> {
    let mut definitions = lore_tools();
    definitions.extend(usage_tools());
    definitions.push(message_search::definition());
    definitions
}

fn usage_tools() -> Vec<Value> {
    let kept_days = tool_traces::KEPT_SECS / 86_400;
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "get_usage",
                "description": "Your own usage of the model provider: requests, tokens, cache hits and cost \
                    in US dollars, by kind of request and per UTC day.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "days": {
                            "type": "integer",
                            "description": format!(
                                "How many days back, today included. Default {DEFAULT_USAGE_DAYS}, at most {}",
                                usage::KEPT_DAYS
                            )
                        }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "get_recent_answers",
                "description": format!(
                    "What happened in your latest answers to pings and DMs, from the last {kept_days} days, newest first: \
                    who asked, the tools you called with their arguments and results, rounds, tokens, cost, time taken \
                    and errors. It doesn't have the questions or your answers: those are in the chat."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": format!("How many answers. Default {DEFAULT_ANSWERS}, at most {MAX_ANSWERS}")
                        },
                        "requester": {
                            "type": "string",
                            "description": "Only answers to people whose name contains this. People asking in DMs are never shown."
                        },
                        "tool": {
                            "type": "string",
                            "description": "Only answers that called this tool, e.g. \"search_lore\""
                        }
                    }
                }
            }
        }),
    ]
}

impl LlmPlugin {
    /// Runs the tool `name` with the model's JSON `args`, while answering `asked`. Errors are
    /// returned as text for the model.
    pub async fn run_tool(
        &self,
        rpc: &rpc::ProtocolClient,
        asked: &Message,
        name: &str,
        args: &str,
    ) -> String {
        #[derive(Deserialize)]
        struct GetUsage {
            days: Option<u64>,
        }
        #[derive(Deserialize)]
        struct GetRecentAnswers {
            limit: Option<usize>,
            requester: Option<String>,
            tool: Option<String>,
        }

        let now = crate::unix_now();
        let result = match name {
            "search_messages" => match parse_args(args) {
                Ok(search) => {
                    // Results posted in a channel must only come from channels everyone can read
                    let public_only = asked.guild_id.is_some();
                    let found = message_search::run(
                        rpc,
                        self.guild_id,
                        asked.author.id,
                        public_only,
                        search,
                    );
                    Ok(found.await)
                }
                Err(err) => Err(err),
            },
            "get_usage" => parse_args(args).map(|GetUsage { days }| {
                let days = days
                    .unwrap_or(DEFAULT_USAGE_DAYS)
                    .clamp(1, usage::KEPT_DAYS);
                usage_report(&self.llm_client.usage(), days, now)
            }),
            "get_recent_answers" => parse_args(args).map(|filter: GetRecentAnswers| {
                recent_answers(
                    &self.tool_traces.lock(),
                    filter
                        .limit
                        .unwrap_or(DEFAULT_ANSWERS)
                        .clamp(1, MAX_ANSWERS),
                    filter.requester.as_deref(),
                    filter.tool.as_deref(),
                    now,
                )
            }),
            _ => return run_lore_tool(&self.lore_book, &self.bot_profile, name, args),
        };

        result.unwrap_or_else(|err| format!("Invalid arguments for {name}: {err}"))
    }
}

/// Models sometimes send no arguments at all for a tool without required ones
fn parse_args<T: DeserializeOwned>(args: &str) -> serde_json::Result<T> {
    let args = if args.trim().is_empty() { "{}" } else { args };
    serde_json::from_str(args)
}

/// Totals by kind and per day, over the last `days` UTC days
fn usage_report(stats: &UsageStats, days: u64, now: u64) -> String {
    let today = now / 86_400;
    let first_day = (today + 1).saturating_sub(days);

    let mut total = Totals::default();
    let mut by_kind = BTreeMap::<UsageKind, Totals>::new();
    let mut per_day = String::new();
    for (day, kinds) in stats.days.range(first_day..=today).rev() {
        let mut day_total = Totals::default();
        for (kind, totals) in kinds {
            by_kind.entry(*kind).or_default().add(totals);
            day_total.add(totals);
        }
        total.add(&day_total);
        writeln!(
            per_day,
            "- {}: {}",
            days_ago(today - day),
            totals_line(&day_total)
        )
        .ok();
    }

    if total.requests == 0 {
        return format!("No requests in the last {days} days");
    }

    let mut report = format!(
        "Usage over the last {days} days, today included\nTotal: {}\nBy kind:\n",
        totals_line(&total)
    );
    for (kind, totals) in &by_kind {
        writeln!(report, "- {}: {}", kind_name(*kind), totals_line(totals)).ok();
    }
    write!(report, "Per day:\n{per_day}").ok();
    report
}

fn totals_line(totals: &Totals) -> String {
    let cached = (totals.cached_tokens * 100)
        .checked_div(totals.prompt_tokens)
        .unwrap_or(0);
    format!(
        "{} requests, ${:.4}, {} input tokens ({cached}% cached), {} output tokens ({} reasoning), {} images",
        totals.requests,
        totals.cost,
        totals.prompt_tokens,
        totals.completion_tokens,
        totals.reasoning_tokens,
        totals.images
    )
}

/// Same names as the web page
fn kind_name(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Chat => "chat replies",
        UsageKind::DirectMessage => "DM replies",
        UsageKind::Command => "commands (/roast, /hype, /tldr)",
        UsageKind::Starboard => "Starboard comments",
        UsageKind::Summary => "summaries of older messages",
        UsageKind::Caption => "image captions",
    }
}

/// The latest traces, newest first. `requester` matches part of a name, ignoring case.
fn recent_answers(
    traces: &ToolTraces,
    limit: usize,
    requester: Option<&str>,
    tool: Option<&str>,
    now: u64,
) -> String {
    let requester = requester.map(str::to_lowercase);
    let answers = traces
        .traces
        .iter()
        .rev()
        .filter(|trace| match &requester {
            Some(wanted) => trace
                .requester
                .as_ref()
                .is_some_and(|name| name.to_lowercase().contains(wanted)),
            None => true,
        })
        .filter(|trace| tool.is_none_or(|tool| trace.calls.iter().any(|call| call.name == tool)))
        .take(limit)
        .map(|trace| describe_trace(trace, now))
        .collect::<Vec<_>>();

    if answers.is_empty() {
        "No matching answers".to_string()
    } else {
        answers.join("\n\n")
    }
}

fn describe_trace(trace: &ToolTrace, now: u64) -> String {
    let who = trace.requester.as_deref().unwrap_or("someone in a DM");
    let mut text = format!(
        "{}, answering {who}, with {}, took {:.1}s",
        time_ago(now.saturating_sub(trace.at)),
        trace.model,
        trace.duration_ms as f64 / 1000.0
    );
    match &trace.outcome {
        Outcome::Answered { rounds, totals } => write!(
            text,
            ", {rounds} round(s), ${:.4}, {} input / {} output tokens",
            totals.cost, totals.prompt_tokens, totals.completion_tokens
        ),
        Outcome::FellBack { error } => write!(
            text,
            ", the tools failed so it answered without them: {}",
            clamp_chars(error, MAX_CALL_CHARS)
        ),
    }
    .ok();

    if trace.calls.is_empty() {
        text.push_str("\n- no tool calls");
    }
    for call in &trace.calls {
        let result = call.result.replace('\n', " / ");
        write!(
            text,
            "\n- {}({}) -> {}",
            call.name,
            clamp_chars(&call.arguments, MAX_CALL_CHARS),
            clamp_chars(&result, MAX_CALL_CHARS)
        )
        .ok();
    }
    text
}

fn time_ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

fn days_ago(days: u64) -> String {
    match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        _ => format!("{days} days ago"),
    }
}

fn lore_tools() -> Vec<Value> {
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

/// Runs the lore tool `name`, like `LlmPlugin::run_tool`. `bot` is the suggester of
/// `suggest_lore` facts.
fn run_lore_tool(
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
        "get_lore" => parse_args(args).map(|GetLore { user_ids }| {
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
            parse_args(args).map(|SearchLore { query }| lore_book.read().search(&query))
        }
        "suggest_lore" => parse_args(args).map(|SuggestLore { user_id, fact }| {
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
    parse_id(id).map(UserId::new)
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

    #[test]
    fn empty_arguments_count_as_none() {
        #[derive(Deserialize)]
        struct Optional {
            days: Option<u64>,
        }
        assert!(parse_args::<Optional>("").unwrap().days.is_none());
        assert!(parse_args::<Optional>(" {} ").unwrap().days.is_none());
    }

    fn usage(cost: f64) -> usage::UsageReport {
        serde_json::from_value(json!({
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "prompt_tokens_details": { "cached_tokens": 50 },
            "cost": cost
        }))
        .unwrap()
    }

    #[test]
    fn usage_report_adds_up_the_days_asked_for() {
        const DAY: u64 = 86_400;
        let today = 100 * DAY;
        let mut stats = UsageStats::default();
        stats.record(today - 10 * DAY, UsageKind::Chat, "m", 0, &usage(8.0));
        stats.record(today - DAY, UsageKind::Chat, "m", 0, &usage(0.25));
        stats.record(today - DAY, UsageKind::Summary, "m", 0, &usage(0.25));
        stats.record(today, UsageKind::Chat, "m", 0, &usage(0.5));

        let report = usage_report(&stats, 7, today + 60);
        assert!(
            report.contains("Total: 3 requests, $1.0000, 300 input tokens (50% cached)"),
            "{report}"
        );
        assert!(
            report.contains("- chat replies: 2 requests, $0.7500"),
            "{report}"
        );
        assert!(
            report.contains("- yesterday: 2 requests, $0.5000"),
            "{report}"
        );
        assert!(!report.contains("$8"), "{report}");

        let empty = usage_report(&UsageStats::default(), 7, today);
        assert_eq!(empty, "No requests in the last 7 days");
    }

    fn trace(at: u64, requester: Option<&str>, tool: &str) -> ToolTrace {
        ToolTrace {
            at,
            kind: UsageKind::Chat,
            model: "m".into(),
            requester: requester.map(str::to_string),
            message_url: None,
            calls: vec![tool_traces::ToolCallRecord {
                name: tool.into(),
                arguments: r#"{"query":"cats"}"#.into(),
                result: "Alice (<@1>): Loves cats\nBob (<@2>): Has a cat".into(),
            }],
            duration_ms: 1_500,
            outcome: Outcome::Answered {
                rounds: 2,
                totals: Totals::default(),
            },
        }
    }

    #[test]
    fn recent_answers_are_newest_first_and_filtered() {
        let mut traces = ToolTraces::default();
        traces.record(trace(1_000, Some("Alice"), "get_lore"));
        traces.record(trace(2_000, Some("Bob"), "search_lore"));
        traces.record(trace(3_000, None, "get_lore"));
        let now = 3_120;

        let all = recent_answers(&traces, 5, None, None, now);
        let answers = all.split("\n\n").collect::<Vec<_>>();
        assert_eq!(answers.len(), 3);
        assert!(
            answers[0]
                .starts_with("2 min ago, answering someone in a DM, with m, took 1.5s, 2 round(s)"),
            "{all}"
        );
        assert!(
            answers[0].ends_with(
                "\n- get_lore({\"query\":\"cats\"}) -> Alice (<@1>): Loves cats / Bob (<@2>): Has a cat"
            ),
            "{all}"
        );
        assert!(answers[2].contains("answering Alice"));

        assert_eq!(
            recent_answers(&traces, 1, None, None, now)
                .split("\n\n")
                .count(),
            1
        );

        let alice = recent_answers(&traces, 5, Some("ALI"), None, now);
        // DMs never match a name
        assert_eq!(alice.matches("answering").count(), 1, "{alice}");
        assert!(alice.contains("answering Alice"), "{alice}");

        let searches = recent_answers(&traces, 5, None, Some("search_lore"), now);
        assert_eq!(searches.matches("answering").count(), 1, "{searches}");
        assert!(searches.contains("answering Bob"), "{searches}");

        assert_eq!(
            recent_answers(&traces, 5, Some("zed"), None, now),
            "No matching answers"
        );
    }
}
