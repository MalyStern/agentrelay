// The SQLite schema (plan 02 §ה — hybrid: append-only-log + mutable state-tables;
// 00 §4 tables + 02 new tables). Kept as a single DDL string so it's the one source
// of truth for both this TS reference store and the Rust daemon. Every log row
// carries `payload_ver`; there is deliberately NO hash-chain in v1 (fix #12).

export const SCHEMA_VERSION = 1

export const SCHEMA = /* sql */ `
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- ── append-only log: real history you must be able to replay ──────────────────
CREATE TABLE IF NOT EXISTS events (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,  -- monotonic = truth ordering
  mission_id  TEXT,
  ts_utc      TEXT NOT NULL,
  actor       TEXT NOT NULL,        -- user|claude|codex|system|validator|scheduler
  kind        TEXT NOT NULL,        -- events.kind taxonomy
  payload     TEXT NOT NULL,        -- JSON
  seq_in_turn INTEGER,
  payload_ver INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_events_mission ON events(mission_id, id);

CREATE TABLE IF NOT EXISTS state_transitions (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  mission_id  TEXT NOT NULL, ts_utc TEXT NOT NULL,
  from_state  TEXT, to_state TEXT NOT NULL, reason_code TEXT,
  payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS checkpoints (
  id            TEXT PRIMARY KEY,
  mission_id    TEXT, work_unit_id TEXT, ts_utc TEXT,
  git_head TEXT, git_branch TEXT, git_status TEXT, files_hash TEXT,
  last_validation TEXT, session_state TEXT, summary TEXT,
  payload_ver   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS quota_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  provider TEXT, ts_utc TEXT, window_kind TEXT,
  used REAL, limit_val REAL, reset_at_utc TEXT, raw TEXT,
  payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS validations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  work_unit_id TEXT, ts_utc TEXT, validator TEXT,
  passed INTEGER, output TEXT, duration_ms INTEGER,
  payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS timeline (       -- light: turn summaries, not text deltas
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  mission_id TEXT, ts_utc TEXT, summary TEXT, kind TEXT,
  payload_ver INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts_utc TEXT, actor TEXT, action TEXT, detail TEXT,
  payload_ver INTEGER NOT NULL DEFAULT 1
);

-- ── mutable state-tables: source of truth for "now" ───────────────────────────
CREATE TABLE IF NOT EXISTS missions (
  id TEXT PRIMARY KEY, title TEXT NOT NULL,
  contract TEXT NOT NULL, contract_ver INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL, autonomy TEXT NOT NULL,
  created_utc TEXT, updated_utc TEXT, timezone TEXT
);

CREATE TABLE IF NOT EXISTS work_units (
  id TEXT PRIMARY KEY, mission_id TEXT NOT NULL, seq INTEGER, title TEXT,
  contract TEXT NOT NULL, worker TEXT, status TEXT,
  worktree_path TEXT, session_ref TEXT, result_packet TEXT, attempts INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY, provider TEXT, external_id TEXT, work_unit_id TEXT,
  cwd TEXT, git_branch TEXT, git_head TEXT, last_checkpoint_id TEXT,
  state TEXT, created_utc TEXT, updated_utc TEXT
);

CREATE TABLE IF NOT EXISTS processes (
  pid INTEGER, mission_id TEXT, work_unit_id TEXT, pgid INTEGER,
  cmd TEXT, role TEXT, started_utc TEXT, alive INTEGER
);

CREATE TABLE IF NOT EXISTS policies (
  id TEXT PRIMARY KEY, mission_id TEXT, rule_kind TEXT, spec TEXT, enabled INTEGER DEFAULT 1
);

CREATE TABLE IF NOT EXISTS plugins (
  id TEXT PRIMARY KEY, kind TEXT, manifest TEXT, signature TEXT,
  granted_perms TEXT, enabled INTEGER, source TEXT
);

CREATE TABLE IF NOT EXISTS scheduled_prompts (
  id TEXT PRIMARY KEY, mission_id TEXT, target TEXT, trigger TEXT NOT NULL,
  body TEXT NOT NULL, condition TEXT, valid_until TEXT, priority INTEGER,
  mission_ver INTEGER, status TEXT
);

CREATE TABLE IF NOT EXISTS notifications (
  id INTEGER PRIMARY KEY AUTOINCREMENT, mission_id TEXT, ts_utc TEXT,
  event_kind TEXT, channel TEXT, importance TEXT, delivered INTEGER, ack INTEGER
);

-- ── 02 new tables: security / cost / isolation / i18n ─────────────────────────
CREATE TABLE IF NOT EXISTS credentials (
  id TEXT PRIMARY KEY, name TEXT, kind TEXT, scope TEXT,
  keyring_ref TEXT, rotation_due TEXT, mission_scope TEXT   -- secret is NEVER stored here
);

CREATE TABLE IF NOT EXISTS isolation_rings (
  id TEXT PRIMARY KEY, mission_id TEXT, kind TEXT,
  container_id TEXT, egress_allowlist TEXT, workspace_mount TEXT, status TEXT
);

CREATE TABLE IF NOT EXISTS cost_ledger (
  id INTEGER PRIMARY KEY AUTOINCREMENT, mission_id TEXT, provider TEXT, ts_utc TEXT,
  tokens_in INTEGER, tokens_out INTEGER, usd_est REAL, provenance TEXT  -- measured|estimated
);

CREATE TABLE IF NOT EXISTS external_resources (
  id TEXT PRIMARY KEY, mission_id TEXT, kind TEXT,  -- container|wsl_distro|mcp_sse|daemon
  handle TEXT, alive INTEGER, verified_utc TEXT
);

CREATE TABLE IF NOT EXISTS locales (
  key TEXT PRIMARY KEY, value TEXT   -- ui_lang|agent_lang|output_lang per mission
);

CREATE TABLE IF NOT EXISTS meta ( key TEXT PRIMARY KEY, value TEXT );
`
