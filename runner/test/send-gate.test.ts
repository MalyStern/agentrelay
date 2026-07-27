import { describe, expect, it } from 'vitest'
import { evaluateSendGate, isTriggerDue, selectNextToSend, type ScheduledPrompt, type SendContext } from '../src/index.js'

const base: ScheduledPrompt = {
  id: 'p1',
  target: 'claude',
  trigger: { type: 'at', at: '2026-07-27T09:00:00Z' },
  body: 'continue',
  priority: 50,
  missionVer: 1,
  status: 'queued',
}
const ctx = (over: Partial<SendContext> = {}): SendContext => ({ nowUtc: '2026-07-27T09:00:00Z', currentMissionVer: 1, ...over })

describe('isTriggerDue', () => {
  it('at / after / on_event / cron', () => {
    expect(isTriggerDue({ type: 'at', at: '2026-07-27T09:00:00Z' }, ctx({ nowUtc: '2026-07-27T09:00:01Z' }))).toBe(true)
    expect(isTriggerDue({ type: 'at', at: '2026-07-27T10:00:00Z' }, ctx())).toBe(false)
    expect(isTriggerDue({ type: 'after', sinceUtc: '2026-07-27T08:00:00Z', afterMs: 3_600_000 }, ctx())).toBe(true)
    expect(isTriggerDue({ type: 'on_event', event: 'ci.green' }, ctx({ firedEvent: 'ci.green' }))).toBe(true)
    expect(isTriggerDue({ type: 'on_event', event: 'ci.green' }, ctx({ firedEvent: 'ci.red' }))).toBe(false)
    expect(isTriggerDue({ type: 'cron', expr: '0 9 * * *' }, ctx({ cronDue: true }))).toBe(true)
  })
})

describe('evaluateSendGate', () => {
  it('sends a clear, due prompt', () => {
    expect(evaluateSendGate(base, ctx())).toBe('send')
  })
  it('holds a prompt whose trigger is not due', () => {
    expect(evaluateSendGate({ ...base, trigger: { type: 'at', at: '2026-07-27T10:00:00Z' } }, ctx())).toBe('not_due')
  })
  it('expires past valid_until', () => {
    expect(evaluateSendGate({ ...base, validUntil: '2026-07-27T08:59:59Z' }, ctx())).toBe('expired')
  })
  it('supersedes a prompt from an older mission version (the anti-stale core)', () => {
    expect(evaluateSendGate({ ...base, missionVer: 1 }, ctx({ currentMissionVer: 2 }))).toBe('superseded')
  })
  it('skips when the pre-send condition is false', () => {
    expect(evaluateSendGate({ ...base, condition: 'tests_green' }, ctx({ conditionResult: false }))).toBe('condition_unmet')
    expect(evaluateSendGate({ ...base, condition: 'tests_green' }, ctx({ conditionResult: true }))).toBe('send')
  })
  it('respects terminal statuses', () => {
    expect(evaluateSendGate({ ...base, status: 'cancelled' }, ctx())).toBe('cancelled')
    expect(evaluateSendGate({ ...base, status: 'superseded' }, ctx())).toBe('superseded')
    expect(evaluateSendGate({ ...base, status: 'sent' }, ctx())).toBe('already_sent')
  })
})

describe('selectNextToSend', () => {
  it('returns the highest-priority sendable prompt, ignoring stale/not-due ones', () => {
    const prompts: ScheduledPrompt[] = [
      { ...base, id: 'low', priority: 10 },
      { ...base, id: 'high', priority: 90 },
      { ...base, id: 'stale', priority: 100, missionVer: 0 }, // superseded → excluded despite top priority
      { ...base, id: 'future', priority: 95, trigger: { type: 'at', at: '2026-07-27T23:00:00Z' } }, // not due
    ]
    expect(selectNextToSend(prompts, ctx({ currentMissionVer: 1 }))!.id).toBe('high')
  })
  it('returns null when nothing is sendable', () => {
    expect(selectNextToSend([{ ...base, status: 'cancelled' }], ctx())).toBeNull()
  })
})
