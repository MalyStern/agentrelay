// Record a provider's normalized event stream to a fixture, and replay it later as
// a ProviderAdapter — so the hard paths (rate-limit mid-turn, session GC, HEAD
// churn) are captured once from a real run and then re-run deterministically in CI,
// forever, without touching a provider or burning quota (plan 02 §יג).

import type { AgentEvent, EnvelopedEvent } from '../contracts/events.js'
import type { ProviderAdapter } from '../contracts/provider.js'
import { FakeAdapter } from '../providers/fake/fake-adapter.js'

/** Drain an async event stream into an array. */
export async function collect(stream: AsyncIterable<AgentEvent>): Promise<AgentEvent[]> {
  const out: AgentEvent[] = []
  for await (const e of stream) out.push(e)
  return out
}

/** Drain a stream, stamping monotonic seq + timestamps. `clock` is injectable so
 * recorded fixtures can be byte-stable in tests. */
export async function recordStream(
  stream: AsyncIterable<AgentEvent>,
  clock: () => string = () => new Date().toISOString(),
): Promise<EnvelopedEvent[]> {
  const out: EnvelopedEvent[] = []
  let seq = 0
  for await (const event of stream) out.push({ seq: seq++, ts: clock(), event })
  return out
}

export interface Fixture {
  schema: 1
  provider: string
  recordedAt: string
  events: EnvelopedEvent[]
}

export function makeFixture(provider: string, events: EnvelopedEvent[], recordedAt: string): Fixture {
  return { schema: 1, provider, recordedAt, events }
}

export function serializeFixture(fixture: Fixture): string {
  return JSON.stringify(fixture, null, 2)
}

export function parseFixture(json: string): Fixture {
  const f = JSON.parse(json) as Fixture
  if (f.schema !== 1 || !Array.isArray(f.events)) throw new Error('not a valid AgentRelay fixture (schema 1)')
  return f
}

/** A turn ends at the first terminal event. Everything up to and including it is one
 * turn; the next events start the following turn. */
const TERMINAL = new Set<AgentEvent['type']>(['turn_completed', 'rate_limited', 'error'])

export function groupIntoTurns(events: AgentEvent[]): AgentEvent[][] {
  const turns: AgentEvent[][] = []
  let current: AgentEvent[] = []
  for (const e of events) {
    current.push(e)
    if (TERMINAL.has(e.type)) {
      turns.push(current)
      current = []
    }
  }
  if (current.length) turns.push(current)
  return turns
}

/** Build a replay ProviderAdapter from a recorded fixture. Under the hood it's a
 * FakeAdapter fed the recorded events, grouped back into turns. */
export function replayAdapterFromFixture(fixture: Fixture): ProviderAdapter {
  const bare = fixture.events.map((e) => e.event)
  return new FakeAdapter({ id: `replay:${fixture.provider}`, turns: groupIntoTurns(bare) })
}
