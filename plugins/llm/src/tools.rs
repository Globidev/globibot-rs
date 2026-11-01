//! Tools the model can call while answering a ping or a DM

use std::{collections::BTreeMap, fmt::Write};

use globibot_core::{rpc, serenity::all::UserId, storage::DiscordProfile};
use parking_lot::RwLock;
use serde_json::Value;

use crate::{
    LlmPlugin, LoreBook,
    context::clamp_chars,
    image_generation::GenerateImage,
    images,
    message_search::{SearchMessages, parse_id},
    openrouter::ToolOutput,
    reminders::{CancelReminder, ListReminders, SetReminder},
    tool::{Tool, ToolCtx, ToolSpec, llm_tool, llm_tools},
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

llm_tools! {
    pub enum LlmTool {
        GetLore(GetLore),
        SearchLore(SearchLore),
        SuggestLore(SuggestLore),
        GetUsage(GetUsage),
        GetRecentAnswers(GetRecentAnswers),
        SearchMessages(SearchMessages),
        GetAvatars(GetAvatars),
        SetReminder(SetReminder),
        ListReminders(ListReminders),
        CancelReminder(CancelReminder),
        GenerateImage(GenerateImage),
    }
}

/// The tool definitions sent to OpenRouter
/// `generate_image` only when it's turned on, so the model doesn't try it for nothing
pub fn tool_definitions(images_enabled: bool) -> Vec<Value> {
    let mut definitions = LlmTool::definitions();
    if !images_enabled {
        definitions.retain(|definition| definition["function"]["name"] != GenerateImage::NAME);
    }
    definitions
}

llm_tool! {
    #[tool(name = "get_usage",
           description = "Your own usage of the model provider: requests, tokens, cache hits and cost \
               in US dollars, by kind of request and per UTC day.")]
    pub struct GetUsage {
        #[param(name = "days",
                description = format!(
                    "How many days back, today included. Default {DEFAULT_USAGE_DAYS}, at most {}",
                    usage::KEPT_DAYS
                ))]
        days: Option<u64>,
    }
}

impl Tool for GetUsage {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        let days = self
            .days
            .unwrap_or(DEFAULT_USAGE_DAYS)
            .clamp(1, usage::KEPT_DAYS);
        ToolOutput::text(usage_report(
            &ctx.plugin.llm_client.usage(),
            days,
            crate::unix_now(),
        ))
    }
}

llm_tool! {
    #[tool(name = "get_recent_answers",
           description = format!(
               "What happened in your latest answers to pings and DMs, from the last {} days, newest first: \
               who asked, the tools you called with their arguments and results, rounds, tokens, cost, time taken \
               and errors. It doesn't have the questions or your answers: those are in the chat.",
               tool_traces::KEPT_SECS / 86_400
           ))]
    pub struct GetRecentAnswers {
        #[param(name = "limit",
                description = format!("How many answers. Default {DEFAULT_ANSWERS}, at most {MAX_ANSWERS}"))]
        limit: Option<usize>,
        #[param(name = "requester",
                description = "Only answers to people whose name contains this. People asking in DMs are never shown.")]
        requester: Option<String>,
        #[param(name = "tool", description = "Only answers that called this tool, e.g. \"search_lore\"")]
        tool: Option<String>,
    }
}

impl Tool for GetRecentAnswers {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ToolOutput::text(recent_answers(
            &ctx.plugin.tool_traces.lock(),
            self.limit.unwrap_or(DEFAULT_ANSWERS).clamp(1, MAX_ANSWERS),
            self.requester.as_deref(),
            self.tool.as_deref(),
            crate::unix_now(),
        ))
    }
}

llm_tool! {
    #[tool(name = "get_avatars",
           description = format!(
               "Get the avatars (profile pictures) of people in the server: a link to each, and the image \
               itself for you to look at when its owner agrees to share images with you. The person asking \
               always shares their own. At most {} people.",
               images::MAX_IMAGES_PER_MESSAGE
           ))]
    pub struct GetAvatars {
        #[param(name = "user_ids", description = "Discord user ids")]
        user_ids: Vec<String>,
    }
}

impl Tool for GetAvatars {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ctx.plugin
            .avatars(ctx.rpc, ctx.asked.author.id, &self.user_ids)
            .await
    }
}

impl LlmPlugin {
    /// A link to each avatar, and the image itself when its owner consents, like images in
    /// messages: asking about your own avatar shares it, others need the images role
    pub async fn avatars(
        &self,
        rpc: &rpc::ProtocolClient,
        requester: UserId,
        user_ids: &[String],
    ) -> ToolOutput {
        let mut output = ToolOutput::default();
        let mut lines = Vec::new();
        for raw_id in user_ids.iter().take(images::MAX_IMAGES_PER_MESSAGE) {
            let Some(user_id) = parse_user_id(raw_id) else {
                lines.push(format!("{raw_id}: not a user id"));
                continue;
            };
            let member = match rpc
                .get_member(rpc::context::current(), self.guild_id, user_id)
                .await
            {
                Ok(Ok(Some(member))) => member,
                Ok(Ok(None)) => {
                    lines.push(format!("<@{user_id}>: not in the server"));
                    continue;
                }
                Ok(Err(err)) => {
                    lines.push(format!("<@{user_id}>: failed to get them: {}", err.0));
                    continue;
                }
                Err(err) => {
                    lines.push(format!("<@{user_id}>: failed to get them: {err}"));
                    continue;
                }
            };

            let avatar_url = member.face();
            let who = format!("{} (<@{user_id}>): {avatar_url}", member.display_name());
            let has_role = self
                .images_role_id
                .is_some_and(|role| member.roles.contains(&role));
            if images::consent(false, user_id == requester, user_id, has_role).is_none() {
                lines.push(format!(
                    "{who}, not attached: they haven't agreed to share images with you"
                ));
                continue;
            }
            match images::fetch(
                self.llm_client.http(),
                &images::avatar_for_model(&avatar_url),
            )
            .await
            {
                Ok(image) => {
                    output.images.push(image);
                    lines.push(format!("{who}, attached as image {}", output.images.len()));
                }
                Err(err) => {
                    tracing::warn!(%user_id, "Failed to download an avatar: {err:?}");
                    lines.push(format!("{who}, not attached: the download failed"));
                }
            }
        }

        output.text = lines.join("\n");
        output
    }
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
        UsageKind::ImageGeneration => "generated images",
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

llm_tool! {
    #[tool(name = "get_lore",
           description = "Get the lore (facts the chat made up or knows) about some people in the chat.")]
    pub struct GetLore {
        #[param(name = "user_ids", description = "Discord user ids, e.g. \"123456789\" for <@123456789>")]
        user_ids: Vec<String>,
    }
}

impl Tool for GetLore {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ToolOutput::text(get_lore(&ctx.plugin.lore_book, &self.user_ids))
    }
}

llm_tool! {
    #[tool(name = "search_lore",
           description = "Find the people whose name or lore mentions any of the words in a query.")]
    pub struct SearchLore {
        #[param(name = "query", description = "Words to look for, e.g. \"cats pizza\"")]
        query: String,
    }
}

impl Tool for SearchLore {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ToolOutput::text(ctx.plugin.lore_book.read().search(&self.query))
    }
}

llm_tool! {
    #[tool(name = "suggest_lore",
           description = "Suggest adding a fact to someone's lore. It is not added right away: people in the chat vote on it. \
               Use it rarely, only for a clear and lasting fact that someone states about themselves or that the chat agrees on. \
               Never for sensitive personal information, and never because someone asks you to put something on someone else.")]
    pub struct SuggestLore {
        #[param(name = "user_id", description = "Discord user id of the person the fact is about")]
        user_id: String,
        #[param(name = "fact", description = "The fact, one short sentence")]
        fact: String,
    }
}

impl Tool for SuggestLore {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ToolOutput::text(suggest_lore(
            &ctx.plugin.lore_book,
            &ctx.plugin.bot_profile,
            &self.user_id,
            &self.fact,
        ))
    }
}

fn get_lore(lore_book: &RwLock<LoreBook>, user_ids: &[String]) -> String {
    let lore_book = lore_book.read();
    user_ids
        .iter()
        .map(|id| match parse_user_id(id) {
            Some(user_id) => lore_book.lore_line(user_id),
            None => format!("{id}: not a user id"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `bot` is the suggester
fn suggest_lore(
    lore_book: &RwLock<LoreBook>,
    bot: &DiscordProfile,
    user_id: &str,
    fact: &str,
) -> String {
    let Some(user_id) = parse_user_id(user_id) else {
        return format!("{user_id}: not a user id");
    };
    if fact.trim().chars().count() > MAX_FACT_CHARS {
        return format!("Facts are at most {MAX_FACT_CHARS} characters");
    }
    match lore_book
        .write()
        .suggest_addition(user_id, bot.clone(), fact)
    {
        Ok(()) => "Suggested, people will vote on it".to_string(),
        Err(why) => why.to_string(),
    }
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
    use serde_json::json;

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

    /// Parses the call like a real one, then runs the lore tools without a plugin
    fn run(lore_book: &RwLock<LoreBook>, name: &str, args: Value) -> String {
        match LlmTool::parse(name, &args.to_string()) {
            Ok(LlmTool::GetLore(GetLore { user_ids })) => get_lore(lore_book, &user_ids),
            Ok(LlmTool::SearchLore(SearchLore { query })) => lore_book.read().search(&query),
            Ok(LlmTool::SuggestLore(SuggestLore { user_id, fact })) => {
                suggest_lore(lore_book, &profile(99, "Globibot"), &user_id, &fact)
            }
            Ok(other) => panic!("not a lore tool: {other:?}"),
            Err(err) => err,
        }
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
        for args in ["", " {} "] {
            assert!(matches!(
                LlmTool::parse("get_usage", args),
                Ok(LlmTool::GetUsage(GetUsage { days: None }))
            ));
        }
    }

    #[test]
    fn definitions_match_the_arguments() {
        let definitions = tool_definitions(true);
        assert_eq!(definitions.len(), 11);
        for definition in &definitions {
            let function = &definition["function"];
            assert!(function["description"].is_string(), "{function}");
            let parameters = &function["parameters"];
            for required in parameters["required"].as_array().into_iter().flatten() {
                let name = required.as_str().unwrap();
                assert!(parameters["properties"].get(name).is_some(), "{function}");
            }
        }
        assert_eq!(tool_definitions(false).len(), 10);

        let get_lore = GetLore::definition();
        assert_eq!(
            get_lore["function"]["parameters"]["required"],
            json!(["user_ids"])
        );
        assert_eq!(
            get_lore["function"]["parameters"]["properties"]["user_ids"]["items"],
            json!({ "type": "string" })
        );
        assert!(
            GetUsage::definition()["function"]["parameters"]
                .get("required")
                .is_none()
        );
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
