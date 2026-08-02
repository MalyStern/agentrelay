//! Cost ledger and budget kill-switch (plan §9, Phase-2).
//!
//! Tokens are the wrong unit to give a person. "1.4 million tokens" means nothing
//! at 3am; "$18.40, and your cap was $15" means something immediately. The provider
//! reports the real number — a contract test against claude 2.1.220 confirms every
//! turn ends with:
//!
//!   {"type":"result", ..., "total_cost_usd":0.0292936, "modelUsage":{...}}
//!
//! so the ledger records what was actually charged rather than an estimate derived
//! from a price table that goes stale.
//!
//! The kill-switch exists because the failure it prevents is unrecoverable in the
//! way that matters: you cannot un-spend money. Every other guard in this system
//! protects something you could restore from git.
//!
//! Two rules:
//!
//! * **The cap is checked BEFORE a turn, not after.** Checking afterwards tells you
//!   how far past the line you already are.
//! * **No cap configured means no limit, and the report says so.** A budget you
//!   believe is set but is not is worse than none at all — the same fail-quiet
//!   shape this codebase keeps finding.

use serde::{Deserialize, Serialize};

/// A cap on spending. `None` anywhere means "not limited", stated explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Budget {
    /// Hard ceiling for one mission, in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_usd: Option<f64>,
    /// Hard ceiling for a single turn — catches one runaway turn rather than a
    /// slow drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_usd: Option<f64>,
}

impl Budget {
    pub fn is_set(&self) -> bool {
        self.mission_usd.is_some() || self.turn_usd.is_some()
    }

    /// In plain words, for `status` — so an unset budget is visible rather than
    /// assumed.
    pub fn describe(&self) -> String {
        match (self.mission_usd, self.turn_usd) {
            (None, None) => {
                "No spending cap is set. AgentRelay will not stop the run on cost."
                    .to_string()
            }
            (Some(m), None) => format!("Spending cap: ${m:.2} for this mission."),
            (None, Some(t)) => format!("Spending cap: ${t:.2} per turn."),
            (Some(m), Some(t)) => {
                format!("Spending cap: ${m:.2} for this mission, ${t:.2} per turn.")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetVerdict {
    /// Room to run.
    Proceed { spent_usd: f64, remaining_usd: Option<f64> },
    /// The cap is reached. The run stops here.
    Stop { spent_usd: f64, cap_usd: f64, reason: String },
}

impl BudgetVerdict {
    pub fn may_proceed(&self) -> bool {
        matches!(self, BudgetVerdict::Proceed { .. })
    }
}

/// Decide whether another turn may start.
///
/// `worst_case_turn_usd` is what the next turn might cost; passing the largest
/// turn seen so far is the honest default. Deciding on `spent` alone would let a
/// run that is one cent under its cap start a five-dollar turn.
pub fn check_before_turn(
    budget: &Budget,
    spent_usd: f64,
    worst_case_turn_usd: f64,
) -> BudgetVerdict {
    if let Some(cap) = budget.turn_usd {
        if worst_case_turn_usd > cap {
            return BudgetVerdict::Stop {
                spent_usd,
                cap_usd: cap,
                reason: format!(
                    "the next turn could cost about ${worst_case_turn_usd:.2}, over the \
                     ${cap:.2} per-turn cap"
                ),
            };
        }
    }
    let Some(cap) = budget.mission_usd else {
        return BudgetVerdict::Proceed { spent_usd, remaining_usd: None };
    };
    if spent_usd >= cap {
        return BudgetVerdict::Stop {
            spent_usd,
            cap_usd: cap,
            reason: format!("${spent_usd:.2} spent, at or over the ${cap:.2} cap for this mission"),
        };
    }
    // The check that matters: would STARTING this turn cross the line? Answering
    // after the fact only tells you how far past it you are.
    if spent_usd + worst_case_turn_usd > cap {
        return BudgetVerdict::Stop {
            spent_usd,
            cap_usd: cap,
            reason: format!(
                "${spent_usd:.2} spent and the next turn could add about \
                 ${worst_case_turn_usd:.2}, which would pass the ${cap:.2} cap"
            ),
        };
    }
    BudgetVerdict::Proceed { spent_usd, remaining_usd: Some(cap - spent_usd) }
}

/// Pull the charged amount out of a provider `result` event.
///
/// Field name verified against a live run, not guessed. Returns None when the
/// provider did not report one — and None must never be read as zero, or a run
/// with no cost reporting would look free.
pub fn cost_from_result(result_line: &serde_json::Value) -> Option<f64> {
    result_line
        .get("total_cost_usd")
        .and_then(|v| v.as_f64())
        .filter(|c| c.is_finite() && *c >= 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_real_cost_field_is_read_from_a_real_result_line() {
        // Copied from a live capture. If the name were wrong every run would look
        // free, and the kill-switch would never fire.
        let line = json!({
            "type": "result",
            "subtype": "success",
            "total_cost_usd": 0.0292936,
            "usage": {"input_tokens": 18, "output_tokens": 693}
        });
        assert_eq!(cost_from_result(&line), Some(0.0292936));
    }

    #[test]
    fn a_missing_cost_is_none_and_never_zero() {
        // Zero would read as "this run is free" and disarm the cap entirely.
        assert_eq!(cost_from_result(&json!({"type": "result"})), None);
        assert_eq!(cost_from_result(&json!({"total_cost_usd": "1.00"})), None);
        assert_eq!(cost_from_result(&json!({"total_cost_usd": -1.0})), None);
    }

    #[test]
    fn no_budget_means_no_limit_and_says_so_out_loud() {
        let b = Budget::default();
        assert!(!b.is_set());
        assert!(b.describe().contains("No spending cap is set"));
        assert!(check_before_turn(&b, 9_999.0, 500.0).may_proceed());
    }

    #[test]
    fn the_cap_is_enforced_before_the_turn_that_would_cross_it() {
        let b = Budget { mission_usd: Some(10.0), turn_usd: None };
        // $9.50 spent, a turn that might cost $1 — that crosses $10, so it does not
        // start. Checking afterwards would just tell us we are at $10.50.
        match check_before_turn(&b, 9.50, 1.00) {
            BudgetVerdict::Stop { reason, cap_usd, spent_usd } => {
                assert_eq!(cap_usd, 10.0);
                assert_eq!(spent_usd, 9.50);
                assert!(reason.contains("would pass"), "{reason}");
            }
            other => panic!("must stop, got {other:?}"),
        }
        // Comfortably inside: proceed, and say what is left.
        match check_before_turn(&b, 2.00, 1.00) {
            BudgetVerdict::Proceed { remaining_usd, .. } => {
                assert_eq!(remaining_usd, Some(8.0));
            }
            other => panic!("must proceed, got {other:?}"),
        }
    }

    #[test]
    fn a_single_runaway_turn_is_caught_by_the_per_turn_cap() {
        // Slow drift is caught by the mission cap; one enormous turn is not, and it
        // is the one that happens while you are asleep.
        let b = Budget { mission_usd: Some(100.0), turn_usd: Some(2.0) };
        assert!(!check_before_turn(&b, 1.0, 5.0).may_proceed());
        assert!(check_before_turn(&b, 1.0, 0.5).may_proceed());
    }

    #[test]
    fn being_exactly_at_the_cap_stops() {
        let b = Budget { mission_usd: Some(5.0), turn_usd: None };
        assert!(!check_before_turn(&b, 5.0, 0.0).may_proceed());
    }

    #[test]
    fn the_budget_survives_a_round_trip_through_config_json() {
        let b = Budget { mission_usd: Some(12.5), turn_usd: Some(1.0) };
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<Budget>(&json).unwrap(), b);
        // And an absent budget parses as no limit rather than failing.
        assert_eq!(serde_json::from_str::<Budget>("{}").unwrap(), Budget::default());
    }
}
