//! The normalized event stream every provider adapter emits into — the Rust mirror
//! of `runner/src/contracts/events.ts`. Mission Engine / Watchtower / Timeline /
//! Quota all speak only in these, so adding a provider is one adapter and nothing
//! else moves.

use serde::{Deserialize, Serialize};

/// Where a surfaced value came from (plan 02 §ב): Measured = we counted it,
/// Estimated = a model/heuristic produced it, Provider = straight from the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    Measured,
    Estimated,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    pub provenance: Provenance,
}

impl Usage {
    pub fn zero() -> Self {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            total_tokens: None,
            provenance: Provenance::Measured,
        }
    }
    /// Total used, preferring the provider total, else input+output.
    pub fn used(&self) -> u64 {
        self.total_tokens
            .unwrap_or(self.input_tokens + self.output_tokens)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaWindow {
    #[serde(rename = "5h")]
    FiveHour,
    Weekly,
    Context,
}

/// One normalized event. Tagged on `type` to match the TS discriminated union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted,
    TextDelta {
        text: String,
    },
    ToolPre {
        tool: String,
        args: serde_json::Value,
    },
    ToolPost {
        tool: String,
        result: serde_json::Value,
    },
    FileEdited {
        path: String,
    },
    CmdRun {
        cmd: String,
    },
    PermissionRequested {
        action: String,
    },
    TokenUsage {
        usage: Usage,
    },
    TurnCompleted {
        usage: Usage,
    },
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    RateLimited {
        reset_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        window: Option<QuotaWindow>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
}

/// A persisted/replayed stream item: the event plus ordering metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopedEvent {
    pub seq: u64,
    pub ts: String,
    pub event: AgentEvent,
}
