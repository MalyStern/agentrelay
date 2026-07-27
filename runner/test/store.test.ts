import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { SCHEMA_VERSION, Store } from '../src/index.js'

// Deterministic, strictly-increasing clock.
function makeClock(): () => string {
  const base = Date.parse('2026-01-01T00:00:00.000Z')
  let n = 0
  return () => new Date(base + n++ * 1000).toISOString()
}

let store: Store
beforeEach(() => {
  store = new Store(':memory:', makeClock())
})
afterEach(() => store.close())

describe('Store: migration', () => {
  it('applies the schema and records the version', () => {
    expect(store.schemaVersion()).toBe(SCHEMA_VERSION)
  })
})

describe('Store: append-only event log (replayable history)', () => {
  it('appends and reads back in monotonic order with JSON payloads', () => {
    store.appendEvent({ missionId: 'm1', actor: 'system', kind: 'mission.created', payload: { title: 'X' } })
    store.appendEvent({ missionId: 'm1', actor: 'claude', kind: 'tool.pre', payload: { tool: 'Bash' }, seqInTurn: 0 })
    store.appendEvent({ missionId: 'm2', actor: 'system', kind: 'mission.created', payload: {} })

    const evs = store.getEvents('m1')
    expect(evs.map((e) => e.kind)).toEqual(['mission.created', 'tool.pre'])
    expect(evs[0]!.payload).toEqual({ title: 'X' })
    expect(evs[1]!.seqInTurn).toBe(0)
    expect(evs[0]!.id).toBeLessThan(evs[1]!.id) // monotonic
    expect(evs.every((e) => e.payloadVer === 1)).toBe(true)
    expect(store.getEvents('m2')).toHaveLength(1)
  })
})

describe('Store: state transitions', () => {
  it('records and reads a mission state history', () => {
    store.recordTransition('m1', null, 'planned')
    store.recordTransition('m1', 'planned', 'running')
    store.recordTransition('m1', 'running', 'waiting_quota', 'quota.rate_limited')
    const t = store.getTransitions('m1')
    expect(t.map((x) => x.to)).toEqual(['planned', 'running', 'waiting_quota'])
    expect(t[2]!.reasonCode).toBe('quota.rate_limited')
  })
})

describe('Store: missions state-table (upsert)', () => {
  it('inserts then updates the same row', () => {
    store.upsertMission({ id: 'm1', title: 'Nightly refactor', contract: { goal: 'x' }, status: 'planned', autonomy: 'overnight' })
    expect(store.getMission('m1')).toMatchObject({ title: 'Nightly refactor', status: 'planned', contract: { goal: 'x' } })

    store.upsertMission({ id: 'm1', title: 'Nightly refactor', contract: { goal: 'x' }, status: 'running', autonomy: 'overnight' })
    expect(store.getMission('m1')!.status).toBe('running')
    expect(store.listMissions()).toHaveLength(1) // upsert, not duplicate
  })
})

describe('Store: work units', () => {
  it('upserts and lists ordered by seq, with optional fields', () => {
    store.upsertWorkUnit({ id: 'w2', missionId: 'm1', seq: 2, contract: {}, status: 'pending' })
    store.upsertWorkUnit({ id: 'w1', missionId: 'm1', seq: 1, contract: {}, worker: 'codex', status: 'running', worktreePath: '/wt/1' })
    const units = store.listWorkUnits('m1')
    expect(units.map((u) => u.id)).toEqual(['w1', 'w2'])
    expect(units[0]).toMatchObject({ worker: 'codex', worktreePath: '/wt/1' })
    expect(units[1]!.worker).toBeUndefined()
  })
})

describe('Store: checkpoints', () => {
  it('returns the latest checkpoint by time', () => {
    store.saveCheckpoint({ id: 'cp1', missionId: 'm1', gitHead: 'aaa', summary: 'did A' })
    store.saveCheckpoint({ id: 'cp2', missionId: 'm1', gitHead: 'bbb', summary: 'did B' })
    const latest = store.latestCheckpoint('m1')
    expect(latest).toMatchObject({ id: 'cp2', gitHead: 'bbb', summary: 'did B' })
    expect(store.latestCheckpoint('nope')).toBeNull()
  })
})
