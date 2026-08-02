//! The Daemon: the single object that ties the pieces together for a real run —
//! config + the SQLite store + keep-awake + the MissionRunner. The CLI/binary and
//! (later) the IPC surface call into this. Library code so it's testable.

use crate::adapter::ProviderAdapter;
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::keepawake::KeepAwake;
use crate::mission::{run_turn_supervised, TurnOutcome};
use crate::provider::{Guard, SessionSpec, TurnInput};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn default_program() -> String {
    "claude".to_string()
}
fn default_true() -> bool {
    true
}

/// The mission config (a lite Mission Contract for v0). Committed as
/// `.agentrelay/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The engine program to run (default "claude").
    #[serde(default = "default_program")]
    pub program: String,
    /// Guards enforced on every tool_pre (Watchtower layer-1).
    #[serde(default)]
    pub guards: Vec<Guard>,
    /// Keep the machine awake during a run.
    #[serde(default = "default_true")]
    pub keep_awake: bool,
    /// Completion validators (Watchtower layer-2): a unit is done only if every one
    /// passes. Empty = no evidence required (not recommended for autopilot).
    #[serde(default)]
    pub validators: Vec<crate::validator::Validator>,
    /// Where to send a notification when the night ends or a unit fails. The only
    /// outbound network path in the daemon, and `None` by default.
    #[serde(default)]
    pub notify: crate::notify::NotifyConfig,
    /// Spending cap. Unset means no limit — and `status` says so out loud, because
    /// a budget you believe is set but isn't is worse than none.
    #[serde(default)]
    pub budget: crate::cost::Budget,
    /// Run the agent inside a container. Off unless an image is named.
    #[serde(default)]
    pub isolation: crate::isolation::Isolation,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            program: default_program(),
            guards: vec![
                Guard::new(crate::provider::GuardKind::NoPush),
                Guard::new(crate::provider::GuardKind::NoDeploy),
                Guard::new(crate::provider::GuardKind::WorkspaceBoundary),
            ],
            keep_awake: true,
            validators: vec![],
            notify: crate::notify::NotifyConfig::None,
            budget: crate::cost::Budget::default(),
            isolation: crate::isolation::Isolation::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusReport {
    pub db_path: String,
    pub schema_version: i64,
    pub missions: i64,
    pub keep_awake: bool,
    pub program: String,
    /// In plain words: what, if anything, leaves this machine.
    pub notifications: String,
    /// In plain words: the spending cap, or that there isn't one.
    pub budget: String,
    pub spent_usd: f64,
    /// Where the default mission is, and whether it will move on its own.
    pub state: String,
    pub state_meaning: String,
    pub waiting_for_you: bool,
    /// In plain words: whether the agent is contained, and what it can reach.
    pub isolation: String,
}

const DIR: &str = ".agentrelay";

pub struct Daemon {
    root: PathBuf,
    store: Store,
    config: Config,
}

impl Daemon {
    fn dir(root: &Path) -> PathBuf {
        root.join(DIR)
    }
    fn config_path(root: &Path) -> PathBuf {
        Self::dir(root).join("config.json")
    }
    fn db_path(root: &Path) -> PathBuf {
        Self::dir(root).join("agentrelay.db")
    }

    /// Scaffold `.agentrelay/` (config + db) under `root`.
    pub fn init(root: impl AsRef<Path>) -> Result<Daemon> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(Self::dir(&root))
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        let config = Config::default();
        if !Self::config_path(&root).exists() {
            let json = serde_json::to_string_pretty(&config).unwrap();
            std::fs::write(Self::config_path(&root), json)
                .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        }

        // The supervisor checkpoints with `git add --all`, which would otherwise
        // sweep our own database — machine state, holding prompts and file paths —
        // into the user's commits. A self-ignoring directory is the fix: it needs no
        // cooperation from the user's .gitignore and works in a repo we did not
        // create. `!config.json` keeps the one file worth committing, so a team can
        // share the guards and validators.
        let ignore = Self::dir(&root).join(".gitignore");
        if !ignore.exists() {
            std::fs::write(
                &ignore,
                "# AgentRelay machine state — not yours to commit.\n*\n!.gitignore\n!config.json\n",
            )
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        }

        let store = Store::open(Self::db_path(&root).to_str().unwrap())
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(Daemon { root, store, config })
    }

    /// Open an existing workspace (or fall back to defaults if config is missing).
    pub fn open(root: impl AsRef<Path>) -> Result<Daemon> {
        let root = root.as_ref().to_path_buf();
        if !Self::dir(&root).exists() {
            return Err(AgentRelayError::new(
                ErrorCode::Internal,
                format!("no {DIR} here — run `agentrelay init` first"),
            ));
        }
        // A MISSING config is fine (defaults). A config that exists but doesn't parse
        // is not: silently falling back would drop the user's guards and validators
        // without a word — the supervisor would look armed while running unguarded.
        let config = match std::fs::read_to_string(Self::config_path(&root)) {
            Err(_) => Config::default(),
            // Notepad and PowerShell's `Set-Content -Encoding utf8` both prepend a
            // UTF-8 BOM, which serde rejects — on Windows that is the *normal* way a
            // user edits this file, so strip it rather than blame them.
            Ok(s) => serde_json::from_str(s.strip_prefix('\u{feff}').unwrap_or(&s)).map_err(|e| {
                AgentRelayError::new(
                    ErrorCode::Internal,
                    format!(
                        "{} is not valid config: {e}\nFix it (or delete it to use defaults) — \
                         refusing to run with guards silently disabled.",
                        Self::config_path(&root).display()
                    ),
                )
            })?,
        };
        let store = Store::open(Self::db_path(&root).to_str().unwrap())
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(Daemon { root, store, config })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn status(&self) -> StatusReport {
        let st = self.mission_state("default");
        StatusReport {
            db_path: Self::db_path(&self.root).to_string_lossy().into_owned(),
            schema_version: self.store.schema_version(),
            missions: self.store.count_missions().unwrap_or(0),
            keep_awake: self.config.keep_awake,
            program: self.config.program.clone(),
            notifications: self.config.notify.describe_privacy(),
            budget: self.config.budget.describe(),
            spent_usd: self.store.total_cost_usd("default"),
            state: st.as_str().to_string(),
            state_meaning: st.describe().to_string(),
            waiting_for_you: st.needs_a_person(),
            isolation: self.config.isolation.describe(),
        }
    }

    /// May another turn start, given what has been spent so far?
    ///
    /// Checked BEFORE the turn. Asking afterwards only reports how far past the
    /// line the run already is, and money is the one thing here that cannot be
    /// restored from git.
    pub fn check_budget(&self, mission_id: &str) -> crate::cost::BudgetVerdict {
        let spent = self.store.total_cost_usd(mission_id);
        // The most expensive turn so far is the honest guess at the next one. With
        // no history, a small non-zero figure still prevents starting a run whose
        // cap is already reached.
        let worst = self.store.max_turn_cost_usd(mission_id).max(0.01);
        crate::cost::check_before_turn(&self.config.budget, spent, worst)
    }

    /// Send the report headline, if — and only if — notifications are configured.
    ///
    /// A delivery failure is returned, never propagated into the run: a night's
    /// work must not be lost because a phone server was unreachable.
    pub fn notify_report(&self, r: &crate::report::MorningReport) -> Result<bool> {
        if !self.config.notify.is_enabled() {
            return Ok(false);
        }
        let urgency = if r.units_failed > 0 {
            crate::notify::Urgency::Attention
        } else {
            crate::notify::Urgency::Quiet
        };
        // The headline only. Never r.render(), which names files.
        let n = crate::notify::Notification::new("AgentRelay", r.headline(), urgency);
        crate::notify::from_config(&self.config.notify).send(&n)?;
        Ok(true)
    }

    /// Run one supervised turn against a given adapter (testable with the fake).
    pub async fn run_turn_with(
        &self,
        adapter: &dyn ProviderAdapter,
        mission_id: &str,
        prompt: &str,
        cwd: &str,
    ) -> Result<TurnOutcome> {
        // Hold keep-awake for the duration of the turn (best-effort).
        let _awake = if self.config.keep_awake {
            KeepAwake::new(false).ok()
        } else {
            None
        };
        let handle = adapter
            .start_session(SessionSpec {
                cwd: cwd.to_string(),
                ..Default::default()
            })
            .await?;
        let now = chrono::Utc::now().to_rfc3339();
        run_turn_supervised(
            adapter,
            &handle,
            TurnInput {
                prompt: prompt.to_string(),
                read_only: false,
            },
            &self.config.guards,
            &self.store,
            mission_id,
            &now,
        )
        .await
    }

    /// Run against the configured Claude engine.
    pub async fn run_turn(&self, mission_id: &str, prompt: &str, cwd: &str) -> Result<TurnOutcome> {
        // The budget kill-switch, before anything is spent. Every other guard here
        // protects something git can restore; this one protects the only thing that
        // cannot be un-spent.
        // If the user asked for isolation and we cannot provide it, stop. Running
        // unisolated would deliver the opposite of what was configured, silently.
        if let Some(why) = crate::isolation::blocker(&self.config.isolation) {
            return Err(AgentRelayError::new(ErrorCode::Internal, why));
        }
        if let crate::cost::BudgetVerdict::Stop { reason, .. } = self.check_budget(mission_id) {
            return Err(AgentRelayError::new(
                ErrorCode::Internal,
                format!("budget reached — not starting a turn: {reason}"),
            ));
        }
        let adapter = crate::claude_adapter::ClaudeAdapter::with_program(&self.config.program);
        self.run_turn_with(&adapter, mission_id, prompt, cwd).await
    }

    /// The completion gate (Watchtower layer-2): run the config's validators in
    /// `cwd`, persist each result, and return the results plus whether ALL passed.
    /// A unit is "done" only on system evidence, never the model's word.
    pub async fn run_validators(&self, work_unit_id: &str, cwd: &str) -> Vec<crate::validator::ValidationResult> {
        let results = crate::validator::run_all(&self.config.validators, cwd).await;
        for r in &results {
            let _ = self.store.save_validation(work_unit_id, r);
        }
        results
    }

    /// Complete a unit of work: run the validators, and checkpoint the tree **only
    /// if they all passed**.
    ///
    /// This is the join between the two halves of the design. Checkpointing on the
    /// agent's say-so would fill the night with commits of broken code, each one
    /// looking like a safe point to return to; checkpointing on evidence means every
    /// commit in the morning is a state that actually built. When the gate fails
    /// nothing is committed — the work stays in the tree, visible and undone.
    /// Where this mission is. `Draft` when nothing has been recorded yet.
    pub fn mission_state(&self, mission_id: &str) -> crate::state::MissionState {
        self.store
            .latest_state(mission_id)
            .and_then(|s| crate::state::MissionState::parse(&s))
            .unwrap_or(crate::state::MissionState::Draft)
    }

    /// Move the mission, refusing any move the state machine does not allow.
    ///
    /// The refusal is the point. Recording an impossible transition would leave the
    /// state plausible but wrong, and the next process to read it — after a crash,
    /// hours later — has no way to tell.
    pub fn transition(
        &self,
        mission_id: &str,
        to: crate::state::MissionState,
        reason: &str,
    ) -> Result<crate::state::MissionState> {
        let from = self.mission_state(mission_id);
        crate::state::check(from, to).map_err(|e| {
            AgentRelayError::new(ErrorCode::Internal, e.to_string())
        })?;
        if from != to {
            self.store
                .record_transition(mission_id, Some(from.as_str()), to.as_str(), Some(reason))
                .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        }
        Ok(to)
    }

    /// Should a unit of roughly this length be started now, or would it be cut off
    /// by the quota window closing?
    ///
    /// Answerable only because the provider reports its reset time on healthy
    /// turns. Being interrupted mid-unit is the expensive case — half-finished,
    /// unvalidated, uncheckpointed work, and a fresh session that has lost the
    /// context needed to continue it.
    pub fn should_start_unit(&self, estimated_ms: i64, now_utc: &str) -> crate::quota::StartDecision {
        let reset = self.store.latest_quota_reset(&self.config.program);
        crate::quota::decide_start(reset.as_deref(), now_utc, estimated_ms, None)
    }

    /// Record what the working tree looked like BEFORE the agent starts a unit.
    ///
    /// Everything already uncommitted at this moment belongs to the human. Without
    /// this snapshot the supervisor cannot tell their work from the agent's, and a
    /// checkpoint would sweep in an unrelated edit the user left open — which the
    /// "undo this unit" command would then throw away.
    pub fn begin_unit(&self, mission_id: &str, unit_id: &str, cwd: &str) -> Result<Vec<String>> {
        let existing = crate::checkpoint::dirty_paths(cwd).unwrap_or_default();
        self.store
            .save_unit_baseline(mission_id, unit_id, &existing)
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(existing)
    }

    pub async fn complete_unit(&self, mission_id: &str, unit_id: &str, cwd: &str, summary: &str) -> UnitOutcome {
        let validations = self.run_validators(unit_id, cwd).await;
        let gate_passed = crate::validator::all_passed(&validations);

        // Record the unit itself, not just its validations. Without this row the
        // morning report has nothing to iterate: it found the checkpoints but not
        // the units, and cheerfully reported "all validators passed" on a night
        // where one had failed. A report that is wrong in the reassuring direction
        // is worse than no report.
        let _ = self.store.upsert_work_unit(&crate::store::WorkUnitRow {
            id: unit_id.to_string(),
            mission_id: mission_id.to_string(),
            seq: None,
            title: Some(summary.to_string()),
            contract: serde_json::json!({}),
            worker: None,
            status: Some(if gate_passed { "done" } else { "failed" }.to_string()),
            worktree_path: Some(cwd.to_string()),
        });

        // No validators configured is NOT a pass — there is no evidence either way,
        // and silently checkpointing would present an unverified state as verified.
        if !gate_passed {
            return UnitOutcome {
                validations,
                gate_passed,
                checkpoint: None,
                checkpoint_error: None,
                left_alone: vec![],
            };
        }

        // Work out what is the agent's to commit. Anything already uncommitted when
        // the unit began is the user's, and stays theirs.
        let now_dirty = crate::checkpoint::dirty_paths(cwd).unwrap_or_default();
        let baseline = self.store.get_unit_baseline(mission_id, unit_id);
        let (to_commit, left_alone) = match &baseline {
            Some(before) => {
                let (mine, theirs): (Vec<String>, Vec<String>) =
                    now_dirty.into_iter().partition(|p| !before.contains(p));
                (mine, theirs)
            }
            // No baseline. If the tree is clean there is nothing of the user's to
            // lose. If it is NOT clean we cannot tell whose work it is, and the
            // safe answer is to commit nothing and say why — guessing here is how
            // you delete someone's evening.
            None if now_dirty.is_empty() => (vec![], vec![]),
            None => {
                return UnitOutcome {
                    validations,
                    gate_passed,
                    checkpoint: None,
                    checkpoint_error: Some(format!(
                        "not checkpointed: {} file(s) were already uncommitted and no baseline was \
                         recorded, so the supervisor cannot tell your work from the agent's. \
                         Run `agentrelay begin` before the agent starts, or commit/stash your own \
                         changes first. Nothing was touched.",
                        now_dirty.len()
                    )),
                    left_alone: now_dirty,
                };
            }
        };

        match crate::checkpoint::create_paths(cwd, unit_id, summary, &to_commit) {
            Ok(cp) => {
                if cp.created {
                    let _ = self.store.save_checkpoint(&crate::store::CheckpointRow {
                        id: cp.id.clone(),
                        mission_id: Some(mission_id.to_string()),
                        git_head: Some(cp.head.clone()),
                        git_branch: cp.branch.clone(),
                        summary: Some(summary.to_string()),
                    });
                }
                UnitOutcome { validations, gate_passed, checkpoint: Some(cp), checkpoint_error: None, left_alone }
            }
            // A repo we can't checkpoint (not a repo, git missing) must not be
            // reported as a failed unit — the work passed; only the safety net is
            // missing, and the user needs to be told which of the two happened.
            Err(e) => UnitOutcome {
                validations,
                gate_passed,
                checkpoint: None,
                checkpoint_error: Some(e.to_string()),
                left_alone,
            },
        }
    }
}

impl Daemon {
    /// What actually happened overnight, sourced line by line.
    pub fn morning_report(&self, mission_id: &str, cwd: &str) -> crate::report::MorningReport {
        // Saved bytes is a counter we actually incremented each time a re-read was
        // skipped — NOT the sum of the receipts, which is the total content held and
        // would overstate the saving every time. A report built to be auditable
        // cannot afford a flattering number in it.
        let saved = self.store.get_saved_bytes(mission_id).unwrap_or(0);
        crate::report::build(&self.store, mission_id, cwd, saved)
    }

    /// Load this mission's file receipts — what the agent already holds.
    ///
    /// Called on resume: after a quota reset the agent starts with an empty
    /// conversation but the *repo* has not changed, so the expensive part (reading
    /// the codebase again) is exactly what the receipts make unnecessary.
    pub fn receipt_book(&self, mission_id: &str) -> crate::context::ReceiptBook {
        let saved = self.store.get_receipts(mission_id).unwrap_or_default();
        crate::context::ReceiptBook::with_receipts(saved)
    }

    /// Decide whether a file needs to go into context, record the decision, and
    /// persist it so the answer survives a restart.
    pub fn observe_read(
        &self,
        book: &mut crate::context::ReceiptBook,
        mission_id: &str,
        path: &str,
        content: &[u8],
        turn: u32,
    ) -> crate::context::ReadDecision {
        let decision = book.observe(path, content, turn);
        let _ = self.store.save_receipt(mission_id, decision.receipt());
        if let crate::context::ReadDecision::AlreadyHave { saved_bytes, .. } = &decision {
            let _ = self.store.add_saved_bytes(mission_id, *saved_bytes);
        }
        decision
    }
}

/// The result of finishing a unit: what the evidence said, and what was recorded.
#[derive(Debug)]
pub struct UnitOutcome {
    pub validations: Vec<crate::validator::ValidationResult>,
    pub gate_passed: bool,
    pub checkpoint: Option<crate::checkpoint::Checkpoint>,
    pub checkpoint_error: Option<String>,
    /// Files that were already uncommitted before the unit began. The supervisor
    /// deliberately did not touch them; they are reported so the user knows.
    pub left_alone: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AgentEvent, Usage};
    use crate::fake_adapter::FakeAdapter;
    use serde_json::json;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("agentrelay-daemon-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn init_creates_workspace_and_status_reads_it() {
        let root = tmp();
        let d = Daemon::init(&root).unwrap();
        assert!(root.join(".agentrelay/config.json").exists());
        assert!(root.join(".agentrelay/agentrelay.db").exists());
        let s = d.status();
        assert_eq!(s.schema_version, crate::store::SCHEMA_VERSION);
        assert_eq!(s.missions, 0);
        assert_eq!(s.program, "claude");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_reflects_a_persisted_mission() {
        let root = tmp();
        {
            let d = Daemon::init(&root).unwrap();
            d.store()
                .upsert_mission(&crate::store::MissionRow {
                    id: "m1".into(),
                    title: "Nightly".into(),
                    contract: json!({}),
                    status: "running".into(),
                    autonomy: "overnight".into(),
                    timezone: None,
                })
                .unwrap();
        }
        let d2 = Daemon::open(&root).unwrap();
        assert_eq!(d2.status().missions, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_without_init_errors() {
        let root = tmp();
        assert!(Daemon::open(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_config_is_an_error_not_a_silent_fallback() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        std::fs::write(Daemon::config_path(&root), "{ this is not json ").unwrap();
        let err = match Daemon::open(&root) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a broken config must not open silently"),
        };
        assert!(err.contains("not valid config"), "got: {err}");
        // The danger this guards: a typo must never quietly drop the guards.
        assert!(err.contains("guards silently disabled"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_config_saved_by_notepad_or_powershell_with_a_bom_still_parses() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            "\u{feff}{\"program\":\"codex\",\"validators\":[{\"kind\":\"tests\",\"command\":\"npm test\"}]}",
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();
        assert_eq!(d.config().program, "codex");
        assert_eq!(d.config().validators.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_config_still_falls_back_to_defaults() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        std::fs::remove_file(Daemon::config_path(&root)).unwrap();
        let d = Daemon::open(&root).unwrap();
        assert_eq!(d.config().program, Config::default().program);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A workspace whose cwd is a real git repo with one commit — what a mission
    /// actually runs against.
    fn tmp_repo() -> (PathBuf, String) {
        let root = tmp();
        std::fs::create_dir_all(&root).unwrap();
        let p = root.to_str().unwrap().to_string();
        let g = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&p).output().unwrap()
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.name=T", "-c", "user.email=t@e.com", "commit", "-q", "-m", "base"]);
        (root, p)
    }

    #[tokio::test]
    async fn a_unit_is_checkpointed_only_when_the_evidence_passes() {
        let (root, cwd) = tmp_repo();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "validators": [{ "kind": "tests", "command": "node --version" }] }).to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();

        // The supervisor notes the tree BEFORE the agent starts, so it can tell the
        // agent's output from the user's own uncommitted work.
        let baseline = d.begin_unit("m1", "u1", &cwd).unwrap();
        assert!(!baseline.is_empty(), "init leaves its own files uncommitted");

        // The agent changed something and the validators pass -> committed.
        std::fs::write(root.join("work.txt"), "the agent's output\n").unwrap();
        let out = d.complete_unit("m1", "u1", &cwd, "unit 1: add work.txt").await;
        assert!(out.gate_passed);
        let cp = out.checkpoint.clone().unwrap_or_else(|| panic!("no checkpoint; err={:?} left_alone={:?}", out.checkpoint_error, out.left_alone));
        assert!(cp.created);
        assert!(crate::checkpoint::is_ours(&cwd, &cp.head));
        // The tree is deliberately NOT clean: what was already uncommitted before
        // the unit began stays that way, and is reported rather than swept up.
        assert_eq!(cp.files_changed, 1, "only the agent's file");
        assert_eq!(out.left_alone, baseline);
        assert!(crate::checkpoint::dirty_paths(&cwd).unwrap().contains(&baseline[0]));
        // Our own database must never end up in the user's history.
        let tracked = std::process::Command::new("git")
            .args(["ls-files"])
            .current_dir(&cwd)
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&tracked.stdout);
        assert!(tracked.contains("work.txt"), "the agent's work is committed");
        assert!(!tracked.contains("agentrelay.db"), "our database is not: {tracked}");
        assert_eq!(d.store().latest_checkpoint("m1").unwrap().unwrap().git_head, Some(cp.head));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_failing_gate_leaves_the_work_uncommitted() {
        let (root, cwd) = tmp_repo();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "validators": [{ "kind": "tests", "command": "this-command-does-not-exist-xyz-123" }] })
                .to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();
        let before = crate::checkpoint::head(&cwd).unwrap();

        std::fs::write(root.join("broken.txt"), "half-finished\n").unwrap();
        let out = d.complete_unit("m1", "u1", &cwd, "unit 1").await;

        assert!(!out.gate_passed);
        assert!(out.checkpoint.is_none(), "broken work must never become a restore point");

        // …and the morning report must SAY it failed. The bug this pins: without a
        // work-unit row the report saw no units and reported the night as clean.
        let report = d.morning_report("m1", &cwd);
        assert_eq!(report.units_failed, 1, "{}", report.render());
        assert!(report.headline().contains("FAILED"), "{}", report.headline());
        assert_eq!(crate::checkpoint::head(&cwd).unwrap(), before, "HEAD must not move");
        // The work itself is untouched — visible in the tree for the human to judge.
        assert!(root.join("broken.txt").exists());
        assert!(!crate::checkpoint::is_clean(&cwd).unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn receipts_survive_a_quota_reset_so_the_repo_is_not_re_read() {
        // The scenario this exists for: the run dies at the quota, the daemon
        // restarts hours later with a fresh conversation, and the repo is unchanged.
        let root = tmp();
        Daemon::init(&root).unwrap();
        let big = vec![b'x'; 40_000];

        {
            let d = Daemon::open(&root).unwrap();
            let mut book = d.receipt_book("m1");
            assert!(book.is_empty(), "first run knows nothing");
            let first = d.observe_read(&mut book, "m1", "src/big.rs", &big, 1);
            assert!(first.should_send(), "it has to be read once");
        }

        // --- process dies here; new Daemon, new ReceiptBook ---
        {
            let d = Daemon::open(&root).unwrap();
            let mut book = d.receipt_book("m1");
            assert_eq!(book.len(), 1, "the receipt was persisted");
            let again = d.observe_read(&mut book, "m1", "src/big.rs", &big, 1);
            assert!(!again.should_send(), "unchanged file: 40 KB not spent again");
            assert_eq!(book.saved_bytes, 40_000);
            // The saving is counted at the moment of the skip, so the morning
            // report can state it without inferring — and it survives the restart.
            assert_eq!(d.store().get_saved_bytes("m1").unwrap(), 40_000);

            // And the safety property still holds across the restart.
            let edited = d.observe_read(&mut book, "m1", "src/big.rs", b"different now", 2);
            assert!(edited.should_send(), "a changed file is always re-sent");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_users_own_uncommitted_work_is_never_swept_into_a_checkpoint() {
        // The worst bug this project has had. You edit a file, go to bed, the agent
        // touches something unrelated — and `git add --all` put YOUR file in the
        // agent's commit, next to an "undo this unit" command that would delete it.
        let (root, cwd) = tmp_repo();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "validators": [{ "kind": "tests", "command": "node --version" }] }).to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();

        // Three hours of the user's own work, uncommitted.
        std::fs::write(root.join("seed.txt"), "my novel, chapter one, much improved\n").unwrap();
        d.begin_unit("m1", "u1", &cwd).unwrap();

        // The agent does something unrelated.
        std::fs::write(root.join("agent.txt"), "agent output\n").unwrap();
        let out = d.complete_unit("m1", "u1", &cwd, "unit 1").await;

        let cp = out.checkpoint.expect("the agent's work is still checkpointed");
        assert!(out.left_alone.contains(&"seed.txt".to_string()), "{:?}", out.left_alone);

        let committed = std::process::Command::new("git")
            .args(["show", "--stat", "--name-only", "--format=", &cp.head])
            .current_dir(&cwd)
            .output()
            .unwrap();
        let committed = String::from_utf8_lossy(&committed.stdout);
        assert!(committed.contains("agent.txt"), "{committed}");
        assert!(!committed.contains("seed.txt"), "the user's file was swept in: {committed}");

        // And the undo we advertise really is safe to run now.
        // Run the exact command the CLI prints, and check the user's work survives.
        let cmd = crate::checkpoint::undo_command(&cp.head);
        let args: Vec<&str> = cmd.split_whitespace().skip(1).take(3).collect();
        let _ = std::process::Command::new("git")
            .args(&args)
            .current_dir(&cwd)
            .output()
            .unwrap();
        let novel = std::fs::read_to_string(root.join("seed.txt")).unwrap();
        assert!(novel.contains("much improved"), "undo destroyed the user's work: {novel}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_dirty_tree_with_no_baseline_is_refused_rather_than_guessed_at() {
        let (root, cwd) = tmp_repo();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "validators": [{ "kind": "tests", "command": "node --version" }] }).to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();
        std::fs::write(root.join("seed.txt"), "user's edit\n").unwrap();
        let before = crate::checkpoint::head(&cwd).unwrap();

        // No begin_unit: the supervisor cannot tell whose work this is.
        let out = d.complete_unit("m1", "u1", &cwd, "unit 1").await;
        assert!(out.gate_passed, "the validators still ran and passed");
        assert!(out.checkpoint.is_none());
        let err = out.checkpoint_error.unwrap();
        assert!(err.contains("cannot tell your work from the agent's"), "{err}");
        assert!(err.contains("Nothing was touched"), "{err}");
        assert_eq!(crate::checkpoint::head(&cwd).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_budget_stops_the_run_before_the_money_is_spent() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "budget": { "mission_usd": 1.00 } }).to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();

        assert!(d.check_budget("m1").may_proceed(), "nothing spent yet");

        // Two turns at 60 cents. The second is refused BEFORE it runs, not
        // reported afterwards.
        d.store().add_cost_usd("m1", 0.60).unwrap();
        d.store().note_turn_cost_usd("m1", 0.60).unwrap();
        match d.check_budget("m1") {
            crate::cost::BudgetVerdict::Stop { spent_usd, cap_usd, reason } => {
                assert!((spent_usd - 0.60).abs() < 1e-9);
                assert!((cap_usd - 1.00).abs() < 1e-9);
                assert!(reason.contains("would pass"), "{reason}");
            }
            other => panic!("must stop before crossing, got {other:?}"),
        }

        // And run_turn itself refuses rather than relying on the caller to check.
        let err = d.run_turn("m1", "go", ".").await.unwrap_err().to_string();
        assert!(err.contains("budget reached"), "{err}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spending_survives_a_restart_because_a_cap_is_useless_otherwise() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        {
            let d = Daemon::open(&root).unwrap();
            d.store().add_cost_usd("m1", 0.25).unwrap();
            d.store().add_cost_usd("m1", 0.25).unwrap();
        }
        let d = Daemon::open(&root).unwrap();
        assert!((d.store().total_cost_usd("m1") - 0.50).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn with_no_budget_configured_status_says_so_rather_than_implying_one() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        let d = Daemon::open(&root).unwrap();
        assert!(!d.config().budget.is_set());
        assert!(d.status().budget.contains("No spending cap is set"), "{}", d.status().budget);
        assert!(d.check_budget("m1").may_proceed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stopped_mission_cannot_be_restarted_by_a_daemon_that_comes_back_later() {
        use crate::state::MissionState::*;
        let root = tmp();
        Daemon::init(&root).unwrap();
        {
            let d = Daemon::open(&root).unwrap();
            assert_eq!(d.mission_state("m1"), Draft, "nothing recorded yet");
            d.transition("m1", Running, "night started").unwrap();
            d.transition("m1", Stopping, "user asked to stop").unwrap();
            d.transition("m1", Stopped, "processes killed").unwrap();
        }

        // A fresh process — the crash-restart case — must not pick it back up.
        let d = Daemon::open(&root).unwrap();
        assert_eq!(d.mission_state("m1"), Stopped, "the state survived the restart");
        let err = d.transition("m1", Running, "resuming after restart").unwrap_err().to_string();
        assert!(err.contains("final"), "{err}");
        assert!(err.contains("start a new mission"), "{err}");
        assert_eq!(d.mission_state("m1"), Stopped, "and the refusal did not move it");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_illegal_transition_is_refused_rather_than_recorded() {
        use crate::state::MissionState::*;
        let root = tmp();
        Daemon::init(&root).unwrap();
        let d = Daemon::open(&root).unwrap();
        d.transition("m1", Running, "started").unwrap();

        // Straight to `stopped` would claim processes were killed when nothing was.
        assert!(d.transition("m1", Stopped, "shortcut").is_err());
        assert_eq!(d.mission_state("m1"), Running, "state unchanged by a refusal");

        // The legal route works and is recorded.
        d.transition("m1", Stopping, "stop requested").unwrap();
        assert_eq!(d.mission_state("m1"), Stopping);
        assert_eq!(d.store().get_transitions("m1").unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn re_declaring_the_current_state_does_not_pile_up_rows() {
        use crate::state::MissionState::*;
        let root = tmp();
        Daemon::init(&root).unwrap();
        let d = Daemon::open(&root).unwrap();
        d.transition("m1", Running, "started").unwrap();
        d.transition("m1", Running, "still running").unwrap();
        d.transition("m1", Running, "still running").unwrap();
        assert_eq!(d.store().get_transitions("m1").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn notifications_are_off_unless_the_user_turns_them_on() {
        // The daemon's one outbound network path. A default that phoned home would
        // break the promise the whole product is built on.
        let root = tmp();
        Daemon::init(&root).unwrap();
        let d = Daemon::open(&root).unwrap();
        assert!(!d.config().notify.is_enabled());
        assert!(d.status().notifications.contains("Nothing leaves this machine"));

        // With notifications off, asking to notify sends nothing and says so.
        let r = d.morning_report("m1", root.to_str().unwrap());
        assert_eq!(d.notify_report(&r).unwrap(), false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn enabling_notifications_states_exactly_what_would_be_sent() {
        let root = tmp();
        Daemon::init(&root).unwrap();
        std::fs::write(
            Daemon::config_path(&root),
            json!({ "notify": { "kind": "ntfy", "topic": "my-topic" } }).to_string(),
        )
        .unwrap();
        let d = Daemon::open(&root).unwrap();
        assert!(d.config().notify.is_enabled());
        let shown = d.status().notifications;
        assert!(shown.contains("my-topic"), "{shown}");
        assert!(shown.contains("No file contents"), "{shown}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn no_validators_configured_is_not_a_pass() {
        // "Nobody proved it works" and "it works" must not be the same outcome.
        let (root, cwd) = tmp_repo();
        Daemon::init(&root).unwrap();
        let d = Daemon::open(&root).unwrap();
        std::fs::write(root.join("work.txt"), "x\n").unwrap();

        let out = d.complete_unit("m1", "u1", &cwd, "unit 1").await;
        assert!(!out.gate_passed);
        assert!(out.checkpoint.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_validators_gates_completion_and_persists() {
        let root = tmp();
        std::fs::create_dir_all(root.join(".agentrelay")).unwrap();
        let cfg = json!({
            "program": "claude",
            "keep_awake": false,
            "validators": [
                {"kind": "tests", "command": "node --version"},
                {"kind": "build", "command": "this-command-does-not-exist-xyz-123"}
            ]
        });
        std::fs::write(root.join(".agentrelay/config.json"), cfg.to_string()).unwrap();
        let d = Daemon::open(&root).unwrap();
        let results = d.run_validators("w1", ".").await;
        assert_eq!(results.len(), 2);
        assert!(!crate::validator::all_passed(&results)); // one failed -> unit not done
        let saved = d.store().get_validations("w1").unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|(v, p)| v == "tests" && *p));
        assert!(saved.iter().any(|(v, p)| v == "build" && !*p));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn run_turn_with_drives_an_adapter_and_logs() {
        let root = tmp();
        let d = Daemon::init(&root).unwrap();
        let turn = vec![
            AgentEvent::TurnStarted,
            AgentEvent::ToolPre { tool: "Bash".into(), args: json!({"command":"npm test"}) },
            AgentEvent::TurnCompleted { usage: Usage { input_tokens: 10, output_tokens: 5, ..Usage::zero() } },
        ];
        let fake = FakeAdapter::new("fake", vec![turn]);
        let out = d.run_turn_with(&fake, "m1", "do the thing", ".").await.unwrap();
        assert!(out.completed);
        assert!(out.violations.is_empty());
        assert_eq!(d.store().get_events("m1").unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }
}
