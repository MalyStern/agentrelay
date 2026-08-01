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
        let config = std::fs::read_to_string(Self::config_path(&root))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
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
