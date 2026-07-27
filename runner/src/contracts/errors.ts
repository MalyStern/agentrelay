// Structured error taxonomy. The daemon (and these runners) emit stable CODES, not
// translated prose — the UI maps a code to a localized string (plan 02 §י: logs are
// always en+structured; user-facing text is composed in the UI layer). Codes are
// dotted <domain>.<condition> so they group cleanly and never collide with UI copy.

export const ERROR_CODES = {
  // quota / rate limiting (reactive — Claude-headless has no live counter, 02 §ז.2)
  QUOTA_RATE_LIMITED: 'quota.rate_limited',
  QUOTA_USAGE_LIMIT_EXCEEDED: 'quota.usage_limit_exceeded',

  // session lifecycle (resume is the core feature; sessions can be GC'd, 02 §7)
  SESSION_RESUME_FAILED: 'session.resume_failed',
  SESSION_NOT_FOUND: 'session.not_found',
  SESSION_GARBAGE_COLLECTED: 'session.garbage_collected',

  // process manager — stop that really stops (02 §ז.3, #9)
  PROCESS_STOP_INCOMPLETE: 'process.stop_incomplete', // external resources still alive
  PROCESS_SPAWN_FAILED: 'process.spawn_failed',

  // guards / policy (Watchtower layer 1)
  GUARD_BLOCKED: 'guard.blocked',
  GUARD_READONLY_VIOLATION: 'guard.readonly_violation', // write during first-turn read-only

  // checkpoint / VCS
  CHECKPOINT_HEAD_FOREIGN: 'checkpoint.head_foreign', // an unsigned external commit moved HEAD (#6)
  CHECKPOINT_SAVE_FAILED: 'checkpoint.save_failed',

  // provider / adapter (an upstream API shape changed, 02 §8.7)
  PROVIDER_STREAM_ERROR: 'provider.stream_error',
  PROVIDER_PROTOCOL_MISMATCH: 'provider.protocol_mismatch',
  PROVIDER_UNAVAILABLE: 'provider.unavailable',

  // credential broker / isolation
  CREDENTIAL_UNAVAILABLE: 'credential.unavailable',
  ISOLATION_SETUP_FAILED: 'isolation.setup_failed',

  // generic
  INTERNAL: 'internal',
} as const

export type ErrorCode = (typeof ERROR_CODES)[keyof typeof ERROR_CODES]

/** The daemon/runner error type. Carries a stable code + structured context (never
 * a localized message). `retriable` tells the scheduler whether a resume/retry is
 * sensible; `resetAt` is populated for quota errors straight from the provider. */
export class AgentRelayError extends Error {
  readonly code: ErrorCode
  readonly context: Record<string, unknown>
  readonly retriable: boolean
  readonly resetAt: string | undefined

  constructor(
    code: ErrorCode,
    opts: { message?: string; context?: Record<string, unknown>; retriable?: boolean; resetAt?: string } = {},
  ) {
    super(opts.message ?? code)
    this.name = 'AgentRelayError'
    this.code = code
    this.context = opts.context ?? {}
    this.retriable = opts.retriable ?? false
    this.resetAt = opts.resetAt
  }

  toJSON(): { code: ErrorCode; message: string; context: Record<string, unknown>; retriable: boolean; resetAt?: string } {
    return {
      code: this.code,
      message: this.message,
      context: this.context,
      retriable: this.retriable,
      ...(this.resetAt ? { resetAt: this.resetAt } : {}),
    }
  }
}

export const isAgentRelayError = (e: unknown): e is AgentRelayError => e instanceof AgentRelayError
