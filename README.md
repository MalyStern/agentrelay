# AgentRelay

**AgentRelay is a local supervisor (a Rust daemon) for AI coding agents that run
unattended** — Claude Code, with adapters for Codex and ACP agents. It blocks a
forbidden tool call *before* it runs, refuses to call work "done" without passing
tests, checkpoints every validated unit into git so a bad night is one command to
undo, caps spending, and writes a morning report where **every line says where it came
from** — so you can tell evidence from a guess at 8am.

In one sentence: **run an AI coding agent overnight and keep control.** Everything runs
on your machine. No account, no telemetry, nothing uploaded.

## The idea in one line

An overnight agent run fails in known ways — it drifts onto the wrong problem, a stray
`git push` slips out, "stop" doesn't really stop, it burns the night repeating one
broken fix, or it claims success it can't prove. AgentRelay handles each of those
**deterministically**, and never presents a guess as a fact.

## When to use it

Reach for AgentRelay when you want an AI coding agent to work while you're not
watching, and you need guarantees instead of hope:

- **Run an agent overnight on a real repository** and wake up to validated commits, not
  a pile of unreviewed edits or a broken tree.
- **Let an agent grind on a failing test suite or a refactor** for hours, gated so it
  can only mark a unit "done" when your tests actually pass.
- **Stop an agent from doing something irreversible** — pushing to `main`, deploying,
  writing outside the workspace, reading `.env` — *before* the call runs, not after.
- **Cap what an unattended run can spend**, checked before every turn, because money is
  the one thing a `git reset` can't restore.
- **Keep an agent's overnight work undoable** — every validated step is a git
  checkpoint that touches only the agent's files, never your own uncommitted work.
- **Get a trustworthy morning summary** that separates what a command verified from what
  the model merely claimed.
- **Contain an untrusted agent** in a network-off container when the guards alone
  aren't enough.
- **Map an unfamiliar codebase** the agent is about to work on into a queryable
  knowledge graph (via the optional graphify integration).

If instead you want a hosted, fully-autonomous agent with no local control surface,
AgentRelay is the opposite of that on purpose — its whole value is the control layer.

## What it does

| | |
|---|---|
| 🛡️ **Guards that block** | A PreToolUse hook stops a forbidden call (a push, a write outside the workspace, touching `.env`) *before* it runs — verified against a live Claude, not just in theory. |
| ✅ **Evidence-gated "done"** | A unit is done only when your real validators (`npm test`, `cargo test`, …) pass. The tool's exit code decides, not the model's claim. |
| 📌 **Undoable checkpoints** | Each passing unit is a git commit authored as AgentRelay — and it commits **only the agent's work**, never your own uncommitted edits. Undo is `git reset --keep`, which refuses rather than discards. |
| 💰 **Budget kill-switch** | A spending cap checked *before* each turn. Money is the one thing git can't restore. |
| 🌙 **Survives the night** | Reads the quota reset time to schedule around the wall, keeps the machine awake, and resumes without re-reading the whole repo. |
| 📊 **A report you can trust** | Every line tagged **VERIFIED** (a command decided it) / **RECORDED** (observed) / **ESTIMATED** (a rule of thumb). Nothing the model merely claimed appears. |
| 🐳 **Optional isolation** | Run the agent in a container with the network off by default — the one layer that doesn't depend on recognising a command. |
| 🕸️ **Optional knowledge graph** | Build a queryable map of the codebase with `agentrelay graph` (via the external [graphify](https://pypi.org/project/graphifyy/) tool) — free AST-only by default, so the agent can query structure instead of re-reading files. |

## How to use it

**As a Claude Code plugin** (installing it installs the guards):

```
/plugin marketplace add MalyStern/agentrelay
/plugin install agentrelay@malystern
```

The plugin ships the PreToolUse guard, slash commands (`/agentrelay:status`,
`:report`, `:done`, …), a skill that teaches the safe workflow, and a dependency-free
MCP server. It needs the engine binary once:

```bash
cargo install --path daemon    # puts `agentrelay` on your PATH
```

**As a CLI:**

```bash
agentrelay init                       # scaffold .agentrelay/config.json
#   edit it: guards, your real validators, a budget, the scope
agentrelay begin --unit u1            # record the tree before the agent works
#   ... the agent does the unit ...
agentrelay done "unit 1" --unit u1    # validate → checkpoint only if it passes
agentrelay report                     # the morning report, every line sourced
```

**As a desktop app** — a native window (Tauri) that starts the daemon and reads the
report for you, no terminal, no token to paste (`app/`).

## Status — honest

Phases 1–4 are **built and tested: 284 Rust tests**, plus a portable end-to-end
verification harness (`scripts/verify.ps1` / `scripts/verify.sh`) that drives the real
binary through init → validate → the checkpoint-safety → the report → the guard
actually denying a `.env` write. Every push runs the daemon's tests, clippy (warnings
are errors), and formatting on Linux, Windows, and macOS
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)). The desktop app builds and
launches; the plugin's hook was verified blocking a live Claude tool call.

What still needs a human: one real overnight run against a live Claude to confirm the
quota-reset resume behaviour end-to-end (it's tested against a fake agent and the real
stream format, but not a full live night), a visual pass over the app, and
native-speaker review of the non-English message catalogues. See
[`CONTINUITY.md`](CONTINUITY.md) for the detailed state.

## Layout

```
daemon/    the Rust supervisor — the product. 284 tests.
app/       the Tauri desktop app (native window over the daemon's local API)
plugin/    the Claude Code plugin (hook + commands + skill + MCP server)
ui/        a standalone browser UI (demo mode + live)
i18n/      message catalogue, 7 languages, with a bidi/plural validator
scripts/   verify.ps1 / verify.sh (portable harness) + make-sandbox.ps1 (demo workspace)
docs/      the authoritative design (Hebrew) + ADRs
```

Build and verify everything:

```bash
cd daemon && cargo test          # the core, 284 tests
```

```bash
pwsh scripts/verify.ps1          # Windows/macOS/Linux: builds + tests + a real lifecycle
```

```bash
bash scripts/verify.sh           # the same, for Linux/macOS shells
```

## Design principle

**Detect facts, never guess** — and make every gap loud, once, rather than failing
quietly. A supervisor you can't audit is a supervisor you have to redo by hand, so the
report is sourced, an unset cap says so out loud, a missing measurement is "not
measured" and never 0, and a guard that can't be armed says so instead of pretending.

## License

MIT.

---

<sub>Keywords: supervise Claude Code overnight, autonomous AI coding agent guardrails,
PreToolUse hook block, evidence-gated agent completion, git checkpoint AI agent work,
budget cap for AI agents, Claude Code plugin, MCP server, ACP agent supervision, Codex
exec supervisor, quota-reset resume, local-first no-telemetry agent runner, knowledge
graph of codebase, graphify integration, Rust + Tauri.</sub>
