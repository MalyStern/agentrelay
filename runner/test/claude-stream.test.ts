import { describe, expect, it } from 'vitest'
import { ClaudeStreamParser, looksLikeRateLimit, parseClaudeLine, type AgentEvent } from '../src/index.js'

describe('parseClaudeLine', () => {
  it('init → turn_started + session id', () => {
    const r = parseClaudeLine({ type: 'system', subtype: 'init', session_id: 'sess_abc', model: 'claude', tools: [] })
    expect(r.events).toEqual([{ type: 'turn_started' }])
    expect(r.sessionId).toBe('sess_abc')
  })

  it('assistant text → text_delta, tool_use → tool_pre', () => {
    expect(parseClaudeLine({ type: 'assistant', message: { content: [{ type: 'text', text: 'hello' }] } }).events).toEqual([
      { type: 'text_delta', text: 'hello' },
    ])
    expect(
      parseClaudeLine({ type: 'assistant', message: { content: [{ type: 'tool_use', id: 't1', name: 'Bash', input: { command: 'ls' } }] } }).events,
    ).toEqual([{ type: 'tool_pre', tool: 'Bash', args: { command: 'ls' } }])
  })

  it('user tool_result → tool_post', () => {
    expect(
      parseClaudeLine({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'file list' }] } }).events,
    ).toEqual([{ type: 'tool_post', tool: 't1', result: 'file list' }])
  })

  it('result success → turn_completed with mapped usage; sets done', () => {
    const r = parseClaudeLine({
      type: 'result',
      subtype: 'success',
      is_error: false,
      session_id: 'sess_abc',
      usage: { input_tokens: 100, output_tokens: 40, cache_read_input_tokens: 20 },
    })
    expect(r.done).toBe(true)
    expect(r.events).toEqual([
      { type: 'turn_completed', usage: { inputTokens: 100, outputTokens: 40, cacheReadTokens: 20, provenance: 'provider' } },
    ])
  })

  it('result rate-limit error → error with quota code', () => {
    const r = parseClaudeLine({ type: 'result', subtype: 'error_during_execution', is_error: true, result: 'You have hit the usage limit' })
    expect(r.events).toEqual([{ type: 'error', message: 'You have hit the usage limit error_during_execution', code: 'quota.rate_limited' }])
    expect(r.done).toBe(true)
  })

  it('result generic error → provider.stream_error', () => {
    const r = parseClaudeLine({ type: 'result', is_error: true, error: 'boom' })
    expect(r.events[0]).toMatchObject({ type: 'error', code: 'provider.stream_error' })
  })

  it('ignores unknown or malformed lines', () => {
    expect(parseClaudeLine({ type: 'whatever' }).events).toEqual([])
    expect(parseClaudeLine(null).events).toEqual([])
    expect(parseClaudeLine('nope').events).toEqual([])
  })
})

describe('looksLikeRateLimit', () => {
  it('matches common phrasings, not normal text', () => {
    for (const s of ['rate limit exceeded', 'usage limit reached', 'Error 429', 'too many requests']) {
      expect(looksLikeRateLimit(s)).toBe(true)
    }
    expect(looksLikeRateLimit('build succeeded')).toBe(false)
  })
})

describe('ClaudeStreamParser (NDJSON, buffered)', () => {
  it('parses a full turn across chunk boundaries and captures session + done', () => {
    const lines = [
      JSON.stringify({ type: 'system', subtype: 'init', session_id: 'sess_1' }),
      JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: 'thinking' }] } }),
      JSON.stringify({ type: 'assistant', message: { content: [{ type: 'tool_use', name: 'Write', input: { file_path: 'a.ts' } }] } }),
      JSON.stringify({ type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'ok' }] } }),
      JSON.stringify({ type: 'result', subtype: 'success', session_id: 'sess_1', usage: { input_tokens: 5, output_tokens: 2 } }),
    ]
    const ndjson = lines.join('\n') + '\n'
    const parser = new ClaudeStreamParser()

    // Feed in awkward chunks, including a split mid-line.
    const mid = Math.floor(ndjson.length / 2)
    const events: AgentEvent[] = []
    events.push(...parser.push(ndjson.slice(0, mid)))
    events.push(...parser.push(ndjson.slice(mid)))

    expect(events.map((e) => e.type)).toEqual(['turn_started', 'text_delta', 'tool_pre', 'tool_post', 'turn_completed'])
    expect(parser.sessionId).toBe('sess_1')
    expect(parser.done).toBe(true)
  })

  it('tolerates blank and non-JSON lines on stdout', () => {
    const parser = new ClaudeStreamParser()
    const events = parser.push('\nnot json\n' + JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: 'x' }] } }) + '\n')
    expect(events).toEqual([{ type: 'text_delta', text: 'x' }])
  })
})
