//! Provider contract types — the Rust mirror of `runner/src/contracts/provider.ts`.
//! The async `ProviderAdapter` trait itself lands with the first real adapter; these
//! are the value types the pure logic (guards, quota) already needs.

use crate::events::{Provenance, QuotaWindow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub resume: bool,
    pub interrupt: bool,
    pub steer: bool,
    pub guards: bool,
    pub fork_session: bool,
    /// Can a tool RESULT be replaced before the model sees it? (kill-test A)
    pub transform_tool_result: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindowState {
    pub kind: QuotaWindow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaState {
    pub provider: String,
    pub windows: Vec<QuotaWindowState>,
}

/// Guard kinds mirror policies.rule_kind (00 §4) + the enforcement table (02 §ז.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardKind {
    ForbidPath,
    AllowedPaths,
    ForbidCmd,
    NoPush,
    NoDeploy,
    NoNet,
    NoDependency,
    WorkspaceBoundary,
    ReadOnly,
    AllowOnlyBranch,
    MaxFiles,
    MaxCost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guard {
    pub kind: GuardKind,
    /// kind-specific spec (a glob, list of globs, a branch, a cap, a regex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_json::Value>,
}

impl Guard {
    pub fn new(kind: GuardKind) -> Self {
        Guard { kind, spec: None }
    }
    pub fn with_spec(kind: GuardKind, spec: serde_json::Value) -> Self {
        Guard {
            kind,
            spec: Some(spec),
        }
    }
}
