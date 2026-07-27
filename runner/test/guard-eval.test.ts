import { describe, expect, it } from 'vitest'
import { deriveActions, evaluateGuards, evaluateToolPre, globToRegExp } from '../src/index.js'
import type { AgentEvent, Guard } from '../src/index.js'

const toolPre = (tool: string, args: unknown): Extract<AgentEvent, { type: 'tool_pre' }> => ({ type: 'tool_pre', tool, args })
const CWD = '/home/u/proj'

describe('deriveActions', () => {
  it('maps write / shell / network tools to neutral actions', () => {
    expect(deriveActions(toolPre('Write', { file_path: 'src/a.ts' }))).toEqual([{ kind: 'write', path: 'src/a.ts' }])
    expect(deriveActions(toolPre('Edit', { path: 'b.css' }))).toEqual([{ kind: 'write', path: 'b.css' }])
    expect(deriveActions(toolPre('Bash', { command: 'git status' }))).toEqual([{ kind: 'command', command: 'git status' }])
    expect(deriveActions(toolPre('WebFetch', { url: 'https://x.com' }))).toEqual([{ kind: 'network', target: 'https://x.com' }])
    expect(deriveActions(toolPre('SomeUnknownTool', {}))).toEqual([])
  })
})

describe('command guards', () => {
  const g = (kind: Guard['kind']): Guard[] => [{ kind }]
  it('no_push blocks git push, allows other git', () => {
    expect(evaluateGuards([{ kind: 'command', command: 'git push origin main' }], g('no_push'), CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'command', command: 'git commit -m x' }], g('no_push'), CWD).allow).toBe(true)
  })
  it('no_deploy blocks deploy tooling', () => {
    for (const cmd of ['vercel deploy', 'kubectl apply -f x', 'terraform apply', 'fly deploy']) {
      expect(evaluateGuards([{ kind: 'command', command: cmd }], g('no_deploy'), CWD).allow).toBe(false)
    }
    expect(evaluateGuards([{ kind: 'command', command: 'npm run build' }], g('no_deploy'), CWD).allow).toBe(true)
  })
  it('no_net blocks network tools and network commands', () => {
    expect(evaluateGuards([{ kind: 'network', target: 'x' }], g('no_net'), CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'command', command: 'curl https://evil' }], g('no_net'), CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'command', command: 'ls -la' }], g('no_net'), CWD).allow).toBe(true)
  })
  it('no_dependency blocks installs across ecosystems', () => {
    for (const cmd of ['npm install left-pad', 'pnpm add react', 'pip install requests', 'cargo add serde', 'go get x']) {
      expect(evaluateGuards([{ kind: 'command', command: cmd }], g('no_dependency'), CWD).allow).toBe(false)
    }
    expect(evaluateGuards([{ kind: 'command', command: 'npm run test' }], g('no_dependency'), CWD).allow).toBe(true)
  })
  it('forbid_cmd blocks a custom pattern', () => {
    const guards: Guard[] = [{ kind: 'forbid_cmd', spec: 'rm\\s+-rf' }]
    expect(evaluateGuards([{ kind: 'command', command: 'rm -rf /' }], guards, CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'command', command: 'rm file.txt' }], guards, CWD).allow).toBe(true)
  })
})

describe('path guards', () => {
  it('workspace_boundary blocks writes outside the workspace and parent escapes', () => {
    const guards: Guard[] = [{ kind: 'workspace_boundary' }]
    expect(evaluateGuards([{ kind: 'write', path: '/home/u/proj/src/a.ts' }], guards, CWD).allow).toBe(true)
    expect(evaluateGuards([{ kind: 'write', path: '/etc/passwd' }], guards, CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'write', path: '../secrets.txt' }], guards, CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'write', path: 'src/ok.ts' }], guards, CWD).allow).toBe(true)
  })
  it('allowed_paths permits only matching globs (e.g. a CSS-only unit)', () => {
    const guards: Guard[] = [{ kind: 'allowed_paths', spec: ['**/*.css', 'src/theme/**'] }]
    expect(evaluateGuards([{ kind: 'write', path: 'src/a.css' }], guards, CWD).allow).toBe(true)
    expect(evaluateGuards([{ kind: 'write', path: 'src/theme/x.ts' }], guards, CWD).allow).toBe(true)
    const denied = evaluateGuards([{ kind: 'write', path: 'src/logic.ts' }], guards, CWD)
    expect(denied.allow).toBe(false)
    expect(denied.violations[0]).toMatchObject({ guard: 'allowed_paths', reasonCode: 'guard.allowed_paths' })
  })
  it('forbid_path blocks matching globs', () => {
    const guards: Guard[] = [{ kind: 'forbid_path', spec: ['**/*.env', 'infra/**'] }]
    expect(evaluateGuards([{ kind: 'write', path: 'app/.env' }], guards, CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'write', path: 'infra/deploy.yml' }], guards, CWD).allow).toBe(false)
    expect(evaluateGuards([{ kind: 'write', path: 'src/a.ts' }], guards, CWD).allow).toBe(true)
  })
})

describe('evaluateToolPre (end-to-end)', () => {
  it('blocks a disallowed write from a tool_pre event and reports the violation', () => {
    const guards: Guard[] = [{ kind: 'allowed_paths', spec: '**/*.css' }]
    const decision = evaluateToolPre(toolPre('Write', { file_path: 'src/logic.ts' }), guards, CWD)
    expect(decision.allow).toBe(false)
    expect(decision.violations).toHaveLength(1)
    expect(decision.violations[0]!.action).toEqual({ kind: 'write', path: 'src/logic.ts' })
  })
  it('allows when every guard is satisfied', () => {
    const guards: Guard[] = [{ kind: 'no_push' }, { kind: 'workspace_boundary' }]
    expect(evaluateToolPre(toolPre('Bash', { command: 'npm test' }), guards, CWD).allow).toBe(true)
  })
})

describe('globToRegExp', () => {
  it('handles ** and *', () => {
    expect(globToRegExp('**/*.css').test('src/a/b.css')).toBe(true)
    expect(globToRegExp('src/theme/**').test('src/theme/x/y.ts')).toBe(true)
    expect(globToRegExp('*.ts').test('a/b.ts')).toBe(false)
  })
})
