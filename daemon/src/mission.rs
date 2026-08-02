//! MissionRunner — the supervision loop that ties the pieces together (plan 00
//! §6/§7/§9). It drives an adapter through one turn: enforces Watchtower layer-1
//! guards on every `tool_pre` (interrupting the turn on a violation), tracks quota,
//! plans a resume on a rate-limit, and appends every event to the store. This is the
//! first place the whole daemon works end-to-end; tested with the fake adapter.

use crate::adapter::ProviderAdapter;
use crate::events::AgentEvent;
use crate::guard::{derive_actions, evaluate_guards, GuardViolation};
use crate::provider::{Guard, SessionHandle, TurnInput};
use crate::quota::{plan_resume_after_rate_limit, QuotaTracker, ResumePlan};
use crate::store::{AppendEvent, Store};
use futures::StreamExt;

#[derive(Debug, Default, serde::Serialize)]
pub struct TurnOutcome {
    /// Number of normalized events observed.
    pub events_seen: usize,
    /// Guard violations that fired on tool_pre events.
    pub violations: Vec<GuardViolation>,
    /// Set when the turn hit a provider rate-limit.
    pub resume_plan: Option<ResumePlan>,
    /// Tokens used this turn (from usage events).
    pub used_tokens: u64,
    /// The turn reached turn_completed.
    pub completed: bool,
    /// We interrupted the turn (a guard blocked an action).
    pub interrupted: bool,
}

fn event_kind(e: &AgentEvent) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

/// Drive one supervised turn. `now_utc` is the daemon clock (for the resume plan).
pub async fn run_turn_supervised(
    adapter: &dyn ProviderAdapter,
    handle: &SessionHandle,
    input: TurnInput,
    guards: &[Guard],
    store: &Store,
    mission_id: &str,
    now_utc: &str,
) -> crate::error::Result<TurnOutcome> {
    let mut outcome = TurnOutcome::default();
    let mut tracker = QuotaTracker::new(adapter.id());
    let cwd = handle.cwd.clone();

    let mut stream = adapter.run_turn(handle, input).await?;
    while let Some(event) = stream.next().await {
        outcome.events_seen += 1;
        // Persist to the append-only log (best-effort; a store error doesn't abort).
        let _ = store.append_event(&AppendEvent {
            mission_id: Some(mission_id.to_string()),
            actor: adapter.id().to_string(),
            kind: event_kind(&event),
            payload: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
            seq_in_turn: Some(outcome.events_seen as i64 - 1),
        });

        match &event {
            AgentEvent::ToolPre { tool, args } => {
                let decision = evaluate_guards(&derive_actions(tool, args), guards, &cwd);
                if !decision.allow {
                    // Record WHAT was refused, not just that the turn stopped. A
                    // guard that fires silently leaves the morning with an
                    // unexplained halt: the report's whole job is to say why, and
                    // "the agent tried to push / write outside the workspace" is the
                    // single most important thing a night can tell you.
                    for v in &decision.violations {
                        let _ = store.append_event(&AppendEvent {
                            mission_id: Some(mission_id.to_string()),
                            actor: "watchtower".to_string(),
                            kind: "guard.violation".to_string(),
                            payload: serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                            seq_in_turn: Some(outcome.events_seen as i64 - 1),
                        });
                    }
                    outcome.violations.extend(decision.violations);
                    // A blocked action stops the turn (Watchtower layer-1).
                    let _ = adapter.interrupt(handle).await;
                    outcome.interrupted = true;
                    break;
                }
            }
            AgentEvent::TokenUsage { usage } | AgentEvent::TurnCompleted { usage } => {
                outcome.used_tokens += usage.used();
                let _ = tracker.ingest(&event);
                if matches!(event, AgentEvent::TurnCompleted { .. }) {
                    outcome.completed = true;
                }
            }
            // The agent confirming a call never ran. Recorded under its own kind so
            // the report can say "the provider refused it" rather than only "we
            // told it no" — the difference matters when you are deciding in the
            // morning whether a guard actually held.
            AgentEvent::ToolDenied { tool, .. } => {
                let _ = store.append_event(&AppendEvent {
                    mission_id: Some(mission_id.to_string()),
                    actor: "provider".to_string(),
                    kind: "tool.denied".to_string(),
                    payload: serde_json::json!({ "tool": tool }),
                    seq_in_turn: Some(outcome.events_seen as i64 - 1),
                });
            }
            // Healthy heartbeat: record where the wall is and carry on. Acting on it
            // would abort a turn that has nothing wrong with it.
            AgentEvent::QuotaHint { reset_at, window, provider } => {
                let _ = store.save_quota_snapshot(
                    provider.as_deref().unwrap_or("claude"),
                    &window.map(|w| format!("{w:?}")).unwrap_or_else(|| "unknown".into()),
                    reset_at,
                );
            }
            AgentEvent::RateLimited { reset_at, .. } => {
                outcome.resume_plan =
                    Some(plan_resume_after_rate_limit(reset_at, now_utc, None, None));
                break;
            }
            _ => {}
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Usage, QuotaWindow};
    use crate::fake_adapter::FakeAdapter;
    use crate::provider::GuardKind;
    use serde_json::json;

    fn handle() -> SessionHandle {
        SessionHandle { id: "s".into(), external_id: None, provider: "fake".into(), cwd: "/home/u/proj".into() }
    }
    fn usage() -> Usage {
        Usage { input_tokens: 100, output_tokens: 40, ..Usage::zero() }
    }

    #[tokio::test]
    async fn clean_turn_completes_accumulates_and_logs() {
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::ToolPre { tool: "Bash".into(), args: json!({"command": "npm test"}) },
            AgentEvent::TurnCompleted { usage: usage() },
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let store = Store::open_in_memory().unwrap();
        let guards = vec![Guard::new(GuardKind::NoPush), Guard::new(GuardKind::WorkspaceBoundary)];
        let out = run_turn_supervised(&fake, &handle(), TurnInput::default(), &guards, &store, "m1", "2026-07-27T21:00:00Z").await.unwrap();
        assert!(out.completed);
        assert!(!out.interrupted);
        assert!(out.violations.is_empty());
        assert_eq!(out.used_tokens, 140);
        // every event was appended to the log
        assert_eq!(store.get_events("m1").unwrap().len(), 3);
    }

    #[tokio::test]
    async fn guard_violation_interrupts_the_turn() {
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::ToolPre { tool: "Write".into(), args: json!({"file_path": "/etc/passwd"}) },
            AgentEvent::TurnCompleted { usage: usage() }, // should never be reached
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let store = Store::open_in_memory().unwrap();
        let guards = vec![Guard::new(GuardKind::WorkspaceBoundary)];
        let out = run_turn_supervised(&fake, &handle(), TurnInput::default(), &guards, &store, "m1", "2026-07-27T21:00:00Z").await.unwrap();
        assert!(out.interrupted);
        assert!(!out.completed);
        assert_eq!(out.violations.len(), 1);
        assert_eq!(out.violations[0].guard, GuardKind::WorkspaceBoundary);
        assert_eq!(fake.interrupt_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_quota_hint_is_recorded_without_stopping_the_turn() {
        // Claude emits this on every turn. If it were treated as the wall, every
        // healthy run would abort at its first event.
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::QuotaHint {
                reset_at: "2026-08-03T04:00:00+00:00".into(),
                window: Some(QuotaWindow::FiveHour),
                provider: Some("claude".into()),
            },
            AgentEvent::TurnCompleted { usage: usage() },
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let store = Store::open_in_memory().unwrap();
        let out = run_turn_supervised(&fake, &handle(), TurnInput::default(), &[], &store, "m1", "2026-08-02T00:00:00Z").await.unwrap();

        assert!(out.completed, "the turn must run to completion");
        assert!(out.resume_plan.is_none(), "a hint is not a rate limit");
        assert_eq!(
            store.latest_quota_reset("claude").as_deref(),
            Some("2026-08-03T04:00:00+00:00"),
            "and the wall's location is remembered"
        );
    }

    #[tokio::test]
    async fn rate_limit_produces_a_resume_plan() {
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::RateLimited { reset_at: "2026-07-27T22:00:00.000Z".into(), window: Some(QuotaWindow::FiveHour), provider: None },
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let store = Store::open_in_memory().unwrap();
        let out = run_turn_supervised(&fake, &handle(), TurnInput::default(), &[], &store, "m1", "2026-07-27T21:00:00.000Z").await.unwrap();
        let plan = out.resume_plan.expect("resume plan");
        assert!(plan.wait_ms > 0);
        assert!(!out.completed);
    }
}
