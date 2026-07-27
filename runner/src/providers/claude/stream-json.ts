// Claude Code adapter core: parse `claude -p --output-format stream-json` NDJSON
// into normalized AgentEvents (plan 00 §5.1). This is a PURE function of a parsed
// line, so it's fully testable with sample lines — no Claude, no quota. The Agent
// SDK path emits the same message shapes.
//
// NOTE (plan 02 §8.7): the exact stream-json shape can shift between CLI versions;
// this parser is defensive and MUST be backed by a live contract-test/canary. The
// rate-limit mapping in particular needs live data — see `looksLikeRateLimit`.

import type { AgentEvent, Usage } from '../../contracts/index.js'

export interface ClaudeParseResult {
  events: AgentEvent[]
  /** Session id, when a line carries it (init + result) — needed for --resume. */
  sessionId?: string
  /** True on the terminal `result` line. */
  done?: boolean
}

function mapUsage(u: unknown): Usage {
  const o = (u ?? {}) as Record<string, number>
  return {
    inputTokens: o.input_tokens ?? 0,
    outputTokens: o.output_tokens ?? 0,
    ...(o.cache_read_input_tokens != null ? { cacheReadTokens: o.cache_read_input_tokens } : {}),
    ...(o.cache_creation_input_tokens != null ? { cacheCreationTokens: o.cache_creation_input_tokens } : {}),
    provenance: 'provider',
  }
}

/** Heuristic — needs a live contract test to confirm the exact wording/shape. */
export function looksLikeRateLimit(text: string): boolean {
  return /rate.?limit|usage limit|too many requests|\b429\b|quota (?:exceeded|reached)/i.test(text)
}

/** Parse one already-JSON-parsed stream-json line. */
export function parseClaudeLine(line: unknown): ClaudeParseResult {
  if (!line || typeof line !== 'object') return { events: [] }
  const l = line as Record<string, unknown>
  const type = l.type
  const sessionId = typeof l.session_id === 'string' ? l.session_id : undefined
  const withSid = (r: ClaudeParseResult): ClaudeParseResult => (sessionId ? { ...r, sessionId } : r)

  if (type === 'system') {
    // init line — carries session_id, tools, model. Marks the turn's start.
    return withSid({ events: l.subtype === 'init' ? [{ type: 'turn_started' }] : [] })
  }

  if (type === 'assistant') {
    const content = (((l.message as Record<string, unknown>)?.content) ?? []) as Record<string, unknown>[]
    const events: AgentEvent[] = []
    for (const block of content) {
      if (block.type === 'text' && typeof block.text === 'string') {
        events.push({ type: 'text_delta', text: block.text })
      } else if (block.type === 'tool_use' && typeof block.name === 'string') {
        events.push({ type: 'tool_pre', tool: block.name, args: block.input ?? {} })
      }
    }
    return withSid({ events })
  }

  if (type === 'user') {
    const content = (((l.message as Record<string, unknown>)?.content) ?? []) as Record<string, unknown>[]
    const events: AgentEvent[] = []
    for (const block of content) {
      if (block.type === 'tool_result') {
        events.push({ type: 'tool_post', tool: String(block.tool_use_id ?? 'tool'), result: block.content ?? null })
      }
    }
    return withSid({ events })
  }

  if (type === 'result') {
    const usage = mapUsage(l.usage)
    const isError = l.is_error === true || (typeof l.subtype === 'string' && l.subtype.startsWith('error'))
    const text = [l.result, l.error, l.subtype].filter((x) => typeof x === 'string').join(' ')
    if (isError && looksLikeRateLimit(text)) {
      // Claude-headless usually doesn't give a resetAt; emit the quota code and let
      // the Quota Manager apply a fallback reset (see quota-manager fallback).
      return withSid({ events: [{ type: 'error', message: text || 'rate limited', code: 'quota.rate_limited' }], done: true })
    }
    if (isError) {
      return withSid({ events: [{ type: 'error', message: text || 'error', code: 'provider.stream_error' }], done: true })
    }
    return withSid({ events: [{ type: 'turn_completed', usage }], done: true })
  }

  return withSid({ events: [] })
}

/**
 * Stateful NDJSON reader: feed raw stdout chunks, get normalized events. Buffers
 * partial lines, ignores blank/non-JSON lines, and remembers the session id.
 */
export class ClaudeStreamParser {
  private buffer = ''
  private _sessionId: string | undefined
  private _done = false

  get sessionId(): string | undefined {
    return this._sessionId
  }
  get done(): boolean {
    return this._done
  }

  /** Feed a chunk of stdout; returns the events parsed from any completed lines. */
  push(chunk: string): AgentEvent[] {
    this.buffer += chunk
    const events: AgentEvent[] = []
    let nl: number
    while ((nl = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, nl).trim()
      this.buffer = this.buffer.slice(nl + 1)
      if (!line) continue
      let obj: unknown
      try {
        obj = JSON.parse(line)
      } catch {
        continue // tolerate non-JSON noise on stdout
      }
      const r = parseClaudeLine(obj)
      if (r.sessionId) this._sessionId = r.sessionId
      if (r.done) this._done = true
      events.push(...r.events)
    }
    return events
  }
}
