//! Quota Manager — the central feature (plan 00 §9). Rust port of
//! `runner/src/quota/quota-manager.ts`, including the QA fixes (NaN-safe reset math
//! with a fallback, first-turn read-only enforcement). Detection is REACTIVE: we
//! learn we're limited from the provider's own `reset_at`, never a guess.

use crate::events::{AgentEvent, QuotaWindow};
use crate::provider::{Guard, GuardKind};
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use std::collections::HashMap;

/// 5h fallback when the provider gives no / a malformed reset time.
pub const DEFAULT_RESET_MS: i64 = 5 * 60 * 60 * 1000;

fn to_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.timestamp_millis())
}
fn to_iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub enum QuotaSignal {
    Usage { used: u64 },
    RateLimited { reset_at: String, window: QuotaWindow },
}

pub fn interpret_event(event: &AgentEvent) -> Option<QuotaSignal> {
    match event {
        AgentEvent::TokenUsage { usage } | AgentEvent::TurnCompleted { usage } => {
            Some(QuotaSignal::Usage { used: usage.used() })
        }
        AgentEvent::RateLimited { reset_at, window, .. } => Some(QuotaSignal::RateLimited {
            reset_at: reset_at.clone(),
            window: window.unwrap_or(QuotaWindow::FiveHour),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResumePlan {
    pub resume_at_utc: String,
    pub wait_ms: i64,
    pub buffer_ms: i64,
}

/// Plan the resume from the provider's reset + a safety buffer. Never panics/NaNs on
/// a malformed reset — falls back to now + `fallback_ms`.
pub fn plan_resume_after_rate_limit(
    reset_at_utc: &str,
    now_utc: &str,
    buffer_ms: Option<i64>,
    fallback_ms: Option<i64>,
) -> ResumePlan {
    let buffer_ms = buffer_ms.unwrap_or(60_000);
    let now = to_ms(now_utc).unwrap_or(0);
    let reset = to_ms(reset_at_utc).unwrap_or_else(|| now + fallback_ms.unwrap_or(DEFAULT_RESET_MS));
    let resume_at = reset + buffer_ms;
    ResumePlan {
        resume_at_utc: to_iso(resume_at),
        wait_ms: (resume_at - now).max(0),
        buffer_ms,
    }
}

/// What to do with a unit given how much of the window is left.
#[derive(Debug, Clone, PartialEq)]
pub enum StartDecision {
    /// There is room. Go.
    Start,
    /// The window closes before this unit could finish. Waiting until it reopens
    /// costs less than starting work that will be cut in half.
    WaitUntilReset { resume_at_utc: String, wait_ms: i64 },
    /// We have no idea where the wall is, so we cannot reason about it. Start —
    /// refusing to work because we lack information would be worse than the risk.
    StartUnknownWindow,
}

/// Decide whether a unit fits in what remains of the quota window.
///
/// Only possible because the provider reports `resetsAt` on healthy turns; the
/// plan assumed it did not, so this decision could not previously be made at all
/// and every unit was started blind. Being cut off mid-unit is the expensive case:
/// the work is half-done, unvalidated, and uncheckpointed, and resuming means
/// re-establishing context the new session no longer has.
pub fn decide_start(
    reset_at_utc: Option<&str>,
    now_utc: &str,
    estimated_unit_ms: i64,
    buffer_ms: Option<i64>,
) -> StartDecision {
    let buffer_ms = buffer_ms.unwrap_or(60_000);
    let Some(reset) = reset_at_utc.and_then(to_ms) else {
        return StartDecision::StartUnknownWindow;
    };
    let Some(now) = to_ms(now_utc) else {
        return StartDecision::StartUnknownWindow;
    };

    let remaining = reset - now;
    // Already past the reset: the window has reopened, nothing to wait for.
    if remaining <= 0 {
        return StartDecision::Start;
    }
    if remaining >= estimated_unit_ms {
        return StartDecision::Start;
    }
    let resume_at = reset + buffer_ms;
    StartDecision::WaitUntilReset {
        resume_at_utc: to_iso(resume_at),
        wait_ms: (resume_at - now).max(0),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifyDeadline {
    pub verify_by_utc: String,
    pub verify_now: bool,
}

/// When to verify the session still exists — BEFORE the reset window (fix #7).
pub fn session_verify_deadline(
    reset_at_utc: &str,
    now_utc: &str,
    lead_ms: Option<i64>,
    fallback_ms: Option<i64>,
) -> VerifyDeadline {
    let lead_ms = lead_ms.unwrap_or(5 * 60_000);
    let now = to_ms(now_utc).unwrap_or(0);
    let reset = to_ms(reset_at_utc).unwrap_or_else(|| now + fallback_ms.unwrap_or(DEFAULT_RESET_MS));
    let verify_by = reset - lead_ms;
    VerifyDeadline {
        verify_by_utc: to_iso(verify_by),
        verify_now: verify_by <= now,
    }
}

/// The first turn after a resume must be read-only until a checkpoint is approved
/// (fix #5) — enforced in CODE via a guard, not a prompt.
pub fn first_turn_read_only_guards() -> Vec<Guard> {
    vec![Guard::new(GuardKind::ReadOnly)]
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSnapshot {
    pub kind: QuotaWindow,
    pub used: Option<u64>,
    pub reset_at_utc: Option<String>,
}

/// Fold events into per-window quota snapshots. Usage counts are additive.
#[derive(Default)]
pub struct QuotaTracker {
    provider: String,
    windows: HashMap<QuotaWindow, WindowSnapshot>,
}

impl QuotaTracker {
    pub fn new(provider: impl Into<String>) -> Self {
        QuotaTracker {
            provider: provider.into(),
            windows: HashMap::new(),
        }
    }

    pub fn ingest(&mut self, event: &AgentEvent) -> Option<QuotaSignal> {
        let sig = interpret_event(event)?;
        match &sig {
            QuotaSignal::Usage { used } => {
                let w = self.windows.entry(QuotaWindow::FiveHour).or_insert(WindowSnapshot {
                    kind: QuotaWindow::FiveHour,
                    used: Some(0),
                    reset_at_utc: None,
                });
                w.used = Some(w.used.unwrap_or(0) + used);
            }
            QuotaSignal::RateLimited { reset_at, window } => {
                self.windows.insert(
                    *window,
                    WindowSnapshot {
                        kind: *window,
                        used: None,
                        reset_at_utc: Some(reset_at.clone()),
                    },
                );
            }
        }
        Some(sig)
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn window(&self, kind: QuotaWindow) -> Option<&WindowSnapshot> {
        self.windows.get(&kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Provenance, Usage};

    fn usage(i: u64, o: u64) -> Usage {
        Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            total_tokens: None,
            provenance: Provenance::Provider,
        }
    }

    const HOUR: i64 = 3_600_000;

    #[test]
    fn a_unit_that_fits_in_the_window_starts() {
        assert_eq!(
            decide_start(Some("2026-08-02T04:00:00Z"), "2026-08-02T01:00:00Z", HOUR, None),
            StartDecision::Start,
            "three hours left, one hour of work"
        );
    }

    #[test]
    fn a_unit_that_would_be_cut_in_half_waits_instead() {
        // The expensive failure: work stopped mid-unit is unvalidated and
        // uncheckpointed, and resuming means rebuilding context a fresh session
        // does not have. Waiting twenty minutes beats that.
        match decide_start(Some("2026-08-02T04:00:00Z"), "2026-08-02T03:40:00Z", HOUR, None) {
            StartDecision::WaitUntilReset { resume_at_utc, wait_ms } => {
                assert!(resume_at_utc.starts_with("2026-08-02T04:01"), "{resume_at_utc}");
                assert!(wait_ms > 20 * 60_000 && wait_ms < 22 * 60_000, "{wait_ms}");
            }
            other => panic!("expected a wait, got {other:?}"),
        }
    }

    #[test]
    fn a_window_that_has_already_reopened_is_not_waited_for() {
        assert_eq!(
            decide_start(Some("2026-08-02T01:00:00Z"), "2026-08-02T03:00:00Z", HOUR, None),
            StartDecision::Start
        );
    }

    #[test]
    fn not_knowing_where_the_wall_is_means_start_not_stop() {
        // Refusing to work for lack of information would be a worse failure than
        // occasionally being interrupted.
        assert_eq!(
            decide_start(None, "2026-08-02T01:00:00Z", HOUR, None),
            StartDecision::StartUnknownWindow
        );
        assert_eq!(
            decide_start(Some("not a timestamp"), "2026-08-02T01:00:00Z", HOUR, None),
            StartDecision::StartUnknownWindow
        );
    }

    #[test]
    fn interpret() {
        assert_eq!(
            interpret_event(&AgentEvent::TokenUsage { usage: usage(100, 40) }),
            Some(QuotaSignal::Usage { used: 140 })
        );
        assert_eq!(
            interpret_event(&AgentEvent::RateLimited {
                reset_at: "2026-07-27T22:00:00Z".into(),
                window: Some(QuotaWindow::Weekly),
                provider: None
            }),
            Some(QuotaSignal::RateLimited {
                reset_at: "2026-07-27T22:00:00Z".into(),
                window: QuotaWindow::Weekly
            })
        );
        assert_eq!(interpret_event(&AgentEvent::TurnStarted), None);
    }

    #[test]
    fn plan_resume_waits_until_reset_plus_buffer() {
        let p = plan_resume_after_rate_limit("2026-07-27T22:00:00.000Z", "2026-07-27T21:00:00.000Z", Some(60_000), None);
        assert_eq!(p.wait_ms, 61 * 60_000);
        assert_eq!(p.resume_at_utc, "2026-07-27T22:01:00.000Z");
    }

    #[test]
    fn plan_resume_never_negative() {
        let p = plan_resume_after_rate_limit("2026-07-27T20:00:00.000Z", "2026-07-27T21:00:00.000Z", None, None);
        assert_eq!(p.wait_ms, 0);
    }

    #[test]
    fn plan_resume_falls_back_on_malformed_reset() {
        let p = plan_resume_after_rate_limit("not-a-date", "2026-07-27T21:00:00.000Z", None, Some(3_600_000));
        assert_eq!(p.wait_ms, 3_600_000 + 60_000);
        assert!(!p.resume_at_utc.is_empty());
    }

    #[test]
    fn verify_deadline_before_window() {
        let d = session_verify_deadline("2026-07-27T22:00:00.000Z", "2026-07-27T21:00:00.000Z", Some(5 * 60_000), None);
        assert_eq!(d.verify_by_utc, "2026-07-27T21:55:00.000Z");
        assert!(!d.verify_now);
        let d2 = session_verify_deadline("2026-07-27T22:00:00.000Z", "2026-07-27T21:58:00.000Z", Some(5 * 60_000), None);
        assert!(d2.verify_now);
    }

    #[test]
    fn first_turn_is_read_only() {
        assert_eq!(first_turn_read_only_guards(), vec![Guard::new(GuardKind::ReadOnly)]);
    }

    #[test]
    fn tracker_accumulates_and_records_reset() {
        let mut t = QuotaTracker::new("claude");
        t.ingest(&AgentEvent::TokenUsage { usage: usage(100, 40) });
        t.ingest(&AgentEvent::TurnCompleted { usage: usage(100, 40) });
        t.ingest(&AgentEvent::RateLimited {
            reset_at: "2026-07-27T22:00:00Z".into(),
            window: Some(QuotaWindow::Weekly),
            provider: None,
        });
        assert_eq!(t.window(QuotaWindow::FiveHour).unwrap().used, Some(280));
        assert_eq!(
            t.window(QuotaWindow::Weekly).unwrap().reset_at_utc.as_deref(),
            Some("2026-07-27T22:00:00Z")
        );
    }
}
