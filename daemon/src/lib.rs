//! AgentRelay daemon core.
//!
//! This crate is the Rust source of truth for the orchestration logic. Where a
//! module has a `runner/src/**` counterpart in TypeScript, the two are kept in
//! lockstep: the TS version is the executable spec (tested there), and this is the
//! production implementation (tested here). See `docs/adr/0001-*`.

pub mod acp;
pub mod acp_adapter;
pub mod adapter;
pub mod calendar;
pub mod checkpoint;
pub mod claude_adapter;
pub mod claude_stream;
pub mod cloud;
pub mod codex;
pub mod context;
pub mod fake_adapter;
pub mod cost;
pub mod credential;
pub mod daemon;
pub mod drift;
pub mod error;
pub mod events;
pub mod fixloop;
pub mod guard;
pub mod hook;
pub mod ipc;
pub mod isolation;
pub mod keepawake;
pub mod mission;
pub mod process;
pub mod provider;
pub mod notify;
pub mod quota;
pub mod report;
pub mod send_gate;
pub mod templates;
pub mod validator;
pub mod which;
pub mod state;
pub mod store;

pub use error::{AgentRelayError, ErrorCode, Result};
pub use events::{AgentEvent, EnvelopedEvent, Provenance, QuotaWindow, Usage};
pub use provider::{Capabilities, Guard, GuardKind, QuotaState, QuotaWindowState};
