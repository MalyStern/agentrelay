//! Hybrid SQLite store (plan 02 §ה) — Rust port of `runner/src/store`, using
//! rusqlite (bundled SQLite). Append-only event log you can replay + mutable
//! state-tables as the source of truth for "now". Every log row carries
//! `payload_ver`; no hash-chain in v1.

use crate::validator::ValidationResult;
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mission_id TEXT, ts_utc TEXT NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL,
  payload TEXT NOT NULL, seq_in_turn INTEGER, payload_ver INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_events_mission ON events(mission_id, id);

CREATE TABLE IF NOT EXISTS state_transitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT, mission_id TEXT NOT NULL, ts_utc TEXT NOT NULL,
  from_state TEXT, to_state TEXT NOT NULL, reason_code TEXT, payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS checkpoints (
  id TEXT PRIMARY KEY, mission_id TEXT, work_unit_id TEXT, ts_utc TEXT,
  git_head TEXT, git_branch TEXT, git_status TEXT, files_hash TEXT,
  last_validation TEXT, session_state TEXT, summary TEXT, payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS validations (
  id INTEGER PRIMARY KEY AUTOINCREMENT, work_unit_id TEXT, ts_utc TEXT, validator TEXT,
  passed INTEGER, output TEXT, duration_ms INTEGER, payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS quota_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT, ts_utc TEXT, window_kind TEXT,
  used REAL, limit_val REAL, reset_at_utc TEXT, raw TEXT, payload_ver INTEGER NOT NULL DEFAULT 1
);

-- What content the agent already holds, per file. Survives a restart on purpose:
-- after a quota reset the whole point is to resume without re-reading the repo.
CREATE TABLE IF NOT EXISTS file_receipts (
  mission_id TEXT NOT NULL, path TEXT NOT NULL, hash TEXT NOT NULL,
  bytes INTEGER NOT NULL, turn INTEGER NOT NULL, ts_utc TEXT,
  payload_ver INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY (mission_id, path)
);

CREATE TABLE IF NOT EXISTS missions (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, contract TEXT NOT NULL,
  contract_ver INTEGER NOT NULL DEFAULT 1, status TEXT NOT NULL, autonomy TEXT NOT NULL,
  created_utc TEXT, updated_utc TEXT, timezone TEXT
);

CREATE TABLE IF NOT EXISTS work_units (
  id TEXT PRIMARY KEY, mission_id TEXT NOT NULL, seq INTEGER, title TEXT,
  contract TEXT NOT NULL, worker TEXT, status TEXT, worktree_path TEXT,
  session_ref TEXT, result_packet TEXT, attempts INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS meta ( key TEXT PRIMARY KEY, value TEXT );
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: i64,
    pub mission_id: Option<String>,
    pub ts_utc: String,
    pub actor: String,
    pub kind: String,
    pub payload: Value,
    pub seq_in_turn: Option<i64>,
    pub payload_ver: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendEvent {
    pub mission_id: Option<String>,
    pub actor: String,
    pub kind: String,
    pub payload: Value,
    pub seq_in_turn: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MissionRow {
    pub id: String,
    pub title: String,
    pub contract: Value,
    pub status: String,
    pub autonomy: String,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkUnitRow {
    pub id: String,
    pub mission_id: String,
    pub seq: Option<i64>,
    pub title: Option<String>,
    pub contract: Value,
    pub worker: Option<String>,
    pub status: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointRow {
    pub id: String,
    pub mission_id: Option<String>,
    pub git_head: Option<String>,
    pub git_branch: Option<String>,
    pub summary: Option<String>,
}

/// The connection is wrapped in a Mutex so `Store` is `Sync` — the daemon is shared
/// across concurrent IPC connections, and rusqlite's `Connection` is `!Sync`. Locks
/// are held only for the duration of one query.
pub struct Store {
    conn: std::sync::Mutex<Connection>,
    clock: Box<dyn Fn() -> String + Send + Sync>,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Store {
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::with_clock(Connection::open_in_memory()?, Box::new(now_iso))
    }

    pub fn open(path: &str) -> rusqlite::Result<Self> {
        Self::with_clock(Connection::open(path)?, Box::new(now_iso))
    }

    pub fn with_clock(conn: Connection, clock: Box<dyn Fn() -> String + Send + Sync>) -> rusqlite::Result<Self> {
        let store = Store {
            conn: std::sync::Mutex::new(conn),
            clock,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    pub fn schema_version(&self) -> i64 {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    // ── append-only log ──────────────────────────────────────────────────────
    pub fn append_event(&self, e: &AppendEvent) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO events(mission_id, ts_utc, actor, kind, payload, seq_in_turn, payload_ver)
             VALUES (?1,?2,?3,?4,?5,?6,1)",
            params![
                e.mission_id,
                (self.clock)(),
                e.actor,
                e.kind,
                e.payload.to_string(),
                e.seq_in_turn,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_events(&self, mission_id: &str) -> rusqlite::Result<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, mission_id, ts_utc, actor, kind, payload, seq_in_turn, payload_ver
             FROM events WHERE mission_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![mission_id], |r| {
            let payload: String = r.get(5)?;
            Ok(StoredEvent {
                id: r.get(0)?,
                mission_id: r.get(1)?,
                ts_utc: r.get(2)?,
                actor: r.get(3)?,
                kind: r.get(4)?,
                payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                seq_in_turn: r.get(6)?,
                payload_ver: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    pub fn record_transition(
        &self,
        mission_id: &str,
        from: Option<&str>,
        to: &str,
        reason_code: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO state_transitions(mission_id, ts_utc, from_state, to_state, reason_code)
             VALUES (?1,?2,?3,?4,?5)",
            params![mission_id, (self.clock)(), from, to, reason_code],
        )?;
        Ok(())
    }

    /// The state this mission was last recorded in, or None if it has never moved.
    pub fn latest_state(&self, mission_id: &str) -> Option<String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT to_state FROM state_transitions WHERE mission_id = ?1
                 ORDER BY id DESC LIMIT 1",
                params![mission_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
    }

    pub fn get_transitions(&self, mission_id: &str) -> rusqlite::Result<Vec<(Option<String>, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT from_state, to_state FROM state_transitions WHERE mission_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![mission_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    // ── state-tables ─────────────────────────────────────────────────────────
    pub fn upsert_mission(&self, m: &MissionRow) -> rusqlite::Result<()> {
        let now = (self.clock)();
        self.conn.lock().unwrap().execute(
            "INSERT INTO missions(id, title, contract, contract_ver, status, autonomy, created_utc, updated_utc, timezone)
             VALUES (?1,?2,?3,1,?4,?5,?6,?6,?7)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, contract=excluded.contract,
               status=excluded.status, autonomy=excluded.autonomy, updated_utc=excluded.updated_utc, timezone=excluded.timezone",
            params![m.id, m.title, m.contract.to_string(), m.status, m.autonomy, now, m.timezone],
        )?;
        Ok(())
    }

    pub fn get_mission(&self, id: &str) -> rusqlite::Result<Option<MissionRow>> {
        let r = self.conn.lock().unwrap().query_row(
            "SELECT id, title, contract, status, autonomy, timezone FROM missions WHERE id = ?1",
            params![id],
            |r| {
                let contract: String = r.get(2)?;
                Ok(MissionRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    contract: serde_json::from_str(&contract).unwrap_or(Value::Null),
                    status: r.get(3)?,
                    autonomy: r.get(4)?,
                    timezone: r.get(5)?,
                })
            },
        );
        match r {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn count_missions(&self) -> rusqlite::Result<i64> {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM missions", [], |r| r.get(0))
    }

    pub fn upsert_work_unit(&self, w: &WorkUnitRow) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO work_units(id, mission_id, seq, title, contract, worker, status, worktree_path)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET seq=excluded.seq, title=excluded.title, contract=excluded.contract,
               worker=excluded.worker, status=excluded.status, worktree_path=excluded.worktree_path",
            params![w.id, w.mission_id, w.seq, w.title, w.contract.to_string(), w.worker, w.status, w.worktree_path],
        )?;
        Ok(())
    }

    pub fn list_work_units(&self, mission_id: &str) -> rusqlite::Result<Vec<WorkUnitRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, mission_id, seq, title, contract, worker, status, worktree_path
             FROM work_units WHERE mission_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![mission_id], |r| {
            let contract: String = r.get(4)?;
            Ok(WorkUnitRow {
                id: r.get(0)?,
                mission_id: r.get(1)?,
                seq: r.get(2)?,
                title: r.get(3)?,
                contract: serde_json::from_str(&contract).unwrap_or(Value::Null),
                worker: r.get(5)?,
                status: r.get(6)?,
                worktree_path: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    pub fn save_checkpoint(&self, cp: &CheckpointRow) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO checkpoints(id, mission_id, ts_utc, git_head, git_branch, summary)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![cp.id, cp.mission_id, (self.clock)(), cp.git_head, cp.git_branch, cp.summary],
        )?;
        Ok(())
    }

    pub fn latest_checkpoint(&self, mission_id: &str) -> rusqlite::Result<Option<CheckpointRow>> {
        let r = self.conn.lock().unwrap().query_row(
            "SELECT id, mission_id, git_head, git_branch, summary FROM checkpoints
             WHERE mission_id = ?1 ORDER BY ts_utc DESC, rowid DESC LIMIT 1",
            params![mission_id],
            |r| {
                Ok(CheckpointRow {
                    id: r.get(0)?,
                    mission_id: r.get(1)?,
                    git_head: r.get(2)?,
                    git_branch: r.get(3)?,
                    summary: r.get(4)?,
                })
            },
        );
        match r {
            Ok(c) => Ok(Some(c)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ── file receipts (Context Optimizer) ─────────────────────────────────────

    /// Upsert on (mission, path): only the newest content per file matters, and
    /// keeping history here would grow without bound for no benefit.
    pub fn save_receipt(&self, mission_id: &str, r: &crate::context::FileReceipt) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO file_receipts(mission_id, path, hash, bytes, turn, ts_utc)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(mission_id, path) DO UPDATE SET
               hash=excluded.hash, bytes=excluded.bytes, turn=excluded.turn, ts_utc=excluded.ts_utc",
            params![mission_id, r.path, r.hash, r.bytes as i64, r.turn as i64, (self.clock)()],
        )?;
        Ok(())
    }

    /// Add what a turn actually cost to the mission's running total.
    ///
    /// Stored as a running sum rather than recomputed from events, so the
    /// kill-switch can answer "how much has this cost" in one query at the moment
    /// it has to decide — and so the answer survives a restart, which is exactly
    /// when a cap is easiest to lose track of.
    pub fn add_cost_usd(&self, mission_id: &str, usd: f64) -> rusqlite::Result<()> {
        if !(usd.is_finite() && usd > 0.0) {
            return Ok(());
        }
        self.conn.lock().unwrap().execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET
               value = CAST(CAST(meta.value AS REAL) + ?3 AS TEXT)",
            params![format!("cost_usd:{mission_id}"), usd.to_string(), usd],
        )?;
        Ok(())
    }

    pub fn total_cost_usd(&self, mission_id: &str) -> f64 {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![format!("cost_usd:{mission_id}")],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|c| c.is_finite())
            .unwrap_or(0.0)
    }

    /// The most expensive single turn seen for this mission — the honest estimate
    /// of what the next one might cost.
    pub fn max_turn_cost_usd(&self, mission_id: &str) -> f64 {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![format!("max_turn_usd:{mission_id}")],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|c| c.is_finite())
            .unwrap_or(0.0)
    }

    pub fn note_turn_cost_usd(&self, mission_id: &str, usd: f64) -> rusqlite::Result<()> {
        if !(usd.is_finite() && usd > 0.0) {
            return Ok(());
        }
        if usd > self.max_turn_cost_usd(mission_id) {
            self.conn.lock().unwrap().execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![format!("max_turn_usd:{mission_id}"), usd.to_string()],
            )?;
        }
        Ok(())
    }

    /// Record where the quota wall is, as the provider reported it.
    ///
    /// Worth persisting even when nothing is wrong: it is how the supervisor knows,
    /// before starting a unit at 03:00, whether the window reopens in ten minutes or
    /// four hours — which is the difference between waiting and wasting the night.
    pub fn save_quota_snapshot(
        &self,
        provider: &str,
        window_kind: &str,
        reset_at_utc: &str,
    ) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO quota_snapshots(provider, ts_utc, window_kind, reset_at_utc)
             VALUES (?1,?2,?3,?4)",
            params![provider, (self.clock)(), window_kind, reset_at_utc],
        )?;
        Ok(())
    }

    /// The most recently reported reset time for a provider, if any.
    pub fn latest_quota_reset(&self, provider: &str) -> Option<String> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT reset_at_utc FROM quota_snapshots WHERE provider = ?1
                 ORDER BY ts_utc DESC, id DESC LIMIT 1",
                params![provider],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// Remember which paths were already dirty when a unit started — the user's own
    /// uncommitted work, which the supervisor must never commit on their behalf.
    pub fn save_unit_baseline(&self, mission_id: &str, unit_id: &str, paths: &[String]) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![
                format!("baseline:{mission_id}:{unit_id}"),
                serde_json::to_string(paths).unwrap_or_else(|_| "[]".into())
            ],
        )?;
        Ok(())
    }

    /// `None` means no baseline was recorded — which is NOT the same as "the tree
    /// was clean", and callers must treat the difference as significant.
    pub fn get_unit_baseline(&self, mission_id: &str, unit_id: &str) -> Option<Vec<String>> {
        let r: rusqlite::Result<String> = self.conn.lock().unwrap().query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![format!("baseline:{mission_id}:{unit_id}")],
            |r| r.get(0),
        );
        r.ok().and_then(|v| serde_json::from_str(&v).ok())
    }

    /// Running total of bytes NOT re-sent because a file was unchanged. Counted at
    /// the moment each skip happens — the only way this number can be true.
    pub fn add_saved_bytes(&self, mission_id: &str, n: usize) -> rusqlite::Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.conn.lock().unwrap().execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(meta.value AS INTEGER) + ?3 AS TEXT)",
            params![format!("saved_bytes:{mission_id}"), n.to_string(), n as i64],
        )?;
        Ok(())
    }

    pub fn get_saved_bytes(&self, mission_id: &str) -> rusqlite::Result<usize> {
        let r: rusqlite::Result<String> = self.conn.lock().unwrap().query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![format!("saved_bytes:{mission_id}")],
            |r| r.get(0),
        );
        match r {
            Ok(v) => Ok(v.parse().unwrap_or(0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    pub fn get_receipts(&self, mission_id: &str) -> rusqlite::Result<Vec<crate::context::FileReceipt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, hash, bytes, turn FROM file_receipts WHERE mission_id = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![mission_id], |r| {
            Ok(crate::context::FileReceipt {
                path: r.get(0)?,
                hash: r.get(1)?,
                bytes: r.get::<_, i64>(2)? as usize,
                turn: r.get::<_, i64>(3)? as u32,
            })
        })?;
        rows.collect()
    }

    // ── validations (Watchtower layer-2 evidence) ─────────────────────────────
    pub fn save_validation(&self, work_unit_id: &str, r: &ValidationResult) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO validations(work_unit_id, ts_utc, validator, passed, output, duration_ms)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![work_unit_id, (self.clock)(), r.validator, r.passed as i64, r.output, r.duration_ms as i64],
        )?;
        Ok(())
    }

    pub fn get_validations(&self, work_unit_id: &str) -> rusqlite::Result<Vec<(String, bool)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT validator, passed FROM validations WHERE work_unit_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![work_unit_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    fn store_with_counter_clock() -> Store {
        let n = Arc::new(AtomicI64::new(0));
        let clock = Box::new(move || {
            let ms = 1_767_225_600_000 + n.fetch_add(1, Ordering::SeqCst) * 1000;
            chrono::Utc
                .timestamp_millis_opt(ms)
                .single()
                .unwrap()
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        });
        Store::with_clock(Connection::open_in_memory().unwrap(), clock).unwrap()
    }

    #[test]
    fn migration_sets_version() {
        assert_eq!(store_with_counter_clock().schema_version(), SCHEMA_VERSION);
    }

    #[test]
    fn append_log_is_ordered_with_json_payloads() {
        let s = store_with_counter_clock();
        s.append_event(&AppendEvent { mission_id: Some("m1".into()), actor: "system".into(), kind: "mission.created".into(), payload: json!({"title":"X"}), seq_in_turn: None }).unwrap();
        s.append_event(&AppendEvent { mission_id: Some("m1".into()), actor: "claude".into(), kind: "tool.pre".into(), payload: json!({"tool":"Bash"}), seq_in_turn: Some(0) }).unwrap();
        s.append_event(&AppendEvent { mission_id: Some("m2".into()), actor: "system".into(), kind: "mission.created".into(), payload: json!({}), seq_in_turn: None }).unwrap();
        let evs = s.get_events("m1").unwrap();
        assert_eq!(evs.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["mission.created", "tool.pre"]);
        assert_eq!(evs[0].payload, json!({"title":"X"}));
        assert_eq!(evs[1].seq_in_turn, Some(0));
        assert!(evs[0].id < evs[1].id);
        assert_eq!(s.get_events("m2").unwrap().len(), 1);
    }

    #[test]
    fn transitions_history() {
        let s = store_with_counter_clock();
        s.record_transition("m1", None, "planned", None).unwrap();
        s.record_transition("m1", Some("planned"), "running", None).unwrap();
        s.record_transition("m1", Some("running"), "waiting_quota", Some("quota.rate_limited")).unwrap();
        let t = s.get_transitions("m1").unwrap();
        assert_eq!(t.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), ["planned", "running", "waiting_quota"]);
    }

    #[test]
    fn mission_upsert() {
        let s = store_with_counter_clock();
        s.upsert_mission(&MissionRow { id: "m1".into(), title: "Nightly".into(), contract: json!({"goal":"x"}), status: "planned".into(), autonomy: "overnight".into(), timezone: None }).unwrap();
        assert_eq!(s.get_mission("m1").unwrap().unwrap().status, "planned");
        s.upsert_mission(&MissionRow { id: "m1".into(), title: "Nightly".into(), contract: json!({"goal":"x"}), status: "running".into(), autonomy: "overnight".into(), timezone: None }).unwrap();
        assert_eq!(s.get_mission("m1").unwrap().unwrap().status, "running");
        assert_eq!(s.count_missions().unwrap(), 1);
    }

    #[test]
    fn work_units_ordered() {
        let s = store_with_counter_clock();
        s.upsert_work_unit(&WorkUnitRow { id: "w2".into(), mission_id: "m1".into(), seq: Some(2), title: None, contract: json!({}), worker: None, status: Some("pending".into()), worktree_path: None }).unwrap();
        s.upsert_work_unit(&WorkUnitRow { id: "w1".into(), mission_id: "m1".into(), seq: Some(1), title: None, contract: json!({}), worker: Some("codex".into()), status: Some("running".into()), worktree_path: Some("/wt/1".into()) }).unwrap();
        let units = s.list_work_units("m1").unwrap();
        assert_eq!(units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(), ["w1", "w2"]);
        assert_eq!(units[0].worker.as_deref(), Some("codex"));
        assert_eq!(units[1].worker, None);
    }

    #[test]
    fn latest_checkpoint_by_time() {
        let s = store_with_counter_clock();
        s.save_checkpoint(&CheckpointRow { id: "cp1".into(), mission_id: Some("m1".into()), git_head: Some("aaa".into()), git_branch: None, summary: Some("did A".into()) }).unwrap();
        s.save_checkpoint(&CheckpointRow { id: "cp2".into(), mission_id: Some("m1".into()), git_head: Some("bbb".into()), git_branch: None, summary: Some("did B".into()) }).unwrap();
        assert_eq!(s.latest_checkpoint("m1").unwrap().unwrap().id, "cp2");
        assert!(s.latest_checkpoint("nope").unwrap().is_none());
    }
}
