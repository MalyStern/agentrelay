# AgentRelay

**Run AI coding agents overnight, unattended — and actually keep control.** A local, single-source-of-truth daemon that supervises Claude Code / Codex / ACP agents through quota resets, enforces guardrails, checkpoints work, and stops when it should — with everything it shows you tagged by provenance (**Measured** / **Estimated** / **Provider**).

> Status: **early build.** See [`STATUS.md`](STATUS.md) for exactly what's implemented-and-tested vs scaffolded. The authoritative design is [`docs/02-תוכנית-סופית.md`](docs/02-תוכנית-סופית.md) (the final plan, hardened by three adversarial review passes).

## Why it exists

Autonomous overnight agent runs fail in specific, well-understood ways: the quota resets mid-task and the agent restarts from scratch (or drifts), a stray `push`/`deploy` slips through, "stop" doesn't actually kill the Docker/WSL children, secrets end up on disk, and the machine falls asleep at 2am. AgentRelay is the supervisor that handles each of those deterministically, so a long task survives the night.

## Architecture (target)

- **Daemon** — Rust + tokio, the single source of truth (Mission Engine, Scheduler, Policy/Watchtower, Quota Manager, Process Manager, Timeline, Credential Broker, Isolation Manager, Context Optimizer). Storage: SQLite (WAL), hybrid append-only-log + state-tables.
- **UI** — Tauri 2 + React/TS, `i18next`+ICU, RTL + a11y, 23 languages.
- **Provider engines** — Node subprocesses behind one normalized contract: Claude **Agent SDK** + Hooks, Codex **App-Server** JSON-RPC, generic **ACP** (~10 agents), and **Cloud-delegated** (Devin / Copilot Agent-Tasks).

The single principle throughout: **detect facts, never guess** — and every surfaced value carries its provenance.

## Repo layout

```
docs/          the design (00 master · 01 context-optimizer · 02 FINAL plan) + ADRs
runner/        Node/TS: normalized contracts + provider adapters + Phase-0 test harness  ← built & tested here
daemon/        Rust workspace (the orchestration core)                                    ← scaffolded (see ADR-0001)
```

**Why the split:** this build environment can't install the Rust toolchain (TLS-intercepted downloads fail), and the plan's own rule is *don't write what you can't verify*. So the verifiable half — the language-neutral contracts, the Node-native adapters the daemon spawns, and the test harness that makes `resume`/`kill`/rate-limit testable without burning quota — is built and tested here in TypeScript, acting as an **executable spec** for the Rust port. Details: [`docs/adr/0001-environment-and-language-split.md`](docs/adr/0001-environment-and-language-split.md).

## Develop (runner)

```bash
cd runner
npm install
npm test        # vitest — the Phase-0 contract + fake-adapter + record/replay suite
npm run typecheck
```
