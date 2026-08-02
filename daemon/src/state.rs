//! The mission state machine (plan §ז, Phase-2: "Mission-Contract + מנוע-מצבים").
//!
//! An overnight run has to be resumable by a process that was not there when it
//! started. That means "where is this mission" must be a value on disk, not
//! something inferred from the shape of the logs at 8am.
//!
//! Two properties this exists to enforce:
//!
//! * **A finished mission cannot quietly restart.** Terminal is terminal. Without
//!   that, a supervisor restarting after a crash can pick up a mission the human
//!   already stopped and carry on working — the failure that makes people stop
//!   trusting automation entirely.
//! * **Every move is refused or recorded, never silently applied.** An illegal
//!   transition is a bug somewhere, and swallowing it leaves the state plausible
//!   but wrong, which is worse than crashing.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    /// Created, nothing started.
    Draft,
    /// The agent is working.
    Running,
    /// Stopped and waiting for a person. The one state the supervisor must never
    /// leave on its own — that is the whole meaning of asking.
    WaitingInput,
    /// Out of quota, waiting for the window to reopen. Resumes by itself.
    RateLimited,
    /// A human paused it.
    Paused,
    /// Stop requested; we are killing processes and writing the final state.
    Stopping,
    /// Every unit finished and its validators passed.
    Done,
    /// Ended without completing.
    Failed,
    /// Ended because someone asked it to.
    Stopped,
}

impl MissionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            MissionState::Draft => "draft",
            MissionState::Running => "running",
            MissionState::WaitingInput => "waiting_input",
            MissionState::RateLimited => "rate_limited",
            MissionState::Paused => "paused",
            MissionState::Stopping => "stopping",
            MissionState::Done => "done",
            MissionState::Failed => "failed",
            MissionState::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> Option<MissionState> {
        Some(match s {
            "draft" => MissionState::Draft,
            "running" => MissionState::Running,
            "waiting_input" => MissionState::WaitingInput,
            "rate_limited" => MissionState::RateLimited,
            "paused" => MissionState::Paused,
            "stopping" => MissionState::Stopping,
            "done" => MissionState::Done,
            "failed" => MissionState::Failed,
            "stopped" => MissionState::Stopped,
            _ => return None,
        })
    }

    /// Nothing leaves a terminal state. A mission that finished — however it
    /// finished — stays finished.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            MissionState::Done | MissionState::Failed | MissionState::Stopped
        )
    }

    /// Will this state continue on its own, or is it waiting for a person?
    pub fn resumes_itself(&self) -> bool {
        matches!(self, MissionState::Running | MissionState::RateLimited)
    }

    /// Is this waiting on a PERSON specifically?
    ///
    /// Not the same as "won't move by itself": `stopping` is already in progress
    /// and needs nobody. Telling the user to act when the answer is "wait a moment"
    /// trains them to ignore the message.
    pub fn needs_a_person(&self) -> bool {
        matches!(
            self,
            MissionState::Draft | MissionState::WaitingInput | MissionState::Paused
        )
    }

    /// What a person reading `status` needs to know.
    pub fn describe(&self) -> &'static str {
        match self {
            MissionState::Draft => "not started yet",
            MissionState::Running => "working",
            MissionState::WaitingInput => {
                "STOPPED, waiting for you — it will not continue by itself"
            }
            MissionState::RateLimited => "out of quota; will resume when the window reopens",
            MissionState::Paused => "paused by you; resume when ready",
            MissionState::Stopping => "stopping — killing processes and writing the final state",
            MissionState::Done => "finished, all validators passed",
            MissionState::Failed => "ended without completing",
            MissionState::Stopped => "stopped because you asked",
        }
    }
}

impl fmt::Display for MissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IllegalTransition {
    pub from: MissionState,
    pub to: MissionState,
    pub why: String,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot go from {} to {}: {}",
            self.from, self.to, self.why
        )
    }
}

/// Is this move allowed? Returns the reason it is not, so a caller can say
/// something useful instead of "invalid state".
pub fn check(from: MissionState, to: MissionState) -> Result<(), IllegalTransition> {
    use MissionState::*;

    if from == to {
        // Re-entering the same state is a no-op, not an error: a crash-restart
        // that re-declares `running` should not be treated as a bug.
        return Ok(());
    }

    if from.is_terminal() {
        return Err(IllegalTransition {
            from,
            to,
            why: format!(
                "{from} is final. A mission that has finished must not restart on its own — \
                 start a new mission instead"
            ),
        });
    }

    let allowed: &[MissionState] = match from {
        Draft => &[Running, Stopping, Stopped, Failed],
        // Anything can go wrong from Running, so nearly everything is reachable.
        Running => &[WaitingInput, RateLimited, Paused, Stopping, Done, Failed],
        // A quota wait resumes by itself; it can also be given up on.
        RateLimited => &[Running, Paused, Stopping, Failed],
        // Waiting for a person: only a person moves it.
        WaitingInput => &[Running, Paused, Stopping, Failed],
        Paused => &[Running, Stopping, Failed],
        // Stopping only ever ends.
        Stopping => &[Stopped, Failed],
        Done | Failed | Stopped => &[],
    };

    if allowed.contains(&to) {
        return Ok(());
    }

    let why = match (from, to) {
        (Stopping, _) => "a stop in progress can only finish, not turn into something else".into(),
        (_, Draft) => "a mission cannot go back to being unstarted".into(),
        (_, Stopped) => {
            "reach `stopped` through `stopping`, so processes are actually killed first".into()
        }
        _ => format!("{to} is not reachable from {from}"),
    };
    Err(IllegalTransition { from, to, why })
}

/// Where a foreign commit on HEAD should put the mission (plan §ז.2).
///
/// If HEAD is a commit neither we nor the last checkpoint knows about, someone
/// else has been working in this repo. Continuing would mean building on top of
/// changes the agent has never seen and the human never mentioned — so the
/// supervisor stops and asks rather than guessing.
pub fn state_for_foreign_head(
    head_is_ours: bool,
    head_changed_since_checkpoint: bool,
) -> MissionState {
    if head_changed_since_checkpoint && !head_is_ours {
        MissionState::WaitingInput
    } else {
        MissionState::Running
    }
}

#[cfg(test)]
mod tests {
    use super::MissionState::*;
    use super::*;

    #[test]
    fn a_normal_night_walks_the_expected_path() {
        for (from, to) in [
            (Draft, Running),
            (Running, RateLimited),
            (RateLimited, Running),
            (Running, Done),
        ] {
            assert!(check(from, to).is_ok(), "{from} -> {to} should be allowed");
        }
    }

    #[test]
    fn a_finished_mission_never_restarts_by_itself() {
        // The failure that makes people stop trusting automation: a supervisor
        // restarts after a crash, finds a mission the human stopped, and resumes it.
        for terminal in [Done, Failed, Stopped] {
            for to in [Running, RateLimited, Paused, WaitingInput, Draft] {
                let err = check(terminal, to).expect_err("{terminal} -> {to} must be refused");
                assert!(err.why.contains("final"), "{err}");
                assert!(err.why.contains("start a new mission"), "{err}");
            }
        }
    }

    #[test]
    fn stopped_is_only_reachable_through_stopping() {
        // Jumping straight to `stopped` would mean recording that everything was
        // killed without anything having been killed.
        let err = check(Running, Stopped).expect_err("must go via stopping");
        assert!(err.why.contains("through `stopping`"), "{err}");
        assert!(check(Running, Stopping).is_ok());
        assert!(check(Stopping, Stopped).is_ok());
    }

    #[test]
    fn a_stop_in_progress_cannot_become_something_else() {
        for to in [Running, Paused, RateLimited, WaitingInput, Done] {
            assert!(
                check(Stopping, to).is_err(),
                "stopping -> {to} must be refused"
            );
        }
    }

    #[test]
    fn nothing_goes_back_to_draft() {
        for from in [Running, Paused, RateLimited, WaitingInput] {
            let err = check(from, Draft).unwrap_err();
            assert!(
                err.why.contains("cannot go back to being unstarted"),
                "{err}"
            );
        }
    }

    #[test]
    fn re_declaring_the_same_state_is_not_an_error() {
        // A crash-restart that says "running" again is normal, not a bug.
        for s in [
            Draft,
            Running,
            RateLimited,
            WaitingInput,
            Paused,
            Stopping,
            Done,
        ] {
            assert!(check(s, s).is_ok(), "{s} -> {s}");
        }
    }

    #[test]
    fn work_in_progress_is_not_reported_as_waiting_for_you() {
        // `stopping` moves on its own; asking the user to act would be noise, and
        // noise is how a real "you must act" message gets ignored.
        assert!(!Stopping.needs_a_person());
        assert!(!Running.needs_a_person());
        assert!(!RateLimited.needs_a_person());
        for s in [Draft, WaitingInput, Paused] {
            assert!(s.needs_a_person(), "{s} does need a person");
        }
        for s in [Done, Failed, Stopped] {
            assert!(!s.needs_a_person(), "{s} is over");
        }
    }

    #[test]
    fn waiting_for_a_person_is_marked_as_not_self_resuming() {
        // The distinction the morning report and any scheduler depend on: will this
        // continue on its own, or is it sitting there until someone looks?
        assert!(!WaitingInput.resumes_itself());
        assert!(!Paused.resumes_itself());
        assert!(
            RateLimited.resumes_itself(),
            "a quota wait continues by itself"
        );
        assert!(Running.resumes_itself());
        assert!(WaitingInput
            .describe()
            .contains("will not continue by itself"));
    }

    #[test]
    fn a_foreign_commit_on_head_stops_and_asks() {
        // Someone else committed in this repo while the agent was working.
        // Continuing would build on changes the agent has never seen.
        assert_eq!(state_for_foreign_head(false, true), WaitingInput);
        // Our own checkpoint moved HEAD — that is expected, carry on.
        assert_eq!(state_for_foreign_head(true, true), Running);
        // HEAD did not move at all.
        assert_eq!(state_for_foreign_head(false, false), Running);
    }

    #[test]
    fn states_survive_a_round_trip_through_the_database() {
        for s in [
            Draft,
            Running,
            WaitingInput,
            RateLimited,
            Paused,
            Stopping,
            Done,
            Failed,
            Stopped,
        ] {
            assert_eq!(MissionState::parse(s.as_str()), Some(s), "{s}");
        }
        assert_eq!(MissionState::parse("nonsense"), None);
    }

    #[test]
    fn the_error_says_what_to_do_not_just_that_it_is_wrong() {
        let err = check(Done, Running).unwrap_err();
        let text = err.to_string();
        assert!(text.starts_with("cannot go from done to running"), "{text}");
        assert!(text.contains("start a new mission"), "{text}");
    }
}
