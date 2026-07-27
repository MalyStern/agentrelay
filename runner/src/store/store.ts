// A thin, typed store over node:sqlite implementing the hybrid model (plan 02 §ה):
// an append-only event log you can replay, plus mutable state-tables that are the
// source of truth for "now". This is the TS reference the Rust daemon mirrors; it's
// fully testable in-memory here.

import { DatabaseSync } from 'node:sqlite'
import { SCHEMA, SCHEMA_VERSION } from './schema.js'

export interface AppendEventInput {
  missionId?: string
  actor: string
  kind: string
  payload: unknown
  seqInTurn?: number
  payloadVer?: number
}

export interface StoredEvent {
  id: number
  missionId: string | null
  tsUtc: string
  actor: string
  kind: string
  payload: unknown
  seqInTurn: number | null
  payloadVer: number
}

export interface MissionRow {
  id: string
  title: string
  contract: unknown
  contractVer?: number
  status: string
  autonomy: string
  timezone?: string
}

export interface WorkUnitRow {
  id: string
  missionId: string
  seq?: number
  title?: string
  contract: unknown
  worker?: string
  status?: string
  worktreePath?: string
  sessionRef?: string
  resultPacket?: unknown
  attempts?: number
}

export interface CheckpointRow {
  id: string
  missionId?: string
  workUnitId?: string
  gitHead?: string
  gitBranch?: string
  gitStatus?: string
  filesHash?: string
  summary?: string
}

const j = (v: unknown): string => JSON.stringify(v ?? null)
const p = <T>(v: unknown): T => (typeof v === 'string' ? (JSON.parse(v) as T) : (v as T))

export class Store {
  private readonly db: DatabaseSync
  private readonly clock: () => string

  constructor(location = ':memory:', clock: () => string = () => new Date().toISOString()) {
    this.db = new DatabaseSync(location)
    this.clock = clock
    this.migrate()
  }

  private migrate(): void {
    this.db.exec(SCHEMA)
    this.db.prepare('INSERT OR REPLACE INTO meta(key, value) VALUES (?, ?)').run('schema_version', String(SCHEMA_VERSION))
  }

  schemaVersion(): number {
    const row = this.db.prepare('SELECT value FROM meta WHERE key = ?').get('schema_version') as { value?: string } | undefined
    return row?.value ? Number(row.value) : 0
  }

  // ── append-only log ──────────────────────────────────────────────────────
  appendEvent(input: AppendEventInput): number {
    const r = this.db
      .prepare('INSERT INTO events(mission_id, ts_utc, actor, kind, payload, seq_in_turn, payload_ver) VALUES (?,?,?,?,?,?,?)')
      .run(
        input.missionId ?? null,
        this.clock(),
        input.actor,
        input.kind,
        j(input.payload),
        input.seqInTurn ?? null,
        input.payloadVer ?? 1,
      )
    return Number(r.lastInsertRowid)
  }

  getEvents(missionId: string): StoredEvent[] {
    const rows = this.db.prepare('SELECT * FROM events WHERE mission_id = ? ORDER BY id').all(missionId) as Record<string, unknown>[]
    return rows.map((row) => ({
      id: Number(row.id),
      missionId: (row.mission_id as string | null) ?? null,
      tsUtc: row.ts_utc as string,
      actor: row.actor as string,
      kind: row.kind as string,
      payload: p(row.payload),
      seqInTurn: row.seq_in_turn == null ? null : Number(row.seq_in_turn),
      payloadVer: Number(row.payload_ver),
    }))
  }

  recordTransition(missionId: string, from: string | null, to: string, reasonCode?: string): void {
    this.db
      .prepare('INSERT INTO state_transitions(mission_id, ts_utc, from_state, to_state, reason_code) VALUES (?,?,?,?,?)')
      .run(missionId, this.clock(), from, to, reasonCode ?? null)
  }

  getTransitions(missionId: string): { from: string | null; to: string; reasonCode: string | null; tsUtc: string }[] {
    const rows = this.db.prepare('SELECT * FROM state_transitions WHERE mission_id = ? ORDER BY id').all(missionId) as Record<string, unknown>[]
    return rows.map((r) => ({
      from: (r.from_state as string | null) ?? null,
      to: r.to_state as string,
      reasonCode: (r.reason_code as string | null) ?? null,
      tsUtc: r.ts_utc as string,
    }))
  }

  // ── state-tables ─────────────────────────────────────────────────────────
  upsertMission(m: MissionRow): void {
    const now = this.clock()
    this.db
      .prepare(
        `INSERT INTO missions(id, title, contract, contract_ver, status, autonomy, created_utc, updated_utc, timezone)
         VALUES (?,?,?,?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET
           title=excluded.title, contract=excluded.contract, contract_ver=excluded.contract_ver,
           status=excluded.status, autonomy=excluded.autonomy, updated_utc=excluded.updated_utc, timezone=excluded.timezone`,
      )
      .run(m.id, m.title, j(m.contract), m.contractVer ?? 1, m.status, m.autonomy, now, now, m.timezone ?? null)
  }

  getMission(id: string): MissionRow | null {
    const r = this.db.prepare('SELECT * FROM missions WHERE id = ?').get(id) as Record<string, unknown> | undefined
    if (!r) return null
    return {
      id: r.id as string,
      title: r.title as string,
      contract: p(r.contract),
      contractVer: Number(r.contract_ver),
      status: r.status as string,
      autonomy: r.autonomy as string,
      ...(r.timezone ? { timezone: r.timezone as string } : {}),
    }
  }

  listMissions(): MissionRow[] {
    const rows = this.db.prepare('SELECT id FROM missions ORDER BY created_utc').all() as { id: string }[]
    return rows.map((r) => this.getMission(r.id)!).filter(Boolean)
  }

  upsertWorkUnit(w: WorkUnitRow): void {
    this.db
      .prepare(
        `INSERT INTO work_units(id, mission_id, seq, title, contract, worker, status, worktree_path, session_ref, result_packet, attempts)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET
           seq=excluded.seq, title=excluded.title, contract=excluded.contract, worker=excluded.worker,
           status=excluded.status, worktree_path=excluded.worktree_path, session_ref=excluded.session_ref,
           result_packet=excluded.result_packet, attempts=excluded.attempts`,
      )
      .run(
        w.id, w.missionId, w.seq ?? null, w.title ?? null, j(w.contract), w.worker ?? null, w.status ?? null,
        w.worktreePath ?? null, w.sessionRef ?? null, w.resultPacket == null ? null : j(w.resultPacket), w.attempts ?? 0,
      )
  }

  listWorkUnits(missionId: string): WorkUnitRow[] {
    const rows = this.db.prepare('SELECT * FROM work_units WHERE mission_id = ? ORDER BY seq').all(missionId) as Record<string, unknown>[]
    return rows.map((r) => {
      const w: WorkUnitRow = { id: r.id as string, missionId: r.mission_id as string, contract: p(r.contract), attempts: Number(r.attempts ?? 0) }
      if (r.seq != null) w.seq = Number(r.seq)
      if (r.title != null) w.title = r.title as string
      if (r.worker != null) w.worker = r.worker as string
      if (r.status != null) w.status = r.status as string
      if (r.worktree_path != null) w.worktreePath = r.worktree_path as string
      if (r.session_ref != null) w.sessionRef = r.session_ref as string
      if (r.result_packet != null) w.resultPacket = p(r.result_packet)
      return w
    })
  }

  saveCheckpoint(cp: CheckpointRow): void {
    this.db
      .prepare(
        `INSERT INTO checkpoints(id, mission_id, work_unit_id, ts_utc, git_head, git_branch, git_status, files_hash, summary)
         VALUES (?,?,?,?,?,?,?,?,?)`,
      )
      .run(cp.id, cp.missionId ?? null, cp.workUnitId ?? null, this.clock(), cp.gitHead ?? null, cp.gitBranch ?? null, cp.gitStatus ?? null, cp.filesHash ?? null, cp.summary ?? null)
  }

  latestCheckpoint(missionId: string): (CheckpointRow & { tsUtc: string }) | null {
    const r = this.db.prepare('SELECT * FROM checkpoints WHERE mission_id = ? ORDER BY ts_utc DESC, rowid DESC LIMIT 1').get(missionId) as
      | Record<string, unknown>
      | undefined
    if (!r) return null
    return {
      id: r.id as string,
      ...(r.mission_id ? { missionId: r.mission_id as string } : {}),
      ...(r.work_unit_id ? { workUnitId: r.work_unit_id as string } : {}),
      ...(r.git_head ? { gitHead: r.git_head as string } : {}),
      ...(r.git_branch ? { gitBranch: r.git_branch as string } : {}),
      ...(r.summary ? { summary: r.summary as string } : {}),
      tsUtc: r.ts_utc as string,
    }
  }

  close(): void {
    this.db.close()
  }
}
