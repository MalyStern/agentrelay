#!/usr/bin/env node
// SessionStart notice for the AgentRelay plugin.
//
// This is where the "engine not installed" gap is made loud — ONCE per session,
// not once per tool call. If `agentrelay` is on PATH the plugin's guards are real;
// if it is not, the guard wrapper allows everything quietly, and a person needs to
// know that supervision is off rather than assume it is on. Saying it here keeps the
// per-call path silent while keeping the fact honest.

import { accessSync } from 'node:fs'
import { join } from 'node:path'

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

if (!onPath('agentrelay')) {
  console.log(
    'AgentRelay plugin: the `agentrelay` engine is not on your PATH, so its guards are ' +
      'NOT active this session — tool calls run unsupervised. Build/install the binary ' +
      '(see the plugin README) to arm the guards, or run `/plugin disable agentrelay` to ' +
      'remove this notice. Work continues normally either way.',
  )
}
process.exit(0)
