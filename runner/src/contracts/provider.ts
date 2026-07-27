// The provider contract. Every engine (Claude Agent SDK, Codex App-Server, ACP,
// Cloud-delegated) implements this, so "Claude commands Codex" and future providers
// are one adapter each (plan 00 §5, 02 §ו). The Rust `trait ProviderAdapter` mirrors
// this; here the event stream is a plain async iterable.

import type { AgentEvent, Provenance, QuotaWindow } from './events.js'

export interface Capabilities {
  /** Can a prior session be resumed by external id? */
  resume: boolean
  /** Can an in-flight turn be interrupted? */
  interrupt: boolean
  /** Can a message be injected between turns (not mid-turn)? */
  steer: boolean
  /** Does it support enforceable guards (Claude hooks / Codex sandbox+approval)? */
  guards: boolean
  /** Can a session be forked before a risky step (Codex thread/fork)? */
  forkSession: boolean
  /** Can a tool RESULT be replaced before the model sees it? (SDK transform /
   * PostToolUse.updatedToolOutput) — the Context-Optimizer depends on this
   * (plan 02 §ז.1, kill-test A). CLI-headless alone = false. */
  transformToolResult: boolean
}

export interface SessionSpec {
  /** Working directory (the worktree). */
  cwd: string
  /** External session/thread id to resume, when resuming. */
  resumeExternalId?: string
  /** Extra environment for the spawned engine (secrets injected by the broker). */
  env?: Record<string, string>
  /** Guards to install before the first turn. */
  guards?: Guard[]
}

export interface SessionHandle {
  /** AgentRelay-internal id. */
  id: string
  /** Provider's own id (claude session_id / codex thread_id), once known. */
  externalId?: string
  provider: string
  cwd: string
}

export interface TurnInput {
  prompt: string
  /**
   * First turn after a resume MUST be read-only (plan 02 §ז.2 / #5): the daemon
   * enforces this with a PreToolUse guard that blocks every Write/Edit until a
   * checkpoint is approved. Adapters surface it so the guard layer can assert it.
   */
  readOnly?: boolean
  /** Opaque attachments (files/images) passed through to the engine. */
  attachments?: unknown[]
}

export interface QuotaWindowState {
  kind: QuotaWindow
  used?: number
  limit?: number
  /** From the provider — never a guess (plan 00 §4 quota_snapshots.reset_at_utc). */
  resetAt?: string
  provenance: Provenance
}

export interface QuotaState {
  provider: string
  windows: QuotaWindowState[]
}

/** Guard kinds mirror policies.rule_kind (00 §4) + the enforcement table (02 §ז.4). */
export type GuardKind =
  | 'forbid_path'
  | 'allowed_paths'
  | 'forbid_cmd'
  | 'no_push'
  | 'no_deploy'
  | 'no_net'
  | 'no_dependency'
  | 'workspace_boundary'
  | 'allow_only_branch'
  | 'max_files'
  | 'max_cost'

export interface Guard {
  kind: GuardKind
  /** kind-specific spec (a glob, a branch name, a numeric cap, …). */
  spec?: unknown
}

export interface ProviderAdapter {
  id(): string
  capabilities(): Capabilities

  startSession(spec: SessionSpec): Promise<SessionHandle>
  resumeSession(externalId: string, spec: SessionSpec): Promise<SessionHandle>

  /** Run one turn; yields normalized events until the turn completes or errors. */
  runTurn(handle: SessionHandle, input: TurnInput): AsyncIterable<AgentEvent>

  /** Inject a message between turns (optional; no-op if unsupported). */
  steer(handle: SessionHandle, message: string): Promise<void>
  /** Stop the current turn AND its process tree (interrupt alone doesn't stop bg). */
  interrupt(handle: SessionHandle): Promise<void>

  quota(): Promise<QuotaState>

  /** Install provider-level enforcement (hooks / sandbox) before a turn. */
  installGuards(handle: SessionHandle, guards: Guard[]): Promise<void>
}
