//! Hybrid SQLite store (plan 02 §ה) — Rust port of `runner/src/store`, using
//! rusqlite (bundled SQLite). Append-only event log you can replay + mutable
//! state-tables as the source of truth for "now". Every log row carries
//! `payload_ver`; no hash-chain in v1.

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
