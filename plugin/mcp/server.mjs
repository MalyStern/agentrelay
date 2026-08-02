#!/usr/bin/env node
// AgentRelay MCP server — dependency-free.
//
// Gives Claude Code real tools for a supervised run (status, the morning report,
// checkpoints, validators, the quota-window check) instead of only slash commands.
// It shells out to the local `agentrelay` binary, scoped to a workspace directory.
//
// No npm dependency and no build step on purpose: the whole product is local-first
// and self-contained, and a plugin that needed `npm install` before its MCP server
// would run would break that promise. So this implements the MCP stdio contract
// (newline-delimited JSON-RPC 2.0: initialize / tools/list / tools/call) by hand.
//
// If the `agentrelay` engine is not on PATH, the server STILL starts and lists its
// tools — each call then returns a clear "engine not installed" error rather than
// crashing, so the failure is legible instead of a dead server.

import { spawnSync } from 'node:child_process'
import { accessSync } from 'node:fs'
import { join } from 'node:path'

const SERVER = { name: 'agentrelay', version: '0.1.0' }

// Resolve a program to its real file the way the shell would, or null if not on PATH.
// This matters on Windows: Node's spawnSync('agentrelay', …) does NOT append .exe or
// search PATHEXT, so a bare name ENOENTs even though `agentrelay.exe` is right there —
// the same trap documented in the daemon's which.rs. So resolve the full path and
// spawn THAT.
function resolveOnPath(bin) {
  const win = process.platform === 'win32'
  const exts = win ? ['.exe', '.cmd', '.bat', ''] : ['']
  for (const dir of (process.env.PATH || '').split(win ? ';' : ':')) {
    if (!dir) continue
    for (const ext of exts) {
      const full = join(dir, bin + ext)
      try {
        accessSync(full)
        return full
      } catch {
        /* keep looking */
      }
    }
  }
  return null
}

// Spawn a resolved program with args. A .cmd/.bat launcher can't be executed directly
// on Windows; only cmd.exe can run it. Returns the spawnSync result, or null if the
// program isn't installed.
function spawnResolved(bin, args, cwd) {
  const full = resolveOnPath(bin)
  if (!full) return null
  const opts = { cwd: cwd && cwd.trim() ? cwd : process.cwd(), encoding: 'utf8', timeout: 120000 }
  const lower = full.toLowerCase()
  if (process.platform === 'win32' && (lower.endsWith('.cmd') || lower.endsWith('.bat'))) {
    return spawnSync('cmd', ['/c', full, ...args], opts)
  }
  return spawnSync(full, args, opts)
}

// Every tool is scoped to a workspace via `cwd`, defaulting to the current directory.
const CWD = {
  type: 'string',
  description: 'The project directory (where you ran agentrelay init). Defaults to the current directory.',
}

const TOOLS = [
  {
    name: 'agentrelay_status',
    description:
      "The workspace's supervision status in plain words: which guards are armed and whether the blocking hook is installed, the spending cap, isolation, and whether the mission is waiting on a person.",
    inputSchema: { type: 'object', properties: { cwd: CWD } },
    run: (a) => run(['status'], a.cwd),
  },
  {
    name: 'agentrelay_report',
    description:
      'The morning report: what the run actually did, every line labelled VERIFIED (a command decided it), RECORDED (observed), or ESTIMATED (a guess). Nothing the model merely claimed appears as fact.',
    inputSchema: {
      type: 'object',
      properties: { cwd: CWD, short: { type: 'boolean', description: 'Just the one-line headline.' } },
    },
    run: (a) => run(a.short ? ['report', '--short'] : ['report'], a.cwd),
  },
  {
    name: 'agentrelay_checkpoints',
    description: 'List the git checkpoints the supervisor made, newest first — validated units you can return to.',
    inputSchema: { type: 'object', properties: { cwd: CWD } },
    run: (a) => run(['checkpoints'], a.cwd),
  },
  {
    name: 'agentrelay_validate',
    description:
      "Run the workspace's configured validators (tests/build/lint) as evidence and report PASS/FAIL per check. This is read-only — it does not commit. A unit is 'done' only if these pass.",
    inputSchema: {
      type: 'object',
      properties: { cwd: CWD, unit: { type: 'string', description: 'A short unit id to record under.' } },
    },
    run: (a) => run(['validate', '--unit', a.unit || 'default'], a.cwd),
  },
  {
    name: 'agentrelay_fits',
    description:
      'Would a unit of roughly N minutes fit before the quota window closes? Exit says go or wait — being cut off mid-unit leaves half-done, unvalidated work.',
    inputSchema: {
      type: 'object',
      properties: {
        cwd: CWD,
        minutes: { type: 'number', description: 'Rough length of the unit, in minutes (default 30).' },
      },
    },
    run: (a) => run(['fits', '--minutes', String(a.minutes ?? 30)], a.cwd),
  },
  {
    name: 'agentrelay_graph',
    description:
      'Build (or refresh) a knowledge graph of the project with graphify, so you can query its structure. Default is the free AST-only build (no tokens); set deep:true for the richer LLM extraction (spends tokens). Returns where graph.html / GRAPH_REPORT.md / graph.json were written. Routed through agentrelay, so if graphify is not installed it says exactly how to add it.',
    inputSchema: {
      type: 'object',
      properties: {
        cwd: CWD,
        deep: { type: 'boolean', description: 'Use graphify LLM extraction (spends tokens) instead of the free AST build.' },
      },
    },
    run: (a) => run(a.deep ? ['graph', '--deep'] : ['graph'], a.cwd),
  },
  {
    name: 'agentrelay_graph_query',
    description:
      'Ask a natural-language question about the project against the knowledge graph built by agentrelay_graph — e.g. "how does the guard decide to block a call?". Much cheaper than re-reading files. Needs graphify installed and a graph already built; says so plainly if either is missing.',
    inputSchema: {
      type: 'object',
      properties: {
        cwd: CWD,
        question: { type: 'string', description: 'The question to answer from the graph.' },
        budget: { type: 'number', description: 'Cap the answer at N tokens (default 2000).' },
      },
    },
    run: (a) => runGraphify(['query', a.question || '', '--budget', String(a.budget ?? 2000)], a.cwd),
  },
]

function run(args, cwd) {
  const r = spawnResolved('agentrelay', args, cwd)
  if (!r) {
    return {
      isError: true,
      text:
        'The `agentrelay` engine is not on PATH, so this tool cannot run. Build/install it (see the AgentRelay plugin README) and try again. Supervision is not active until it is installed.',
    }
  }
  if (r.error) {
    return { isError: true, text: `could not run agentrelay: ${r.error.message}` }
  }
  const out = `${r.stdout || ''}${r.stderr || ''}`.trim() || '(no output)'
  // A non-zero exit is often meaningful here (validators failed, a unit is not done,
  // fits says wait), not a crash — so it is reported as text with the code, not
  // hidden as an error.
  return { isError: false, text: `exit ${r.status}\n${out}` }
}

// Query path talks to graphify directly (agentrelay has no query subcommand). Same
// fail-safe discipline: if graphify is absent, or no graph has been built yet, say so
// in words rather than failing the call.
function runGraphify(args, cwd) {
  const r = spawnResolved('graphify', args, cwd)
  if (!r) {
    return {
      isError: true,
      text:
        'graphify is not installed, so there is no graph to query. Install it with `uv tool install graphifyy` (or `pip install graphifyy`), build a graph with the agentrelay_graph tool, then ask again.',
    }
  }
  if (r.error) {
    return { isError: true, text: `could not run graphify: ${r.error.message}` }
  }
  const out = `${r.stdout || ''}${r.stderr || ''}`.trim() || '(no output)'
  if (r.status !== 0 && /no such file|graph\.json|not found/i.test(out)) {
    return {
      isError: true,
      text: `no knowledge graph found in this workspace yet — build one first with the agentrelay_graph tool.\n\n${out}`,
    }
  }
  return { isError: false, text: `exit ${r.status}\n${out}` }
}

// ── minimal JSON-RPC over newline-delimited stdio ────────────────────────────

function send(msg) {
  process.stdout.write(JSON.stringify(msg) + '\n')
}

function handle(req) {
  const { id, method, params } = req
  // Notifications (no id) get no response.
  const isRequest = id !== undefined && id !== null

  if (method === 'initialize') {
    // Echo the client's protocol version when given — the safe interop choice.
    const protocolVersion = params?.protocolVersion || '2024-11-05'
    return send({
      jsonrpc: '2.0',
      id,
      result: { protocolVersion, capabilities: { tools: {} }, serverInfo: SERVER },
    })
  }
  if (method === 'tools/list') {
    return send({
      jsonrpc: '2.0',
      id,
      result: {
        tools: TOOLS.map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })),
      },
    })
  }
  if (method === 'tools/call') {
    const tool = TOOLS.find((t) => t.name === params?.name)
    if (!tool) {
      return send({ jsonrpc: '2.0', id, error: { code: -32602, message: `unknown tool: ${params?.name}` } })
    }
    const { text, isError } = tool.run(params.arguments || {})
    return send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text }], isError } })
  }
  // ping and other requests: answer with an empty result so the client isn't left
  // waiting; unknown notifications are simply ignored.
  if (isRequest) {
    if (method === 'ping') return send({ jsonrpc: '2.0', id, result: {} })
    return send({ jsonrpc: '2.0', id, error: { code: -32601, message: `method not found: ${method}` } })
  }
}

let buffer = ''
process.stdin.setEncoding('utf8')
process.stdin.on('data', (chunk) => {
  buffer += chunk
  let nl
  while ((nl = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, nl).trim()
    buffer = buffer.slice(nl + 1)
    if (!line) continue
    try {
      handle(JSON.parse(line))
    } catch {
      // A malformed line must not take the server down — a strict MCP client should
      // never send one, but a dead server is worse than a dropped message.
    }
  }
})
process.stdin.on('end', () => process.exit(0))
