//! A deterministic scripted ProviderAdapter — Rust mirror of the TS fake. Burns no
//! quota and reproduces the hard paths (rate-limit mid-turn, session GC on resume,
//! interrupt mid-stream) so the orchestration logic is testable end-to-end.

use crate::adapter::{EventStream, ProviderAdapter};
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::events::{AgentEvent, Usage};
use crate::provider::{Capabilities, Guard, QuotaState, SessionHandle, SessionSpec, TurnInput};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeBehavior {
    Ok,
    GarbageCollected,
    NotFound,
}

pub struct FakeAdapter {
    id: String,
    turns: Mutex<VecDeque<Vec<AgentEvent>>>,
    caps: Capabilities,
    quota: QuotaState,
    resume_behavior: ResumeBehavior,
    interrupted: Arc<AtomicBool>,
    handle_counter: AtomicUsize,
    pub installed_guards: Mutex<Vec<Guard>>,
    pub steers: Mutex<Vec<String>>,
    pub interrupt_count: AtomicUsize,
}

impl FakeAdapter {
    pub fn new(id: impl Into<String>, turns: Vec<Vec<AgentEvent>>) -> Self {
        let id = id.into();
        FakeAdapter {
            quota: QuotaState {
                provider: id.clone(),
                windows: vec![],
            },
            id,
            turns: Mutex::new(turns.into()),
            caps: Capabilities {
                resume: true,
                interrupt: true,
                steer: true,
                guards: true,
                fork_session: false,
                transform_tool_result: true,
            },
            resume_behavior: ResumeBehavior::Ok,
            interrupted: Arc::new(AtomicBool::new(false)),
            handle_counter: AtomicUsize::new(0),
            installed_guards: Mutex::new(vec![]),
            steers: Mutex::new(vec![]),
            interrupt_count: AtomicUsize::new(0),
        }
    }

    pub fn with_resume_behavior(mut self, b: ResumeBehavior) -> Self {
        self.resume_behavior = b;
        self
    }
}

#[async_trait]
impl ProviderAdapter for FakeAdapter {
    fn id(&self) -> &str {
        &self.id
    }
    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn start_session(&self, spec: SessionSpec) -> Result<SessionHandle> {
        let n = self.handle_counter.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(SessionHandle {
            id: format!("{}-sess-{}", self.id, n),
            external_id: Some(format!("{}-ext-{}", self.id, n)),
            provider: self.id.clone(),
            cwd: spec.cwd,
        })
    }

    async fn resume_session(&self, external_id: &str, spec: SessionSpec) -> Result<SessionHandle> {
        match self.resume_behavior {
            ResumeBehavior::GarbageCollected => Err(AgentRelayError::new(
                ErrorCode::SessionGarbageCollected,
                format!("session {external_id} was garbage-collected"),
            )),
            ResumeBehavior::NotFound => Err(AgentRelayError::new(
                ErrorCode::SessionNotFound,
                format!("session {external_id} not found"),
            )),
            ResumeBehavior::Ok => {
                let n = self.handle_counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(SessionHandle {
                    id: format!("{}-sess-{}", self.id, n),
                    external_id: Some(external_id.to_string()),
                    provider: self.id.clone(),
                    cwd: spec.cwd,
                })
            }
        }
    }

    async fn run_turn(&self, _handle: &SessionHandle, _input: TurnInput) -> Result<EventStream> {
        let events = self.turns.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![AgentEvent::TurnCompleted {
                usage: Usage::zero(),
            }]
        });
        let interrupted = self.interrupted.clone();
        interrupted.store(false, Ordering::SeqCst);
        let stream = async_stream::stream! {
            for e in events {
                if interrupted.load(Ordering::SeqCst) {
                    break; // a real interrupt stops the turn mid-stream
                }
                yield e;
            }
        };
        Ok(Box::pin(stream))
    }

    async fn steer(&self, _handle: &SessionHandle, message: &str) -> Result<()> {
        self.steers.lock().unwrap().push(message.to_string());
        Ok(())
    }

    async fn interrupt(&self, _handle: &SessionHandle) -> Result<()> {
        self.interrupted.store(true, Ordering::SeqCst);
        self.interrupt_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn quota(&self) -> Result<QuotaState> {
        Ok(self.quota.clone())
    }

    async fn install_guards(&self, _handle: &SessionHandle, guards: &[Guard]) -> Result<()> {
        self.installed_guards
            .lock()
            .unwrap()
            .extend_from_slice(guards);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::GuardKind;
    use futures::StreamExt;

    fn usage() -> Usage {
        Usage::zero()
    }
    fn handle() -> SessionHandle {
        SessionHandle {
            id: "s".into(),
            external_id: None,
            provider: "fake".into(),
            cwd: "/w".into(),
        }
    }

    #[tokio::test]
    async fn yields_scripted_turns_in_order() {
        let t1 = vec![
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta { text: "a".into() },
            AgentEvent::TurnCompleted { usage: usage() },
        ];
        let fake = FakeAdapter::new("fake", vec![t1.clone()]);
        let got: Vec<_> = fake
            .run_turn(&handle(), TurnInput::default())
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(got, t1);
        // exhausted -> default completed
        let got2: Vec<_> = fake
            .run_turn(&handle(), TurnInput::default())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(matches!(
            got2.last(),
            Some(AgentEvent::TurnCompleted { .. })
        ));
    }

    #[tokio::test]
    async fn interrupt_stops_stream_mid_turn() {
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta { text: "one".into() },
            AgentEvent::TextDelta { text: "two".into() },
            AgentEvent::TurnCompleted { usage: usage() },
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let mut stream = fake
            .run_turn(&handle(), TurnInput::default())
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(e) = stream.next().await {
            let stop = matches!(&e, AgentEvent::TextDelta { text } if text == "one");
            seen.push(e);
            if stop {
                fake.interrupt(&handle()).await.unwrap();
            }
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(fake.interrupt_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resume_behaviors_surface_errors() {
        let spec = SessionSpec {
            cwd: "/w".into(),
            ..Default::default()
        };
        assert!(FakeAdapter::new("f", vec![])
            .resume_session("ext", spec.clone())
            .await
            .is_ok());
        let gc =
            FakeAdapter::new("f", vec![]).with_resume_behavior(ResumeBehavior::GarbageCollected);
        assert_eq!(
            gc.resume_session("ext", spec.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionGarbageCollected
        );
        let nf = FakeAdapter::new("f", vec![]).with_resume_behavior(ResumeBehavior::NotFound);
        assert_eq!(
            nf.resume_session("ext", spec).await.unwrap_err().code,
            ErrorCode::SessionNotFound
        );
    }

    #[tokio::test]
    async fn records_guards_and_steers() {
        let fake = FakeAdapter::new("f", vec![]);
        fake.install_guards(
            &handle(),
            &[
                Guard::new(GuardKind::NoPush),
                Guard::new(GuardKind::ReadOnly),
            ],
        )
        .await
        .unwrap();
        fake.steer(&handle(), "focus").await.unwrap();
        assert_eq!(fake.installed_guards.lock().unwrap().len(), 2);
        assert_eq!(fake.steers.lock().unwrap().as_slice(), ["focus"]);
    }
}
