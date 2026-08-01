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
        StatusReport {
            db_path: Self::db_path(&self.root).to_string_lossy().into_owned(),
            schema_version: self.store.schema_version(),
            missions: self.store.count_missions().unwrap_or(0),
            keep_awake: self.config.keep_awake,
            program: self.config.program.clone(),
        }
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
    pub async fn complete_unit(&self, mission_id: &str, unit_id: &str, cwd: &str, summary: &str) -> UnitOutcome {
        let validations = self.run_validators(unit_id, cwd).await;
        let gate_passed = crate::validator::all_passed(&validations);

        // No validators configured is NOT a pass — there is no evidence either way,
        // and silently checkpointing would present an unverified state as verified.
        if !gate_passed {
            return UnitOutcome { validations, gate_passed, checkpoint: None, checkpoint_error: None };
        }

        match crate::checkpoint::create(cwd, unit_id, summary) {
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
                UnitOutcome { validations, gate_passed, checkpoint: Some(cp), checkpoint_error: None }
            }
            // A repo we can't checkpoint (not a repo, git missing) must not be
            // reported as a failed unit — the work passed; only the safety net is
            // missing, and the user needs to be told which of the two happened.
            Err(e) => UnitOutcome {
                validations,
                gate_passed,
                checkpoint: None,
                checkpoint_error: Some(e.to_string()),
            },
        }
    }
}

/// The result of finishing a unit: what the evidence said, and what was recorded.
#[derive(Debug)]
pub struct UnitOutcome {
    pub validations: Vec<crate::validator::ValidationResult>,
    pub gate_passed: bool,
    pub checkpoint: Option<crate::checkpoint::Checkpoint>,
    pub checkpoint_error: Option<String>,
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

        // The agent changed something and the validators pass -> committed.
        std::fs::write(root.join("work.txt"), "the agent's output\n").unwrap();
        let out = d.complete_unit("m1", "u1", &cwd, "unit 1: add work.txt").await;
        assert!(out.gate_passed);
        let cp = out.checkpoint.expect("a passing unit is checkpointed");
        assert!(cp.created);
        assert!(crate::checkpoint::is_ours(&cwd, &cp.head));
        assert!(crate::checkpoint::is_clean(&cwd).unwrap());
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
        assert_eq!(crate::checkpoint::head(&cwd).unwrap(), before, "HEAD must not move");
        // The work itself is untouched — visible in the tree for the human to judge.
        assert!(root.join("broken.txt").exists());
        assert!(!crate::checkpoint::is_clean(&cwd).unwrap());

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
