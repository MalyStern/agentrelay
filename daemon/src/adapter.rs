//! The provider contract as an async trait — Rust mirror of the `ProviderAdapter`
//! interface in `runner/src/contracts/provider.ts`. Every engine (Claude SDK, Codex
//! app-server, ACP, Cloud-delegated) implements this; the rest of the daemon speaks
//! only in normalized `AgentEvent`s, so adding a provider is one adapter.

use crate::error::Result;
use crate::events::AgentEvent;
use crate::provider::{Capabilities, Guard, QuotaState, SessionHandle, SessionSpec, TurnInput};
use async_trait::async_trait;
use futures::stream::BoxStream;

/// A turn's event stream.
pub type EventStream = BoxStream<'static, AgentEvent>;

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;

    async fn start_session(&self, spec: SessionSpec) -> Result<SessionHandle>;
    async fn resume_session(&self, external_id: &str, spec: SessionSpec) -> Result<SessionHandle>;

    /// Run one turn; yields normalized events until the turn completes or errors.
    async fn run_turn(&self, handle: &SessionHandle, input: TurnInput) -> Result<EventStream>;

    async fn steer(&self, handle: &SessionHandle, message: &str) -> Result<()>;
    /// Stop the current turn AND its process tree.
    async fn interrupt(&self, handle: &SessionHandle) -> Result<()>;

    async fn quota(&self) -> Result<QuotaState>;

    async fn install_guards(&self, handle: &SessionHandle, guards: &[Guard]) -> Result<()>;
}
