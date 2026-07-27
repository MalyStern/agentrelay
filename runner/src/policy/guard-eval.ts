// Watchtower layer-1: deterministic guard evaluation (plan 00 §7, 02 §ז.4). Given a
// proposed action (derived from a normalized `tool_pre` event) and the mission's
// guards, decide block/allow — the brain behind Claude's PreToolUse `exit 2` and the
// Codex approval-intercept. Pure and total: no I/O, no guessing. It returns a stable
// reason CODE (the UI localizes it), never prose.

import type { AgentEvent, Guard, GuardKind } from '../contracts/index.js'

/** A provider-neutral view of what the agent is about to do. */
export type ProposedAction =
  | { kind: 'write'; path: string }
  | { kind: 'command'; command: string }
  | { kind: 'network'; target?: string }

export interface GuardViolation {
  guard: GuardKind
  /** Stable code for the UI to localize, e.g. "guard.no_push". */
  reasonCode: string
  action: ProposedAction
  detail?: string
}

export interface GuardDecision {
  allow: boolean
  violations: GuardViolation[]
}

// ── tiny glob (** , * , ?) → RegExp against POSIX paths ─────────────────────────
export function globToRegExp(glob: string): RegExp {
  let re = ''
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]!
    if (c === '*') {
      if (glob[i + 1] === '*') {
        if (glob[i + 2] === '/') {
          re += '(?:.*/)?'
          i += 2
        } else {
          re += '.*'
          i += 1
        }
      } else re += '[^/]*'
    } else if (c === '?') re += '[^/]'
    else if ('.+^${}()|[]\\'.includes(c)) re += '\\' + c
    else re += c
  }
  return new RegExp(`^${re}$`)
}

const toPosix = (p: string): string => p.replace(/\\/g, '/')

// ── command classifiers (deterministic, conservative) ──────────────────────────
const RE = {
  push: /\bgit\s+push\b/i,
  deploy: /\b(?:deploy|vercel|netlify|flyctl|fly\s+deploy|serverless\s+deploy|kubectl\s+apply|terraform\s+apply|gcloud\s+.*deploy|eb\s+deploy|heroku\s+.*(?:push|deploy))\b/i,
  network: /(?:^|\s|;|&&|\|)(?:curl|wget|nc|ncat|telnet|scp|rsync|ssh)\b/i,
  dependency:
    /\b(?:npm|pnpm|yarn|bun)\s+(?:install|add|i)\b|\bpip3?\s+install\b|\bcargo\s+add\b|\bgo\s+get\b|\bgem\s+install\b|\bpoetry\s+add\b|\buv\s+add\b/i,
}

/**
 * Derive provider-neutral actions from a normalized `tool_pre` event. Recognizes the
 * common Claude/Codex tool shapes; unknown tools yield nothing (fail-open at layer-1,
 * because layer-2 validators + the OS sandbox are the backstops — plan 02 §ז.4).
 */
export function deriveActions(event: Extract<AgentEvent, { type: 'tool_pre' }>): ProposedAction[] {
  const tool = event.tool
  const args = (event.args ?? {}) as Record<string, unknown>
  const str = (k: string): string | undefined => (typeof args[k] === 'string' ? (args[k] as string) : undefined)

  // File-writing tools.
  if (/^(Write|Edit|MultiEdit|NotebookEdit|str_replace_editor|create_file|apply_patch)$/i.test(tool)) {
    const p = str('file_path') ?? str('path') ?? str('filePath') ?? str('notebook_path')
    if (p) return [{ kind: 'write', path: p }]
  }
  // Shell tools.
  if (/^(Bash|Shell|run_terminal_cmd|exec|terminal)$/i.test(tool)) {
    const cmd = str('command') ?? str('cmd') ?? str('script')
    if (cmd) return [{ kind: 'command', command: cmd }]
  }
  // Network-ish tools.
  if (/^(WebFetch|WebSearch|fetch|http_request)$/i.test(tool)) {
    return [{ kind: 'network', ...(str('url') ? { target: str('url')! } : {}) }]
  }
  return []
}

/** Is a write path inside the workspace root? */
function insideWorkspace(path: string, cwd: string): boolean {
  const p = toPosix(path)
  const root = toPosix(cwd).replace(/\/+$/, '')
  if (p.startsWith('/') || /^[a-zA-Z]:\//.test(p)) {
    // absolute — must be under root
    return p === root || p.startsWith(root + '/')
  }
  // relative — reject parent-escapes
  return !p.split('/').includes('..')
}

function checkAction(action: ProposedAction, guard: Guard, cwd: string): GuardViolation | null {
  const v = (detail?: string): GuardViolation => ({
    guard: guard.kind,
    reasonCode: `guard.${guard.kind}`,
    action,
    ...(detail ? { detail } : {}),
  })

  switch (guard.kind) {
    case 'no_push':
      return action.kind === 'command' && RE.push.test(action.command) ? v() : null
    case 'no_deploy':
      return action.kind === 'command' && RE.deploy.test(action.command) ? v() : null
    case 'no_net':
      if (action.kind === 'network') return v()
      return action.kind === 'command' && RE.network.test(action.command) ? v() : null
    case 'no_dependency':
      return action.kind === 'command' && RE.dependency.test(action.command) ? v() : null
    case 'forbid_cmd': {
      const pat = typeof guard.spec === 'string' ? guard.spec : undefined
      if (action.kind === 'command' && pat && new RegExp(pat, 'i').test(action.command)) return v(pat)
      return null
    }
    case 'workspace_boundary':
      return action.kind === 'write' && !insideWorkspace(action.path, cwd) ? v(action.path) : null
    case 'forbid_path': {
      const globs = asGlobs(guard.spec)
      return action.kind === 'write' && globs.some((g) => globToRegExp(g).test(toPosix(action.path))) ? v(action.path) : null
    }
    case 'allowed_paths': {
      const globs = asGlobs(guard.spec)
      if (action.kind !== 'write' || globs.length === 0) return null
      const allowed = globs.some((g) => globToRegExp(g).test(toPosix(action.path)))
      return allowed ? null : v(action.path)
    }
    // Stateful / branch guards are enforced by the daemon with counters/VCS, not per-action here.
    case 'allow_only_branch':
    case 'max_files':
    case 'max_cost':
      return null
  }
}

const asGlobs = (spec: unknown): string[] =>
  typeof spec === 'string' ? [spec] : Array.isArray(spec) ? spec.filter((s): s is string => typeof s === 'string') : []

/** Evaluate all actions against all guards. Blocks if any guard is violated. */
export function evaluateGuards(actions: ProposedAction[], guards: Guard[], cwd: string): GuardDecision {
  const violations: GuardViolation[] = []
  for (const action of actions) {
    for (const guard of guards) {
      const violation = checkAction(action, guard, cwd)
      if (violation) violations.push(violation)
    }
  }
  return { allow: violations.length === 0, violations }
}

/** Convenience: evaluate a `tool_pre` event directly. */
export function evaluateToolPre(
  event: Extract<AgentEvent, { type: 'tool_pre' }>,
  guards: Guard[],
  cwd: string,
): GuardDecision {
  return evaluateGuards(deriveActions(event), guards, cwd)
}
