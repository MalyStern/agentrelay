// A deterministic, scripted ProviderAdapter. It burns no quota and reproduces the
// exact paths that are otherwise almost impossible to test — rate-limit mid-turn,
// session garbage-collected on resume, interrupt mid-stream (plan 02 §יג). Every
// adapter-agnostic mechanism (Quota Manager, resume flow, Process Manager, Timeline)
// can be exercised end-to-end against this.

import type {
  AgentEvent,
  Capabilities,
  Guard,
  ProviderAdapter,
  QuotaState,
  SessionHandle,
  SessionSpec,
  TurnInput,
  Usage,
} from '../../contracts/index.js'
import { AgentRelayError, ERROR_CODES } from '../../contracts/index.js'

export type ResumeBehavior = 'ok' | 'garbage_collected' | 'not_found'

export interface FakeOptions {
  id?: string
  /** One entry per turn: the events that turn yields, in order. */
  turns?: AgentEvent[][]
  capabilities?: Partial<Capabilities>
  quota?: QuotaState
  /** What resumeSession does — lets tests exercise the session-GC path. */
  resumeBehavior?: ResumeBehavior
}

const DEFAULT_CAPS: Capabilities = {
  resume: true,
  interrupt: true,
  steer: true,
  guards: true,
  forkSession: false,
  transformToolResult: true,
}

export class FakeAdapter implements ProviderAdapter {
  private readonly _id: string
  private readonly turns: AgentEvent[][]
  private readonly caps: Capabilities
  private readonly _quota: QuotaState
  private readonly resumeBehavior: ResumeBehavior

  private turnIndex = 0
  private interrupted = false
  private handleCounter = 0

  // Observable state for assertions.
  readonly installedGuards: Guard[] = []
  readonly steers: string[] = []
  readonly turnInputs: TurnInput[] = []
  interruptCount = 0

  constructor(opts: FakeOptions = {}) {
    this._id = opts.id ?? 'fake'
    this.turns = opts.turns ?? []
    this.caps = { ...DEFAULT_CAPS, ...opts.capabilities }
    this._quota = opts.quota ?? { provider: this._id, windows: [] }
    this.resumeBehavior = opts.resumeBehavior ?? 'ok'
  }

  id(): string {
    return this._id
  }

  capabilities(): Capabilities {
    return this.caps
  }

  async startSession(spec: SessionSpec): Promise<SessionHandle> {
    return {
      id: `${this._id}-sess-${++this.handleCounter}`,
      externalId: `${this._id}-ext-${this.handleCounter}`,
      provider: this._id,
      cwd: spec.cwd,
    }
  }

  async resumeSession(externalId: string, spec: SessionSpec): Promise<SessionHandle> {
    if (this.resumeBehavior === 'garbage_collected') {
      throw new AgentRelayError(ERROR_CODES.SESSION_GARBAGE_COLLECTED, {
        message: `session ${externalId} was garbage-collected`,
        context: { externalId },
        retriable: false,
      })
    }
    if (this.resumeBehavior === 'not_found') {
      throw new AgentRelayError(ERROR_CODES.SESSION_NOT_FOUND, { context: { externalId } })
    }
    return { id: `${this._id}-sess-${++this.handleCounter}`, externalId, provider: this._id, cwd: spec.cwd }
  }

  async *runTurn(_handle: SessionHandle, input: TurnInput): AsyncIterable<AgentEvent> {
    this.turnInputs.push(input)
    const turn = this.turns[this.turnIndex++] ?? [{ type: 'turn_completed', usage: zeroUsage() }]
    for (const e of turn) {
      if (this.interrupted) {
        this.interrupted = false
        return // stop mid-stream, as a real interrupt would
      }
      yield e
    }
  }

  async steer(_handle: SessionHandle, message: string): Promise<void> {
    this.steers.push(message)
  }

  async interrupt(_handle: SessionHandle): Promise<void> {
    this.interrupted = true
    this.interruptCount++
  }

  async quota(): Promise<QuotaState> {
    return this._quota
  }

  async installGuards(_handle: SessionHandle, guards: Guard[]): Promise<void> {
    this.installedGuards.push(...guards)
  }
}

const zeroUsage = (): Usage => ({ inputTokens: 0, outputTokens: 0, provenance: 'measured' })
