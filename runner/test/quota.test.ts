import { describe, expect, it } from 'vitest'
import {
  FIRST_TURN_READONLY,
  FakeAdapter,
  QuotaTracker,
  collect,
  evaluateToolPre,
  firstTurnReadOnlyGuards,
  interpretEvent,
  planResumeAfterRateLimit,
  sessionVerifyDeadline,
  type AgentEvent,
  type SessionHandle,
} from '../src/index.js'

const usage = { inputTokens: 100, outputTokens: 40, provenance: 'measured' as const }
const handle: SessionHandle = { id: 's', provider: 'fake', cwd: '/w' }

describe('interpretEvent', () => {
  it('extracts usage and rate-limit signals', () => {
    expect(interpretEvent({ type: 'token_usage', usage })).toEqual({ kind: 'usage', used: 140 })
    expect(interpretEvent({ type: 'rate_limited', resetAt: '2026-07-27T22:00:00Z', window: 'weekly' })).toEqual({
      kind: 'rate_limited',
      resetAtUtc: '2026-07-27T22:00:00Z',
      window: 'weekly',
    })
    expect(interpretEvent({ type: 'text_delta', text: 'x' })).toBeNull()
  })
})

describe('planResumeAfterRateLimit', () => {
  it('waits until reset + buffer', () => {
    const plan = planResumeAfterRateLimit('2026-07-27T22:00:00.000Z', '2026-07-27T21:00:00.000Z', { bufferMs: 60_000 })
    expect(plan.waitMs).toBe(61 * 60_000) // 60 min to reset + 1 min buffer
    expect(plan.resumeAtUtc).toBe('2026-07-27T22:01:00.000Z')
  })
  it('never returns a negative wait when the reset already passed', () => {
    const plan = planResumeAfterRateLimit('2026-07-27T20:00:00.000Z', '2026-07-27T21:00:00.000Z')
    expect(plan.waitMs).toBe(0)
  })
})

describe('sessionVerifyDeadline', () => {
  it('verifies before the reset window with lead time', () => {
    const d = sessionVerifyDeadline('2026-07-27T22:00:00.000Z', '2026-07-27T21:00:00.000Z', { leadMs: 5 * 60_000 })
    expect(d.verifyByUtc).toBe('2026-07-27T21:55:00.000Z')
    expect(d.verifyNow).toBe(false)
  })
  it('says verify-now when the lead point already passed', () => {
    const d = sessionVerifyDeadline('2026-07-27T22:00:00.000Z', '2026-07-27T21:58:00.000Z', { leadMs: 5 * 60_000 })
    expect(d.verifyNow).toBe(true)
  })
})

describe('QuotaTracker', () => {
  it('accumulates usage and records provider reset on rate-limit', () => {
    const t = new QuotaTracker('claude')
    t.ingest({ type: 'token_usage', usage })
    t.ingest({ type: 'turn_completed', usage })
    t.ingest({ type: 'rate_limited', resetAt: '2026-07-27T22:00:00Z', window: 'weekly' })
    const snap = t.snapshot()
    expect(snap.provider).toBe('claude')
    expect(snap.windows.find((w) => w.kind === '5h')?.used).toBe(280)
    expect(snap.windows.find((w) => w.kind === 'weekly')).toMatchObject({
      resetAtUtc: '2026-07-27T22:00:00Z',
      provenance: 'provider',
    })
  })
})

describe('integration: rate-limit → plan → enforced read-only resume', () => {
  it('detects the limit, plans the resume, and blocks a write on the first resumed turn', async () => {
    // A turn that hits the 5h limit mid-way.
    const limitedTurn: AgentEvent[] = [
      { type: 'turn_started' },
      { type: 'text_delta', text: 'editing…' },
      { type: 'rate_limited', resetAt: '2026-07-27T22:00:00.000Z', window: '5h' },
    ]
    const fake = new FakeAdapter({ turns: [limitedTurn] })
    const tracker = new QuotaTracker('fake')

    let hit: { resetAtUtc: string } | null = null
    for (const e of await collect(fake.runTurn(handle, { prompt: 'do work' }))) {
      const sig = tracker.ingest(e)
      if (sig?.kind === 'rate_limited') hit = { resetAtUtc: sig.resetAtUtc }
    }
    expect(hit).not.toBeNull()

    // Plan the resume from the provider's reset.
    const plan = planResumeAfterRateLimit(hit!.resetAtUtc, '2026-07-27T21:30:00.000Z')
    expect(plan.waitMs).toBeGreaterThan(0)

    // First resumed turn is read-only: a write is blocked in code, not by a prompt.
    expect(FIRST_TURN_READONLY.readOnly).toBe(true)
    const guards = firstTurnReadOnlyGuards()
    const decision = evaluateToolPre({ type: 'tool_pre', tool: 'Write', args: { file_path: 'src/a.ts' } }, guards, '/w')
    expect(decision.allow).toBe(false)
    // …but an inspection command is still allowed during the read-only turn.
    expect(evaluateToolPre({ type: 'tool_pre', tool: 'Bash', args: { command: 'git status' } }, guards, '/w').allow).toBe(true)
  })
})
