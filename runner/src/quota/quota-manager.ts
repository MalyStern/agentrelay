// Quota Manager — the central feature (plan 00 §9). Detection is REACTIVE
// (Claude-headless has no live counter, fix #3): we learn we're limited from the
// `rate_limited` event / quota error, using the provider's own `resetAt` (never a
// guess). This module is the pure decision core the daemon mirrors:
//   - plan the resume time from the provider's reset,
//   - decide WHEN to verify the session still exists (before the window, fix #7),
//   - produce the enforced first-turn read-only guard set (fix #5).

import type { AgentEvent, Guard, Provenance, QuotaWindow } from '../contracts/index.js'

export interface QuotaWindowSnapshot {
  kind: QuotaWindow
  used?: number
  limit?: number
  resetAtUtc?: string
  provenance: Provenance
}

/** A quota-relevant fact extracted from a normalized event, or null. */
export type QuotaSignal =
  | { kind: 'usage'; used: number }
  | { kind: 'rate_limited'; resetAtUtc: string; window: QuotaWindow }
  | null

export function interpretEvent(event: AgentEvent): QuotaSignal {
  if (event.type === 'token_usage' || event.type === 'turn_completed') {
    const u = event.usage
    return { kind: 'usage', used: u.totalTokens ?? u.inputTokens + u.outputTokens }
  }
  if (event.type === 'rate_limited') {
    return { kind: 'rate_limited', resetAtUtc: event.resetAt, window: event.window ?? '5h' }
  }
  return null
}

const ms = (iso: string): number => new Date(iso).getTime()
const iso = (t: number): string => new Date(t).toISOString()

export interface ResumePlan {
  /** When to resume — the provider's reset plus a safety buffer. */
  resumeAtUtc: string
  /** Milliseconds to wait from `nowUtc` (never negative). */
  waitMs: number
  bufferMs: number
}

/**
 * Plan the resume after a rate-limit. We wait until the provider's reset time plus a
 * buffer (clocks skew; resets aren't instant). If the reset is somehow already past,
 * resume ~immediately (buffer only).
 */
export function planResumeAfterRateLimit(
  resetAtUtc: string,
  nowUtc: string,
  opts: { bufferMs?: number } = {},
): ResumePlan {
  const bufferMs = opts.bufferMs ?? 60_000
  const resumeAt = ms(resetAtUtc) + bufferMs
  const waitMs = Math.max(0, resumeAt - ms(nowUtc))
  return { resumeAtUtc: iso(resumeAt), waitMs, bufferMs }
}

/**
 * When to verify the provider session still exists. Claude session files can be
 * garbage-collected, and `--resume` fails hours later — exactly in the reset window
 * (fix #7). So verify BEFORE the window, with lead time; if the lead point is already
 * past, verify now.
 */
export function sessionVerifyDeadline(
  resetAtUtc: string,
  nowUtc: string,
  opts: { leadMs?: number } = {},
): { verifyByUtc: string; verifyNow: boolean } {
  const leadMs = opts.leadMs ?? 5 * 60_000
  const verifyBy = ms(resetAtUtc) - leadMs
  return { verifyByUtc: iso(verifyBy), verifyNow: verifyBy <= ms(nowUtc) }
}

/**
 * The first turn after a resume must be read-only until a checkpoint is approved
 * (fix #5) — enforced in CODE, not by a prompt. We add a guard that blocks every
 * write while still allowing inspection commands (git status, cat, tests).
 */
export function firstTurnReadOnlyGuards(): Guard[] {
  return [{ kind: 'forbid_path', spec: '**' }]
}

/** The TurnInput flag the adapter/guard layer keys the read-only turn off. */
export const FIRST_TURN_READONLY = { readOnly: true } as const

/** A tiny stateful tracker: fold events into per-window quota snapshots. Estimated
 * used-counts are marked as such (they never drive a hard decision alone, fix #3). */
export class QuotaTracker {
  private readonly windows = new Map<QuotaWindow, QuotaWindowSnapshot>()

  constructor(private readonly provider: string) {}

  ingest(event: AgentEvent): QuotaSignal {
    const sig = interpretEvent(event)
    if (!sig) return null
    if (sig.kind === 'usage') {
      const w = this.windows.get('5h') ?? { kind: '5h' as const, provenance: 'measured' as Provenance }
      w.used = (w.used ?? 0) + sig.used
      this.windows.set('5h', w)
    } else {
      this.windows.set(sig.window, {
        kind: sig.window,
        resetAtUtc: sig.resetAtUtc,
        provenance: 'provider',
      })
    }
    return sig
  }

  snapshot(): { provider: string; windows: QuotaWindowSnapshot[] } {
    return { provider: this.provider, windows: [...this.windows.values()] }
  }
}
