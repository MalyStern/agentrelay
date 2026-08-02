//! Scheduler send-gate (plan 00 §10) — Rust port of `runner/src/scheduler/send-
//! gate.ts`. The check every queued prompt passes right before send; what stops a
//! stale prompt firing after the mission moved on. Pure. Fails closed on a malformed
//! `valid_until`; the TS `NaN missionVer` hazard can't occur here (mission_ver is a
//! typed integer).

use chrono::DateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    At { at: String },
    After { since_utc: String, after_ms: i64 },
    OnEvent { event: String },
    Cron { expr: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptStatus {
    Queued,
    Sent,
    Expired,
    Cancelled,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledPrompt {
    pub id: String,
    pub target: String,
    pub trigger: Trigger,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<String>,
    pub priority: i32,
    pub mission_ver: u32,
    pub status: PromptStatus,
}

#[derive(Debug, Clone, Default)]
pub struct SendContext {
    pub now_utc: String,
    pub current_mission_ver: u32,
    pub condition_result: Option<bool>,
    pub fired_event: Option<String>,
    pub cron_due: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendVerdict {
    Send,
    NotDue,
    Expired,
    Superseded,
    ConditionUnmet,
    Cancelled,
    AlreadySent,
}

fn to_ms(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|d| d.timestamp_millis())
}

pub fn is_trigger_due(trigger: &Trigger, ctx: &SendContext) -> bool {
    match trigger {
        Trigger::At { at } => match (to_ms(&ctx.now_utc), to_ms(at)) {
            (Some(n), Some(a)) => n >= a,
            _ => false,
        },
        Trigger::After { since_utc, after_ms } => match (to_ms(&ctx.now_utc), to_ms(since_utc)) {
            (Some(n), Some(s)) => n >= s + after_ms,
            _ => false,
        },
        Trigger::OnEvent { event } => ctx.fired_event.as_deref() == Some(event.as_str()),
        Trigger::Cron { .. } => ctx.cron_due == Some(true),
    }
}

/// The gate. Order: terminal statuses -> expiry -> staleness -> trigger -> condition.
pub fn evaluate_send_gate(prompt: &ScheduledPrompt, ctx: &SendContext) -> SendVerdict {
    match prompt.status {
        PromptStatus::Cancelled => return SendVerdict::Cancelled,
        PromptStatus::Superseded => return SendVerdict::Superseded,
        PromptStatus::Expired => return SendVerdict::Expired,
        PromptStatus::Sent => return SendVerdict::AlreadySent,
        PromptStatus::Queued => {}
    }
    if let Some(vu) = &prompt.valid_until {
        match to_ms(vu) {
            None => return SendVerdict::Expired, // malformed -> fail closed
            Some(v) => {
                if to_ms(&ctx.now_utc).unwrap_or(0) > v {
                    return SendVerdict::Expired;
                }
            }
        }
    }
    if prompt.mission_ver < ctx.current_mission_ver {
        return SendVerdict::Superseded;
    }
    if !is_trigger_due(&prompt.trigger, ctx) {
        return SendVerdict::NotDue;
    }
    // `!= Some(true)`, not `== Some(false)`: a condition that exists but was never
    // evaluated leaves `None`, and treating that as "send" means the gate opens
    // precisely when we failed to check it. The rest of this module fails closed;
    // this line used not to.
    if prompt.condition.is_some() && ctx.condition_result != Some(true) {
        return SendVerdict::ConditionUnmet;
    }
    SendVerdict::Send
}

/// Highest-priority sendable prompt; ties break by insertion order (first wins).
pub fn select_next_to_send<'a>(
    prompts: &'a [ScheduledPrompt],
    ctx: &SendContext,
) -> Option<&'a ScheduledPrompt> {
    let mut best: Option<&ScheduledPrompt> = None;
    for p in prompts {
        if evaluate_send_gate(p, ctx) == SendVerdict::Send {
            match best {
                None => best = Some(p),
                Some(b) if p.priority > b.priority => best = Some(p),
                _ => {}
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ScheduledPrompt {
        ScheduledPrompt {
            id: "p".into(),
            target: "claude".into(),
            trigger: Trigger::At { at: "2026-07-27T09:00:00Z".into() },
            body: "go".into(),
            condition: None,
            valid_until: None,
            priority: 50,
            mission_ver: 1,
            status: PromptStatus::Queued,
        }
    }
    fn ctx() -> SendContext {
        SendContext {
            now_utc: "2026-07-27T09:00:00Z".into(),
            current_mission_ver: 1,
            ..Default::default()
        }
    }

    #[test]
    fn sends_a_due_prompt() {
        assert_eq!(evaluate_send_gate(&base(), &ctx()), SendVerdict::Send);
    }

    #[test]
    fn holds_not_due() {
        let mut p = base();
        p.trigger = Trigger::At { at: "2026-07-27T10:00:00Z".into() };
        assert_eq!(evaluate_send_gate(&p, &ctx()), SendVerdict::NotDue);
    }

    #[test]
    fn expires_past_valid_until_and_on_malformed() {
        let mut p = base();
        p.valid_until = Some("2026-07-27T08:59:59Z".into());
        assert_eq!(evaluate_send_gate(&p, &ctx()), SendVerdict::Expired);
        p.valid_until = Some("garbage".into());
        assert_eq!(evaluate_send_gate(&p, &ctx()), SendVerdict::Expired); // fail closed
    }

    #[test]
    fn supersedes_older_mission_version() {
        let mut c = ctx();
        c.current_mission_ver = 2;
        assert_eq!(evaluate_send_gate(&base(), &c), SendVerdict::Superseded);
    }

    #[test]
    fn condition_gate() {
        let mut p = base();
        p.condition = Some("tests_green".into());
        let mut c = ctx();
        c.condition_result = Some(false);
        assert_eq!(evaluate_send_gate(&p, &c), SendVerdict::ConditionUnmet);
        c.condition_result = Some(true);
        assert_eq!(evaluate_send_gate(&p, &c), SendVerdict::Send);
    }

    #[test]
    fn terminal_statuses() {
        let mut p = base();
        p.status = PromptStatus::Cancelled;
        assert_eq!(evaluate_send_gate(&p, &ctx()), SendVerdict::Cancelled);
        p.status = PromptStatus::Sent;
        assert_eq!(evaluate_send_gate(&p, &ctx()), SendVerdict::AlreadySent);
    }

    #[test]
    fn triggers() {
        let mut c = ctx();
        c.now_utc = "2026-07-27T09:00:01Z".into();
        assert!(is_trigger_due(&Trigger::At { at: "2026-07-27T09:00:00Z".into() }, &c));
        assert!(is_trigger_due(
            &Trigger::After { since_utc: "2026-07-27T08:00:00Z".into(), after_ms: 3_600_000 },
            &ctx()
        ));
        c.fired_event = Some("ci.green".into());
        assert!(is_trigger_due(&Trigger::OnEvent { event: "ci.green".into() }, &c));
        c.cron_due = Some(true);
        assert!(is_trigger_due(&Trigger::Cron { expr: "0 9 * * *".into() }, &c));
    }

    #[test]
    fn selects_highest_priority_sendable() {
        let mut high = base();
        high.id = "high".into();
        high.priority = 90;
        let mut low = base();
        low.id = "low".into();
        low.priority = 10;
        let mut stale = base();
        stale.id = "stale".into();
        stale.priority = 100;
        stale.mission_ver = 0; // superseded
        let mut future = base();
        future.id = "future".into();
        future.priority = 95;
        future.trigger = Trigger::At { at: "2026-07-27T23:00:00Z".into() }; // not due
        let prompts = vec![low, high, stale, future];
        assert_eq!(select_next_to_send(&prompts, &ctx()).unwrap().id, "high");
        let cancelled = {
            let mut p = base();
            p.status = PromptStatus::Cancelled;
            vec![p]
        };
        assert!(select_next_to_send(&cancelled, &ctx()).is_none());
    }
}
