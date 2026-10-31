//! What happened in each answer where the model could call tools: who asked, the calls with
//! their arguments and results, and how it ended. Kept a few days, shown on the plugin's web page.

use std::collections::VecDeque;

use globibot_core::storage::{self, StorageValue};
use serde::{Deserialize, Serialize};

use crate::{
    context::clamp_chars,
    usage::{Totals, UsageKind},
};

const KEPT_SECS: u64 = 3 * 86_400;
const KEPT_TRACES: usize = 500;
/// Per argument or result: a lore lookup of many people can be long
const MAX_TEXT_CHARS: usize = 2_000;

pub const TOOL_TRACES_KEY: &str = "latest";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolTraces {
    /// Latest last
    pub traces: VecDeque<ToolTrace>,
}

impl StorageValue for ToolTraces {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_tool_traces";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrace {
    /// Unix seconds
    pub at: u64,
    pub kind: UsageKind,
    pub model: String,
    /// `None` in DMs: who talks to the bot privately isn't shown
    pub requester: Option<String>,
    /// The answered message, `None` in DMs
    pub message_url: Option<String>,
    pub calls: Vec<ToolCallRecord>,
    /// Until the answer, or the failure, of the completion with tools
    pub duration_ms: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    /// JSON, as the model wrote it
    pub arguments: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Answered {
        /// Requests made: one per round of tool calls, plus the answer
        rounds: usize,
        totals: Totals,
    },
    /// The completion with tools failed: the answer came from a retry without tools
    FellBack { error: String },
}

impl ToolTraces {
    pub fn record(&mut self, mut trace: ToolTrace) {
        for call in &mut trace.calls {
            call.arguments = clamp_chars(&call.arguments, MAX_TEXT_CHARS);
            call.result = clamp_chars(&call.result, MAX_TEXT_CHARS);
        }
        let now = trace.at;
        self.traces.push_back(trace);
        self.prune(now);
    }

    /// Drops what's older than a few days, or past the count limit
    pub fn prune(&mut self, now: u64) {
        self.traces.retain(|trace| trace.at + KEPT_SECS > now);
        while self.traces.len() > KEPT_TRACES {
            self.traces.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(at: u64, result: &str) -> ToolTrace {
        ToolTrace {
            at,
            kind: UsageKind::Chat,
            model: "m".into(),
            requester: Some("Alice".into()),
            message_url: None,
            calls: vec![ToolCallRecord {
                name: "get_lore".into(),
                arguments: r#"{"user_ids":["1"]}"#.into(),
                result: result.into(),
            }],
            duration_ms: 10,
            outcome: Outcome::Answered {
                rounds: 2,
                totals: Totals::default(),
            },
        }
    }

    #[test]
    fn keeps_a_few_days_and_a_bounded_count() {
        let mut traces = ToolTraces::default();
        traces.record(trace(0, "old"));
        traces.record(trace(KEPT_SECS, "new"));
        assert_eq!(traces.traces.len(), 1);
        assert_eq!(traces.traces[0].calls[0].result, "new");

        for i in 0..KEPT_TRACES as u64 + 5 {
            traces.record(trace(KEPT_SECS + i, "x"));
        }
        assert_eq!(traces.traces.len(), KEPT_TRACES);
    }

    #[test]
    fn clamps_long_results() {
        let mut traces = ToolTraces::default();
        traces.record(trace(0, &"a".repeat(MAX_TEXT_CHARS + 50)));
        assert_eq!(
            traces.traces[0].calls[0].result.chars().count(),
            MAX_TEXT_CHARS
        );
    }

    #[test]
    fn outcome_is_tagged_for_the_web_page() {
        let json = serde_json::to_value(trace(0, "x")).unwrap();
        assert_eq!(json["outcome"]["type"], "answered");
        assert_eq!(json["outcome"]["rounds"], 2);
        let fell_back = Outcome::FellBack { error: "e".into() };
        assert_eq!(
            serde_json::to_value(fell_back).unwrap()["type"],
            "fell_back"
        );
    }
}
