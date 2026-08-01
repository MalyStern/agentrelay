//! AgentRelay daemon core.
//!
//! This crate is the Rust source of truth for the orchestration logic. Where a
//! module has a `runner/src/**` counterpart in TypeScript, the two are kept in
//! lockstep: the TS version is the executable spec (tested there), and this is the
//! production implementation (tested here). See `docs/adr/0001-*`.

pub mod adapter;
pub mod claude_adapter;
pub mod claude_stream;
pub mod fake_adapter;
pub mod credential;
pub mod daemon;
pub mod error;
pub mod events;
pub mod guard;
pub mod keepawake;
pub mod mission;
pub mod process;
pub mod provider;
pub mod quota;
pub mod send_gate;
pub mod store;

pub use error::{AgentRelayError, ErrorCode, Result};
pub use events::{AgentEvent, EnvelopedEvent, Provenance, QuotaWindow, Usage};
pub use provider::{Capabilities, Guard, GuardKind, QuotaState, QuotaWindowState};
