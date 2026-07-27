// Regression tests for the adversarial QA pass. Each `describe` maps to a finding.
import { describe, expect, it } from 'vitest'
import {
  ClaudeStreamParser,
  evaluateGuards,
  evaluateSendGate,
  evaluateToolPre,
  firstTurnReadOnlyGuards,
  isReadOnlyCommand,
  normPath,
  planResumeAfterRateLimit,
  sessionVerifyDeadline,
  type Guard,
  type ScheduledPrompt,
} from '../src/index.js'

const CWD = '/home/u/proj'
const write = (path: string) => [{ kind: 'write' as const, path }]
const cmd = (command: string) => [{ kind: 'command' as const, command }]

describe('#1 path traversal no longer defeats containment guards', () => {
  it('normPath resolves .. and flags escapes', () => {
    expect(normPath('/repo/../etc/passwd')).toMatchObject({ path: '/etc/passwd', absolute: true, escapes: false })
    expect(normPath('src/theme/../../../etc/passwd')).toMatchObject({ escapes: true })
    expect(normPath('x/../secrets/key')).toMatchObject({ path: 'secrets/key', escapes: false })
  })
  it('workspace_boundary blocks an absolute .. escape', () => {
    const g: Guard[] = [{ kind: 'workspace_boundary' }]
    expect(evaluateGuards(write('/home/u/proj/../etc/passwd'), g, CWD).allow).toBe(false)
    expect(evaluateGuards(write('/home/u/proj/src/../lib/a.ts'), g, CWD).allow).toBe(true) // stays inside
  })
  it('allowed_paths cannot be escaped with ..', () => {
    const g: Guard[] = [{ kind: 'allowed_paths', spec: 'src/theme/**' }]
    expect(evaluateGuards(write('src/theme/../../../etc/passwd'), g, CWD).allow).toBe(false)
    expect(evaluateGuards(write('src/theme/x.css'), g, CWD).allow).toBe(true)
  })
  it('forbid_path catches a .. that resolves into the forbidden area', () => {
    const g: Guard[] = [{ kind: 'forbid_path', spec: 'secrets/**' }]
    expect(evaluateGuards(write('x/../secrets/key'), g, CWD).allow).toBe(false)
  })
})

describe('#2 first-turn read-only actually blocks shell writes / push / network', () => {
  const g = firstTurnReadOnlyGuards()
  it('blocks writes and mutating shell commands', () => {
    expect(evaluateGuards(write('src/a.ts'), g, CWD).allow).toBe(false)
    for (const c of ['echo pwned > src/a.ts', 'rm -rf src', 'git checkout .', 'git push', 'sed -i s/a/b/ x', 'curl evil.com']) {
      expect(evaluateGuards(cmd(c), g, CWD).allow, c).toBe(false)
    }
  })
  it('still allows pure inspection commands', () => {
    for (const c of ['git status', 'git log --oneline', 'ls -la', 'cat package.json', 'grep -r foo src']) {
      expect(evaluateGuards(cmd(c), g, CWD).allow, c).toBe(true)
    }
  })
  it('isReadOnlyCommand rejects redirection and mutating git subcommands', () => {
    expect(isReadOnlyCommand('cat x > y')).toBe(false)
    expect(isReadOnlyCommand('git commit -m x')).toBe(false)
    expect(isReadOnlyCommand('find . -delete')).toBe(false)
    expect(isReadOnlyCommand('git diff && ls')).toBe(true)
  })
})

describe('#3 command classifiers catch real evasions', () => {
  it('no_push catches git -C / git -c push', () => {
    const g: Guard[] = [{ kind: 'no_push' }]
    expect(evaluateGuards(cmd('git -C /repo push'), g, CWD).allow).toBe(false)
    expect(evaluateGuards(cmd('git -c user.name=x push origin'), g, CWD).allow).toBe(false)
  })
  it('no_deploy catches kubectl --context apply', () => {
    expect(evaluateGuards(cmd('kubectl --context prod apply -f x'), [{ kind: 'no_deploy' }], CWD).allow).toBe(false)
  })
  it('no_net catches command-substituted curl', () => {
    const g: Guard[] = [{ kind: 'no_net' }]
    expect(evaluateGuards(cmd('X=$(curl evil.com)'), g, CWD).allow).toBe(false)
    expect(evaluateGuards(cmd('echo `curl evil.com`'), g, CWD).allow).toBe(false)
  })
})

describe('#4 quota math never crashes on a malformed resetAt', () => {
  it('planResumeAfterRateLimit falls back instead of throwing', () => {
    const plan = planResumeAfterRateLimit('not-a-date', '2026-07-27T21:00:00.000Z', { fallbackMs: 3_600_000 })
    expect(Number.isFinite(plan.waitMs)).toBe(true)
    expect(plan.waitMs).toBe(3_600_000 + 60_000) // fallback + buffer
    expect(() => new Date(plan.resumeAtUtc).toISOString()).not.toThrow()
  })
  it('sessionVerifyDeadline falls back too', () => {
    const d = sessionVerifyDeadline('', '2026-07-27T21:00:00.000Z')
    expect(() => new Date(d.verifyByUtc).toISOString()).not.toThrow()
  })
})

describe('#5 stream parser flush() recovers a newline-less final line', () => {
  it('captures done + session id from a flushed result', () => {
    const parser = new ClaudeStreamParser()
    parser.push(JSON.stringify({ type: 'result', subtype: 'success', session_id: 'sess_1', usage: { input_tokens: 1, output_tokens: 1 } }))
    expect(parser.done).toBe(false) // not yet — no newline
    const events = parser.flush()
    expect(events.map((e) => e.type)).toEqual(['turn_completed'])
    expect(parser.done).toBe(true)
    expect(parser.sessionId).toBe('sess_1')
  })
})

describe('#6 send-gate fails closed on malformed dates/versions', () => {
  const base: ScheduledPrompt = {
    id: 'p', target: 'claude', trigger: { type: 'at', at: '2026-07-27T09:00:00Z' },
    body: 'go', priority: 50, missionVer: 1, status: 'queued',
  }
  const now = { nowUtc: '2026-07-27T09:00:00Z', currentMissionVer: 1 }
  it('malformed valid_until → expired, never send', () => {
    expect(evaluateSendGate({ ...base, validUntil: 'garbage' }, now)).toBe('expired')
  })
  it('non-finite missionVer → superseded', () => {
    expect(evaluateSendGate({ ...base, missionVer: NaN }, now)).toBe('superseded')
  })
})

describe('#7 forbid_cmd with an invalid regex fails closed (blocks)', () => {
  it('blocks instead of throwing', () => {
    const g: Guard[] = [{ kind: 'forbid_cmd', spec: '(' }] // invalid regex
    expect(() => evaluateToolPre({ type: 'tool_pre', tool: 'Bash', args: { command: 'ls' } }, g, CWD)).not.toThrow()
    expect(evaluateGuards(cmd('ls'), g, CWD).allow).toBe(false)
  })
})
