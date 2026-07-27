// Scheduler send-gate (plan 00 §10): the check every queued prompt passes through
// *right before* it would be sent — this is what stops a stale prompt from firing
// after the mission moved on. Pure: given a prompt + the current context, it returns
// one verdict. Nothing here does I/O or guesses.

export type Trigger =
  | { type: 'at'; at: string } // ISO time
  | { type: 'after'; sinceUtc: string; afterMs: number }
  | { type: 'on_event'; event: string }
  | { type: 'cron'; expr: string }

export type PromptStatus = 'queued' | 'sent' | 'expired' | 'cancelled' | 'superseded'

export interface ScheduledPrompt {
  id: string
  target: string // claude|codex|all|commander
  trigger: Trigger
  body: string
  /** Optional guard checked right before send (its result is passed in ctx). */
  condition?: string
  /** Expiry — never send after this (ISO). */
  validUntil?: string
  priority: number
  /** The mission contract version this prompt was queued against. */
  missionVer: number
  status: PromptStatus
}

export interface SendContext {
  nowUtc: string
  /** Current mission contract version — a queued prompt from an older version is stale. */
  currentMissionVer: number
  /** Result of evaluating `condition`, when the prompt has one. */
  conditionResult?: boolean
  /** For on_event triggers: the event that just fired. */
  firedEvent?: string
  /** For cron triggers: whether the cron is due now (parsed elsewhere). */
  cronDue?: boolean
}

export type SendVerdict = 'send' | 'not_due' | 'expired' | 'superseded' | 'condition_unmet' | 'cancelled' | 'already_sent'

const ms = (iso: string): number => new Date(iso).getTime()

export function isTriggerDue(trigger: Trigger, ctx: SendContext): boolean {
  switch (trigger.type) {
    case 'at':
      return ms(ctx.nowUtc) >= ms(trigger.at)
    case 'after':
      return ms(ctx.nowUtc) >= ms(trigger.sinceUtc) + trigger.afterMs
    case 'on_event':
      return ctx.firedEvent === trigger.event
    case 'cron':
      return ctx.cronDue === true
  }
}

/**
 * The gate. Order matters: terminal statuses first, then expiry, then staleness
 * (mission changed), then the trigger, then the condition. Only a fully-clear prompt
 * returns 'send'.
 */
export function evaluateSendGate(prompt: ScheduledPrompt, ctx: SendContext): SendVerdict {
  if (prompt.status === 'cancelled') return 'cancelled'
  if (prompt.status === 'superseded') return 'superseded'
  if (prompt.status === 'expired') return 'expired'
  if (prompt.status === 'sent') return 'already_sent'

  if (prompt.validUntil && ms(ctx.nowUtc) > ms(prompt.validUntil)) return 'expired'
  // The mission's contract advanced since this prompt was queued → it's stale.
  if (prompt.missionVer < ctx.currentMissionVer) return 'superseded'
  if (!isTriggerDue(prompt.trigger, ctx)) return 'not_due'
  if (prompt.condition !== undefined && ctx.conditionResult === false) return 'condition_unmet'
  return 'send'
}

/**
 * Pick the next prompt to actually send: among those whose gate says 'send', the
 * highest priority wins; ties break by insertion order (stable). Returns null if
 * nothing is sendable now.
 */
export function selectNextToSend(prompts: ScheduledPrompt[], ctx: SendContext): ScheduledPrompt | null {
  const sendable = prompts.filter((p) => evaluateSendGate(p, ctx) === 'send')
  if (sendable.length === 0) return null
  return sendable.reduce((best, p) => (p.priority > best.priority ? p : best))
}
