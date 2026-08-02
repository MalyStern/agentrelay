#!/usr/bin/env node
// Cross-platform, fail-SAFE PreToolUse guard wrapper for the AgentRelay plugin.
//
// The plugin registers this on every tool call. The engine it drives — the
// `agentrelay` binary — is per-OS and is NOT bundled, so it may not be installed.
// The wrong behaviours, all of which the first version risked:
//   * error on every call ("command not found") — continuous noise;
//   * block every call — makes Claude Code unusable;
//   * pretend to guard when it cannot.
// The right behaviour when the engine is absent is to ALLOW, quietly — and to make
// that gap loud exactly ONCE, which the SessionStart notice does. So this wrapper:
//   engine present  -> run it, pass the tool call through, mirror its decision;
//   engine absent   -> exit 0 (allow), silently.
// Node is used because it is one command that behaves identically on every OS, which
// a raw shell snippet in hooks.json cannot promise.

import { spawnSync } from 'node:child_process'
import { readFileSync, accessSync } from 'node:fs'
import { join } from 'node:path'

// The tool-call JSON Claude Code writes to our stdin. Read it whole so we can hand it
// to the engine unchanged.
let raw = ''
try {
  raw = readFileSync(0, 'utf8')
} catch {
  /* no stdin — treat as empty; the engine will decide, or we allow below */
}

// Scope to the project. The env var is Claude Code's; the stdin `cwd` is the
// fallback; the process cwd is the last resort.
let cwd = process.env.CLAUDE_PROJECT_DIR || ''
if (!cwd) {
  try {
    cwd = JSON.parse(raw).cwd || ''
  } catch {
    /* not JSON we can read — leave cwd for the fallbacks */
  }
}
if (!cwd) cwd = process.cwd()

function onPath(name) {
  const win = process.platform === 'win32'
  const exts = win ? ['.exe', '.cmd', '.bat'] : ['']
  for (const dir of (process.env.PATH || '').split(win ? ';' : ':')) {
    if (!dir) continue
    for (const ext of exts) {
      try {
        accessSync(join(dir, name + ext))
        return true
      } catch {
        /* keep looking */
      }
    }
  }
  return false
}

// Engine not installed: allow, quietly. The SessionStart notice already said once
// that supervision is inactive, so this must NOT add per-call noise.
if (!onPath('agentrelay')) {
  process.exit(0)
}

// Engine present: run it with the same stdin, and mirror its verdict — exit 2 is the
// documented "deny" signal, and it must reach Claude Code unchanged.
const r = spawnSync('agentrelay', ['hook', '--cwd', cwd], {
  input: raw,
  stdio: ['pipe', 'inherit', 'inherit'],
})
process.exit(typeof r.status === 'number' ? r.status : 0)
