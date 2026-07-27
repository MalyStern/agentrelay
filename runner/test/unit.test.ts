import { describe, expect, it } from 'vitest'
import {
  AgentRelayError,
  ERROR_CODES,
  eventKindOf,
  FakeAdapter,
  collect,
  groupIntoTurns,
  makeFixture,
  parseFixture,
  recordStream,
  replayAdapterFromFixture,
  serializeFixture,
  type AgentEvent,
  type SessionHandle,
} from '../src/index.js'

const usage = { inputTokens: 10, outputTokens: 5, provenance: 'measured' as const }
const handle: SessionHandle = { id: 's1', provider: 'fake', cwd: '/w' }

describe('event taxonomy', () => {
  it('maps normalized events to log kinds (and null for non-logged)', () => {
    expect(eventKindOf({ type: 'turn_started' })).toBe('turn.started')
    expect(eventKindOf({ type: 'tool_pre', tool: 'Read', args: {} })).toBe('tool.pre')
    expect(eventKindOf({ type: 'rate_limited', resetAt: 'x' })).toBe('quota.exhausted')
    expect(eventKindOf({ type: 'text_delta', text: 'hi' })).toBeNull()
    expect(eventKindOf({ type: 'token_usage', usage })).toBeNull()
  })
})

describe('FakeAdapter', () => {
  it('yields scripted turns in order and advances the turn index', async () => {
    const t1: AgentEvent[] = [{ type: 'turn_started' }, { type: 'text_delta', text: 'a' }, { type: 'turn_completed', usage }]
    const t2: AgentEvent[] = [{ type: 'turn_started' }, { type: 'file_edited', path: 'x.ts' }, { type: 'turn_completed', usage }]
    const fake = new FakeAdapter({ turns: [t1, t2] })

    expect(await collect(fake.runTurn(handle, { prompt: 'go' }))).toEqual(t1)
    expect(await collect(fake.runTurn(handle, { prompt: 'again' }))).toEqual(t2)
    // exhausted -> default completed turn
    const t3 = await collect(fake.runTurn(handle, { prompt: 'more' }))
    expect(t3.at(-1)?.type).toBe('turn_completed')
    expect(fake.turnInputs).toHaveLength(3)
  })

  it('reproduces a rate-limit mid-turn', async () => {
    const turn: AgentEvent[] = [
      { type: 'turn_started' },
      { type: 'text_delta', text: 'working' },
      { type: 'rate_limited', resetAt: '2026-07-27T22:00:00Z', window: '5h' },
    ]
    const fake = new FakeAdapter({ turns: [turn] })
    const got = await collect(fake.runTurn(handle, { prompt: 'go' }))
    expect(got.at(-1)).toEqual({ type: 'rate_limited', resetAt: '2026-07-27T22:00:00Z', window: '5h' })
  })

  it('interrupt stops the stream mid-turn', async () => {
    const turn: AgentEvent[] = [
      { type: 'turn_started' },
      { type: 'text_delta', text: 'one' },
      { type: 'text_delta', text: 'two' },
      { type: 'turn_completed', usage },
    ]
    const fake = new FakeAdapter({ turns: [turn] })
    const seen: AgentEvent[] = []
    for await (const e of fake.runTurn(handle, { prompt: 'go' })) {
      seen.push(e)
      if (e.type === 'text_delta' && e.text === 'one') await fake.interrupt(handle)
    }
    expect(seen.map((e) => e.type)).toEqual(['turn_started', 'text_delta'])
    expect(fake.interruptCount).toBe(1)
  })

  it('resume behaviors surface the right structured errors', async () => {
    await expect(new FakeAdapter({ resumeBehavior: 'ok' }).resumeSession('ext', { cwd: '/w' })).resolves.toMatchObject({
      externalId: 'ext',
    })
    await expect(new FakeAdapter({ resumeBehavior: 'garbage_collected' }).resumeSession('ext', { cwd: '/w' }))
      .rejects.toMatchObject({ code: ERROR_CODES.SESSION_GARBAGE_COLLECTED, retriable: false })
    await expect(new FakeAdapter({ resumeBehavior: 'not_found' }).resumeSession('ext', { cwd: '/w' }))
      .rejects.toMatchObject({ code: ERROR_CODES.SESSION_NOT_FOUND })
  })

  it('records installed guards and steers for assertions', async () => {
    const fake = new FakeAdapter()
    await fake.installGuards(handle, [{ kind: 'no_push' }, { kind: 'allowed_paths', spec: '**/*.css' }])
    await fake.steer(handle, 'focus on tests')
    expect(fake.installedGuards.map((g) => g.kind)).toEqual(['no_push', 'allowed_paths'])
    expect(fake.steers).toEqual(['focus on tests'])
  })
})

describe('record / replay', () => {
  const stream = async function* (): AsyncIterable<AgentEvent> {
    yield { type: 'turn_started' }
    yield { type: 'tool_pre', tool: 'Read', args: { path: 'a.ts' } }
    yield { type: 'tool_post', tool: 'Read', result: 'ok' }
    yield { type: 'turn_completed', usage }
  }

  it('records with monotonic seq and an injectable clock', async () => {
    let n = 0
    const rec = await recordStream(stream(), () => `t${n++}`)
    expect(rec.map((r) => r.seq)).toEqual([0, 1, 2, 3])
    expect(rec.map((r) => r.ts)).toEqual(['t0', 't1', 't2', 't3'])
    expect(rec[0]!.event).toEqual({ type: 'turn_started' })
  })

  it('groups a flat stream back into turns at terminal events', () => {
    const flat: AgentEvent[] = [
      { type: 'turn_started' }, { type: 'turn_completed', usage },
      { type: 'turn_started' }, { type: 'rate_limited', resetAt: 'x' },
    ]
    const turns = groupIntoTurns(flat)
    expect(turns).toHaveLength(2)
    expect(turns[0]!.at(-1)?.type).toBe('turn_completed')
    expect(turns[1]!.at(-1)?.type).toBe('rate_limited')
  })

  it('replays a fixture identically and survives a serialize round-trip', async () => {
    const rec = await recordStream(stream(), () => '2026-01-01T00:00:00Z')
    const fixture = makeFixture('claude-code', rec, '2026-01-01T00:00:00Z')
    const round = parseFixture(serializeFixture(fixture))
    const adapter = replayAdapterFromFixture(round)
    const replayed = await collect(adapter.runTurn(handle, { prompt: 'x' }))
    expect(replayed).toEqual(rec.map((r) => r.event))
    expect(adapter.id()).toBe('replay:claude-code')
  })

  it('rejects a malformed fixture', () => {
    expect(() => parseFixture('{"schema":2}')).toThrow()
  })
})

describe('AgentRelayError', () => {
  it('carries a stable code + structured context and serializes', () => {
    const e = new AgentRelayError(ERROR_CODES.QUOTA_RATE_LIMITED, {
      message: 'hit 5h limit',
      retriable: true,
      resetAt: '2026-07-27T22:00:00Z',
      context: { window: '5h' },
    })
    expect(e.code).toBe('quota.rate_limited')
    expect(e.toJSON()).toEqual({
      code: 'quota.rate_limited',
      message: 'hit 5h limit',
      context: { window: '5h' },
      retriable: true,
      resetAt: '2026-07-27T22:00:00Z',
    })
  })
})
