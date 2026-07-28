//! Structured error taxonomy — the Rust mirror of `runner/src/contracts/errors.ts`.
//! The daemon emits stable CODES (dotted `<domain>.<condition>`), not localized
//! prose; the UI maps a code to a string.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    QuotaRateLimited,
    QuotaUsageLimitExceeded,
    SessionResumeFailed,
    SessionNotFound,
    SessionGarbageCollected,
    ProcessStopIncomplete,
    ProcessSpawnFailed,
    GuardBlocked,
    GuardReadonlyViolation,
    CheckpointHeadForeign,
    CheckpointSaveFailed,
    ProviderStreamError,
    ProviderProtocolMismatch,
    ProviderUnavailable,
    CredentialUnavailable,
    IsolationSetupFailed,
    Internal,
}

impl ErrorCode {
    /// The stable string form used in logs and the JSON contract.
    pub fn as_str(&self) -> &'static str {
        use ErrorCode::*;
        match self {
            QuotaRateLimited => "quota.rate_limited",
            QuotaUsageLimitExceeded => "quota.usage_limit_exceeded",
            SessionResumeFailed => "session.resume_failed",
            SessionNotFound => "session.not_found",
            SessionGarbageCollected => "session.garbage_collected",
            ProcessStopIncomplete => "process.stop_incomplete",
            ProcessSpawnFailed => "process.spawn_failed",
            GuardBlocked => "guard.blocked",
            GuardReadonlyViolation => "guard.readonly_violation",
            CheckpointHeadForeign => "checkpoint.head_foreign",
            CheckpointSaveFailed => "checkpoint.save_failed",
            ProviderStreamError => "provider.stream_error",
            ProviderProtocolMismatch => "provider.protocol_mismatch",
            ProviderUnavailable => "provider.unavailable",
            CredentialUnavailable => "credential.unavailable",
            IsolationSetupFailed => "isolation.setup_failed",
            Internal => "internal",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The daemon error type: a stable code, a human (English) message, and whether a
/// resume/retry is sensible. `reset_at` is populated for quota errors from the
/// provider.
#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct AgentRelayError {
    pub code: ErrorCode,
    pub message: String,
    pub retriable: bool,
    pub reset_at: Option<String>,
}

impl AgentRelayError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AgentRelayError {
            code,
            message: message.into(),
            retriable: false,
            reset_at: None,
        }
    }
    pub fn retriable(mut self, v: bool) -> Self {
        self.retriable = v;
        self
    }
    pub fn reset_at(mut self, at: impl Into<String>) -> Self {
        self.reset_at = Some(at.into());
        self
    }
}

pub type Result<T> = std::result::Result<T, AgentRelayError>;
