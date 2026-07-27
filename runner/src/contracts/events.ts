// The normalized event stream every provider adapter emits into. This is the
// language-neutral contract: the Rust daemon's `enum AgentEvent` (plan 00 §5)
// mirrors these exact shapes, and Mission Engine / Watchtower / Timeline / Quota
// speak ONLY in these — so adding a provider is one adapter and nothing else moves.
//
// Modeled as a discriminated union on `type`. Bare payloads match the Rust enum;
// stream metadata (seq, ts) is attached by the envelope below, not baked into each
// variant, so the union stays 1:1 with the daemon.

/** Provenance tag — the plan's über-principle (02 §ב): every surfaced value says
 * where it came from. Measured = we counted it; Estimated = a model/heuristic
 * produced it (shown as ≈/range, never as fact); Provider = straight from the
 * provider's API. */
export type Provenance = 'measured' | 'estimated' | 'provider'

export interface Usage {
  inputTokens: number
  outputTokens: number
  cacheReadTokens?: number
  cacheCreationTokens?: number
  /** Provider-reported total when given; otherwise derived. */
  totalTokens?: number
  provenance: Provenance
}

/** Which quota window a rate-limit belongs to. */
export type QuotaWindow = '5h' | 'weekly' | 'context'

export type AgentEvent =
  | { type: 'turn_started' }
  | { type: 'text_delta'; text: string }
  | { type: 'tool_pre'; tool: string; args: unknown }
  | { type: 'tool_post'; tool: string; result: unknown }
  | { type: 'file_edited'; path: string }
  | { type: 'cmd_run'; cmd: string }
  | { type: 'permission_requested'; action: string }
  | { type: 'token_usage'; usage: Usage }
  | { type: 'turn_completed'; usage: Usage }
  | { type: 'error'; message: string; code?: string }
  | { type: 'rate_limited'; resetAt: string; window?: QuotaWindow; provider?: string }

export type AgentEventType = AgentEvent['type']

/** A stream item as persisted/replayed: the event plus ordering metadata. The
 * daemon assigns `seq`; adapters need not. */
export interface EnvelopedEvent {
  seq: number
  /** ISO-8601 UTC. */
  ts: string
  event: AgentEvent
}

// ── events.kind taxonomy (plan 00 §4) — the vocabulary the whole system logs in.
// Kept as a const so the daemon, the TS runner and tests agree on the strings.
export const EVENT_KINDS = [
  'mission.created', 'mission.started', 'mission.plan_ready', 'mission.completed', 'mission.failed', 'mission.stopped',
  'unit.assigned', 'unit.started', 'unit.result', 'unit.reviewed',
  'turn.started', 'turn.completed', 'turn.interrupted',
  'tool.pre', 'tool.post', 'tool.blocked',
  'file.edited', 'cmd.run',
  'permission.requested', 'permission.granted', 'permission.denied',
  'checkpoint.saved',
  'quota.low', 'quota.exhausted', 'quota.reset', 'quota.resumed',
  'validation.ran', 'drift.detected', 'notify.sent',
  'stop.requested', 'stop.completed',
] as const

export type EventKind = (typeof EVENT_KINDS)[number]

/** Map a normalized AgentEvent to the coarse taxonomy kind used in the event log.
 * Not every event has a 1:1 kind (deltas are not logged individually). */
export function eventKindOf(e: AgentEvent): EventKind | null {
  switch (e.type) {
    case 'turn_started': return 'turn.started'
    case 'turn_completed': return 'turn.completed'
    case 'tool_pre': return 'tool.pre'
    case 'tool_post': return 'tool.post'
    case 'file_edited': return 'file.edited'
    case 'cmd_run': return 'cmd.run'
    case 'permission_requested': return 'permission.requested'
    case 'rate_limited': return 'quota.exhausted'
    case 'text_delta':
    case 'token_usage':
    case 'error':
      return null
  }
}
