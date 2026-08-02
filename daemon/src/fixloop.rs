//! The fix loop (plan §9 "VCS/CI loop", Phase-2): red build → targeted fix → recheck.
//!
//! The point of an overnight run is that a failure at 01:00 gets fixed by 01:20
//! instead of waiting for you. But an unbounded retry loop is how a night gets
//! burned: the agent tries the same thing eleven times, each attempt costs money and
//! quota, and by morning nothing has moved.
//!
//! So the loop has two brakes, and they exist for different failures:
//!
//! * **A hard attempt cap.** Bounds the cost of a problem that simply cannot be
//!   fixed from here.
//! * **A no-progress check.** Stops the moment two attempts fail *the same way* —
//!   which is the real signal that the agent is stuck rather than working. Ten
//!   different failures is progress; the same failure twice is a loop, and the third
//!   try will not be different.
//!
//! When it gives up it hands back a state, not an exception: the mission goes to
//! `waiting_input`, because a human now has to look. Retrying forever is the failure
//! mode that makes people stop leaving these things running.

use crate::validator::ValidationResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixLoopConfig {
    /// How many fix attempts before giving up and asking a person.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Stop early when an attempt fails exactly as the previous one did.
    #[serde(default = "default_true")]
    pub stop_when_stuck: bool,
}

fn default_max_attempts() -> u32 {
    3
}
fn default_true() -> bool {
    true
}

impl Default for FixLoopConfig {
    fn default() -> Self {
        FixLoopConfig { max_attempts: default_max_attempts(), stop_when_stuck: true }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoopDecision {
    /// Try a fix. The prompt says exactly what failed.
    Retry { attempt: u32, prompt: String },
    /// Nothing left to fix.
    Done,
    /// Stop and hand it to a person, with the reason.
    GiveUp { reason: String },
}

/// A short, stable fingerprint of a failure, used to tell "different problem" from
/// "same problem again".
///
/// Built from validator names plus the *shape* of their output rather than the whole
/// text: timestamps, durations and paths change between runs, and comparing raw
/// output would call every attempt different and never detect a loop.
pub fn failure_signature(results: &[ValidationResult]) -> String {
    let mut parts: Vec<String> = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| {
            // The first line that looks like an error is the identity of the
            // failure; everything after it is usually a rehash.
            let key = r
                .output
                .lines()
                .map(str::trim)
                .find(|l| {
                    let low = l.to_ascii_lowercase();
                    low.contains("error") || low.contains("fail") || low.contains("panic")
                })
                .unwrap_or("")
                .chars()
                // Drop digits so line numbers and durations do not make two
                // identical failures look different.
                .filter(|c| !c.is_ascii_digit())
                .take(120)
                .collect::<String>();
            format!("{}::{}", r.validator, key.trim())
        })
        .collect();
    parts.sort();
    parts.join("|")
}

/// What the agent should be told to fix, from the actual failure output.
pub fn build_fix_prompt(results: &[ValidationResult]) -> String {
    let failed: Vec<&ValidationResult> = results.iter().filter(|r| !r.passed).collect();
    let mut p = String::from(
        "The checks for this unit are failing. Fix ONLY what is needed to make them pass.\n\n",
    );
    for r in &failed {
        p.push_str(&format!("--- {} failed ---\n", r.validator));
        // The tail carries the summary and the failing assertion; the head is
        // usually a banner.
        let lines: Vec<&str> = r.output.lines().collect();
        let tail = lines.iter().rev().take(40).rev().copied().collect::<Vec<_>>().join("\n");
        p.push_str(&tail);
        p.push_str("\n\n");
    }
    p.push_str(
        "Do not change the tests to make them pass, do not disable a check, and do not \
         widen the scope of this unit. If the failure cannot be fixed without doing one of \
         those things, stop and say so instead — that answer is more useful than a green \
         build that means nothing.",
    );
    p
}

/// Decide what to do after a round of validators.
///
/// `previous_signature` is the failure from the last attempt, if any.
pub fn decide(
    cfg: &FixLoopConfig,
    results: &[ValidationResult],
    attempt: u32,
    previous_signature: Option<&str>,
) -> LoopDecision {
    if results.is_empty() {
        // No evidence is not success — the same rule the completion gate uses.
        return LoopDecision::GiveUp {
            reason: "no validators are configured, so there is nothing to fix towards".into(),
        };
    }
    if crate::validator::all_passed(results) {
        return LoopDecision::Done;
    }

    let signature = failure_signature(results);

    if cfg.stop_when_stuck {
        if let Some(prev) = previous_signature {
            if prev == signature {
                return LoopDecision::GiveUp {
                    reason: format!(
                        "attempt {attempt} failed in exactly the same way as the one before it. \
                         The agent is stuck rather than making progress, and another try will \
                         cost quota without changing anything"
                    ),
                };
            }
        }
    }

    if attempt >= cfg.max_attempts {
        return LoopDecision::GiveUp {
            reason: format!(
                "{} fix attempts did not make the checks pass. Stopping so a person can look, \
                 rather than spending the rest of the night on it",
                cfg.max_attempts
            ),
        };
    }

    LoopDecision::Retry { attempt: attempt + 1, prompt: build_fix_prompt(results) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, passed: bool, output: &str) -> ValidationResult {
        ValidationResult {
            validator: name.into(),
            passed,
            output: output.into(),
            duration_ms: 1,
        }
    }

    #[test]
    fn a_green_round_ends_the_loop() {
        let d = decide(&FixLoopConfig::default(), &[r("tests", true, "ok")], 1, None);
        assert_eq!(d, LoopDecision::Done);
    }

    #[test]
    fn no_validators_is_not_success() {
        // Same rule as the completion gate: nobody checked is not the same as fine.
        match decide(&FixLoopConfig::default(), &[], 1, None) {
            LoopDecision::GiveUp { reason } => assert!(reason.contains("nothing to fix towards")),
            other => panic!("must not treat no evidence as done: {other:?}"),
        }
    }

    #[test]
    fn a_failure_produces_a_prompt_carrying_the_actual_output() {
        let results = [r("tests", false, "PASS a\nPASS b\nFAIL src/x.ts > it works\nExpected 4, got 5")];
        match decide(&FixLoopConfig::default(), &results, 1, None) {
            LoopDecision::Retry { attempt, prompt } => {
                assert_eq!(attempt, 2);
                assert!(prompt.contains("FAIL src/x.ts"), "{prompt}");
                assert!(prompt.contains("Expected 4, got 5"), "{prompt}");
                // The instructions that stop a green build from being meaningless.
                assert!(prompt.contains("Do not change the tests"), "{prompt}");
                assert!(prompt.contains("do not disable a check"), "{prompt}");
                assert!(prompt.contains("stop and say so"), "{prompt}");
            }
            other => panic!("expected a retry, got {other:?}"),
        }
    }

    #[test]
    fn the_same_failure_twice_stops_immediately() {
        // The real signal that an agent is stuck. Waiting for the attempt cap here
        // would spend two more turns learning what we already know.
        let results = [r("tests", false, "FAIL src/x.ts > it works\nExpected 4, got 5")];
        let sig = failure_signature(&results);
        match decide(&FixLoopConfig::default(), &results, 1, Some(&sig)) {
            LoopDecision::GiveUp { reason } => {
                assert!(reason.contains("same way"), "{reason}");
                assert!(reason.contains("stuck"), "{reason}");
            }
            other => panic!("must stop when stuck, got {other:?}"),
        }
    }

    #[test]
    fn a_different_failure_counts_as_progress_and_keeps_going() {
        // Ten different failures is work. Only repetition means stuck.
        let first = [r("tests", false, "FAIL parser > tokenises")];
        let second = [r("tests", false, "FAIL emitter > writes output")];
        let sig1 = failure_signature(&first);
        assert!(matches!(
            decide(&FixLoopConfig::default(), &second, 1, Some(&sig1)),
            LoopDecision::Retry { .. }
        ));
    }

    #[test]
    fn line_numbers_and_timings_do_not_make_one_failure_look_like_two() {
        // Without this the stuck-detector never fires: every rerun prints different
        // durations and line numbers, so every attempt looks like a new problem.
        let a = [r("tests", false, "FAIL src/x.ts:41 > it works (12ms)")];
        let b = [r("tests", false, "FAIL src/x.ts:99 > it works (873ms)")];
        assert_eq!(failure_signature(&a), failure_signature(&b));
    }

    #[test]
    fn two_genuinely_different_failures_have_different_signatures() {
        let a = [r("tests", false, "FAIL parser > tokenises")];
        let b = [r("tests", false, "FAIL emitter > writes")];
        assert_ne!(failure_signature(&a), failure_signature(&b));
    }

    #[test]
    fn the_attempt_cap_bounds_a_problem_that_cannot_be_fixed_from_here() {
        let cfg = FixLoopConfig { max_attempts: 3, stop_when_stuck: false };
        let results = [r("tests", false, "FAIL something")];
        assert!(matches!(decide(&cfg, &results, 2, None), LoopDecision::Retry { .. }));
        match decide(&cfg, &results, 3, None) {
            LoopDecision::GiveUp { reason } => {
                assert!(reason.contains("3 fix attempts"), "{reason}");
                assert!(reason.contains("a person can look"), "{reason}");
            }
            other => panic!("the cap must hold, got {other:?}"),
        }
    }

    #[test]
    fn the_signature_ignores_checks_that_passed() {
        let a = [r("tests", false, "FAIL x"), r("lint", true, "clean")];
        let b = [r("tests", false, "FAIL x")];
        assert_eq!(failure_signature(&a), failure_signature(&b));
    }

    #[test]
    fn config_defaults_are_conservative_and_round_trip() {
        let d = FixLoopConfig::default();
        assert_eq!(d.max_attempts, 3, "a night should not be spent on one problem");
        assert!(d.stop_when_stuck);
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<FixLoopConfig>(&json).unwrap(), d);
        assert_eq!(serde_json::from_str::<FixLoopConfig>("{}").unwrap(), d);
    }
}
