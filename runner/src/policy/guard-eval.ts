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

/** Resolve `.`/`..` and collapse slashes WITHOUT touching the filesystem, so `..`
 * traversal can't defeat the containment guards. Returns whether it's absolute and
 * whether a relative path escaped above its base (`up > 0`). */
export function normPath(input: string): { absolute: boolean; escapes: boolean; path: string } {
  let p = toPosix(input)
  const driveMatch = /^([a-zA-Z]):\//.exec(p)
  const drive = driveMatch ? driveMatch[1]!.toLowerCase() + ':' : ''
  if (drive) p = p.slice(drive.length)
  const absolute = drive !== '' || p.startsWith('/')
  const segs = p.split('/').filter((s) => s !== '' && s !== '.')
  const out: string[] = []
  let up = 0
  for (const s of segs) {
    if (s === '..') {
      if (out.length) out.pop()
      else if (!absolute) up++ // a relative path climbing above its base
      // absolute: `..` at root is a no-op (can't escape root)
    } else out.push(s)
  }
  const prefix = drive ? drive + '/' : absolute ? '/' : ''
  return { absolute, escapes: up > 0, path: prefix + '../'.repeat(up) + out.join('/') }
}

// ── command classifiers (deterministic, conservative; match the invoked binary
//    anywhere on the line so `git -C x push` / `kubectl --context p apply` are caught)
const RE = {
  push: /\bgit\b(?:\s+\S+)*\s+push\b/i,
  deploy: /\b(?:vercel|netlify|flyctl|serverless)\b|\bfly\s+deploy\b|\bkubectl\b(?:\s+\S+)*\s+apply\b|\bterraform\b(?:\s+\S+)*\s+apply\b|\bgcloud\b[^\n]*\bdeploy\b|\beb\s+deploy\b|\bheroku\b[^\n]*(?:push|deploy)\b|\bansible-playbook\b/i,
  // separators now include ( , backtick and $ so $(curl …) / `curl …` are caught
  network: /(?:^|[\s;&|(`$])(?:curl|wget|nc|ncat|telnet|scp|rsync|ssh)\b/i,
  dependency:
    /\b(?:npm|pnpm|yarn|bun)\s+(?:install|add|i)\b|\bpip3?\s+install\b|\bcargo\s+add\b|\bgo\s+get\b|\bgem\s+install\b|\bpoetry\s+add\b|\buv\s+add\b/i,
}

// ── read-only command allowlist (for the first-turn read-only guard) ────────────
const READONLY_BINS = new Set([
  'ls', 'cat', 'head', 'tail', 'pwd', 'echo', 'printf', 'grep', 'egrep', 'rg', 'wc',
  'stat', 'file', 'which', 'type', 'env', 'printenv', 'date', 'whoami', 'uname', 'true',
  'basename', 'dirname', 'realpath', 'sort', 'uniq', 'cut', 'diff', 'tree', 'du', 'df',
])
const GIT_READONLY = new Set(['status', 'log', 'diff', 'show', 'branch', 'rev-parse', 'ls-files', 'remote', 'describe', 'blame', 'cat-file', 'config', 'tag', 'stash'])

/** True only if EVERY sub-command is a pure inspection with no output/side effect. */
export function isReadOnlyCommand(command: string): boolean {
  if (/[>]/.test(command)) return false // any redirection writes
  const parts = command.split(/&&|\|\||;|\|/).map((s) => s.trim()).filter(Boolean)
  if (parts.length === 0) return false
  return parts.every((part) => {
    const toks = part.split(/\s+/)
    const bin = (toks[0] ?? '').replace(/^.*\//, '') // strip any path prefix
    if (bin === 'git') return toks[1] !== undefined && GIT_READONLY.has(toks[1]!) && !/\b(?:-i|--interactive)\b/.test(part)
    if (bin === 'find') return !/-(?:delete|exec|execdir|fprint|fprintf)\b/.test(part)
    if (bin === 'sed') return !/\s-i\b/.test(part) // sed -i mutates
    return READONLY_BINS.has(bin)
  })
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

/** Is a write path inside the workspace root — AFTER resolving `.`/`..`? */
function insideWorkspace(path: string, cwd: string): boolean {
  const t = normPath(path)
  if (t.escapes) return false // relative path climbed above the workspace
  if (!t.absolute) return true // relative, no escape → inside
  const root = normPath(cwd).path.replace(/\/+$/, '')
  return t.path === root || t.path.startsWith(root + '/')
}

/** The path a relative/absolute write resolves to, expressed for glob matching:
 * workspace-relative when inside, else the normalized absolute path. */
function pathForGlob(path: string, cwd: string): string {
  const t = normPath(path)
  if (!t.absolute) return t.path // already relative (may carry ../ if it escaped)
  const root = normPath(cwd).path.replace(/\/+$/, '')
  if (t.path === root) return ''
  if (t.path.startsWith(root + '/')) return t.path.slice(root.length + 1)
  return t.path
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
      if (action.kind !== 'command' || !pat) return null
      let re: RegExp
      try {
        re = new RegExp(pat, 'i')
      } catch {
        // An uncompilable policy spec fails CLOSED — block, don't ignore.
        return v(`invalid forbid_cmd pattern: ${pat}`)
      }
      return re.test(action.command) ? v(pat) : null
    }
    case 'read_only':
      // First-turn read-only (fix #5): block writes, network, and any command that
      // isn't a pure inspection — a prompt can't be trusted to stay read-only.
      if (action.kind === 'write' || action.kind === 'network') return v()
      if (action.kind === 'command') return isReadOnlyCommand(action.command) ? null : v(action.command)
      return null
    case 'workspace_boundary':
      return action.kind === 'write' && !insideWorkspace(action.path, cwd) ? v(action.path) : null
    case 'forbid_path': {
      const globs = asGlobs(guard.spec)
      const rel = action.kind === 'write' ? pathForGlob(action.path, cwd) : ''
      return action.kind === 'write' && globs.some((g) => globToRegExp(g).test(rel)) ? v(action.path) : null
    }
    case 'allowed_paths': {
      const globs = asGlobs(guard.spec)
      if (action.kind !== 'write' || globs.length === 0) return null
      const rel = pathForGlob(action.path, cwd)
      const allowed = globs.some((g) => globToRegExp(g).test(rel))
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
