//! OpenRouter usage: tokens, cache hits and cost of every completion, kept as daily totals per
//! kind of request plus the latest requests. Shown on the plugin's web page.

use std::collections::{BTreeMap, VecDeque};

use globibot_core::storage::{self, StorageValue};
use serde::{Deserialize, Serialize};

const KEPT_DAYS: u64 = 90;
const KEPT_REQUESTS: usize = 100;

pub const USAGE_KEY: &str = "latest";

/// What a completion was for
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// Replies when mentioned
    Chat,
    /// `/roast`, `/hype`, `/tldr`
    Command,
    /// Comments on Starboard posts
    Starboard,
    /// Summaries of dropped context
    Summary,
}

/// The `usage` object of an OpenRouter response
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageReport {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptTokensDetails>,
    completion_tokens_details: Option<CompletionTokensDetails>,
    /// In credits, which are US dollars
    cost: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CompletionTokensDetails {
    reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub requests: u64,
    /// Includes cached tokens
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    /// Includes reasoning tokens
    pub completion_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
}

impl Totals {
    fn from_report(report: &UsageReport) -> Self {
        Self {
            requests: 1,
            prompt_tokens: report.prompt_tokens.unwrap_or_default(),
            cached_tokens: report
                .prompt_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens)
                .unwrap_or_default(),
            completion_tokens: report.completion_tokens.unwrap_or_default(),
            reasoning_tokens: report
                .completion_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens)
                .unwrap_or_default(),
            cost: report.cost.unwrap_or_default(),
        }
    }

    fn add(&mut self, other: &Self) {
        self.requests += other.requests;
        self.prompt_tokens += other.prompt_tokens;
        self.cached_tokens += other.cached_tokens;
        self.completion_tokens += other.completion_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cost += other.cost;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Unix seconds
    pub at: u64,
    pub kind: UsageKind,
    pub model: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    /// Keyed by UTC day number (days since the Unix epoch)
    pub days: BTreeMap<u64, BTreeMap<UsageKind, Totals>>,
    /// Latest last
    pub recent: VecDeque<RequestRecord>,
}

impl StorageValue for UsageStats {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_usage";
}

impl UsageStats {
    pub fn record(&mut self, at: u64, kind: UsageKind, model: &str, report: &UsageReport) {
        let totals = Totals::from_report(report);
        let day = at / 86_400;

        self.days
            .entry(day)
            .or_default()
            .entry(kind)
            .or_default()
            .add(&totals);
        self.days.retain(|kept_day, _| *kept_day + KEPT_DAYS > day);

        self.recent.push_back(RequestRecord {
            at,
            kind,
            model: model.to_string(),
            totals,
        });
        if self.recent.len() > KEPT_REQUESTS {
            self.recent.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> UsageReport {
        serde_json::from_value(serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050,
            "prompt_tokens_details": { "cached_tokens": 800, "cache_write_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 20 },
            "cost": 0.0012
        }))
        .unwrap()
    }

    #[test]
    fn adds_up_per_day_and_kind() {
        let mut stats = UsageStats::default();
        stats.record(86_400 * 10 + 5, UsageKind::Chat, "m", &report());
        stats.record(86_400 * 10 + 9, UsageKind::Chat, "m", &report());
        stats.record(86_400 * 10 + 9, UsageKind::Summary, "m", &report());

        let day = &stats.days[&10];
        let chat = day[&UsageKind::Chat];
        assert_eq!(chat.requests, 2);
        assert_eq!(chat.prompt_tokens, 2000);
        assert_eq!(chat.cached_tokens, 1600);
        assert_eq!(chat.reasoning_tokens, 40);
        assert!((chat.cost - 0.0024).abs() < 1e-9);
        assert_eq!(day[&UsageKind::Summary].requests, 1);
        assert_eq!(stats.recent.len(), 3);
    }

    #[test]
    fn missing_fields_count_as_zero() {
        let report = serde_json::from_value::<UsageReport>(serde_json::json!({
            "prompt_tokens": 10,
            "prompt_tokens_details": null
        }))
        .unwrap();
        let mut stats = UsageStats::default();
        stats.record(0, UsageKind::Chat, "m", &report);
        assert_eq!(stats.days[&0][&UsageKind::Chat].prompt_tokens, 10);
        assert_eq!(stats.days[&0][&UsageKind::Chat].cached_tokens, 0);
    }

    #[test]
    fn keeps_recent_days_and_requests() {
        let mut stats = UsageStats::default();
        for day in 0..200 {
            stats.record(day * 86_400, UsageKind::Chat, "m", &report());
        }
        assert_eq!(stats.days.len(), KEPT_DAYS as usize);
        assert_eq!(*stats.days.keys().next().unwrap(), 200 - KEPT_DAYS);
        assert_eq!(stats.recent.len(), KEPT_REQUESTS);
        assert_eq!(stats.recent.back().unwrap().at, 199 * 86_400);
    }

    #[test]
    fn round_trips_as_json() {
        let mut stats = UsageStats::default();
        stats.record(0, UsageKind::Starboard, "m", &report());
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["days"]["0"]["starboard"]["requests"], 1);
        assert_eq!(json["recent"][0]["kind"], "starboard");
        assert_eq!(json["recent"][0]["prompt_tokens"], 1000);
        let parsed = serde_json::from_value::<UsageStats>(json).unwrap();
        assert_eq!(parsed.days[&0][&UsageKind::Starboard].requests, 1);
    }
}
