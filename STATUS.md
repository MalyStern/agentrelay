# AgentRelay — build status

Tracks each component: **✅ built + tested** here (TS, verifiable), **🟨 scaffolded** (structure/types, pending the Rust machine), **⬜ not started**. Phases are from the final plan ([`docs/02`](docs/02-תוכנית-סופית.md) §יד).

Legend: component — state — notes.

## Phase 0 — spikes + foundation

| Component | State | Notes |
|---|---|---|
| Normalized contracts (`AgentEvent`, `ProviderAdapter`, `QuotaState`, error taxonomy, event kinds) | ✅ | `runner/src/contracts`. Language-neutral; the Rust `trait`/`enum` mirror these. |
| Fake `ProviderAdapter` | ✅ | `runner/src/providers/fake`. Scripted turns; reproduces rate-limit-mid-turn, session-GC, interrupt. |
| Record / replay harness | ✅ | `runner/src/testing`. Record a real stream to a fixture, replay deterministically — no quota burned. |
| Kill-test A (engine can replace a tool result?) | ⬜ | Needs a live Claude engine; the `transformToolResult` capability flag is wired in the contract. |
| Kill-test B (detect rate-limit + clean resume on a real session) | ⬜ | Replayable once recorded; live recording pending. |
| Kill-test C (kill-tree catches Docker/WSL children, 3 OSes) | ⬜ | Process Manager work; OS-level, needs the daemon. |
| SQLite hybrid schema (append-log + state-tables) | 🟨 | DDL specified in plan 02 §ה; to be laid down as migrations. |
| CLI skeleton | ⬜ | |
| no-hardcoded-strings lint + pseudo-loc | ⬜ | i18n foundation. |

## Phase 1 — control core (Claude/SDK) + base safety — the MVP

| Component | State | Notes |
|---|---|---|
| Claude adapter (Agent SDK + Hooks) | 🟨 | Contract + fake in place; Node SDK runner next. |
| Process Manager (pgid/Job-Object, external_resources, reconcile-before-stop) | ⬜ | Rust/OS; scaffold in daemon. |
| Quota Manager (reactive, small units, checkpoint, first-turn read-only guard) | ⬜ | Logic prototypable in TS against the fake adapter. |
| Credential Broker (keyring + GitHub-App) + redaction | ⬜ | |
| Keep-awake (ES_CONTINUOUS…) | ⬜ | OS; scaffold. |
| Context-Optimizer core (File-Receipts, Output-Compressor, Context-Health=Estimated) | ⬜ | Runs in the Node engine (SDK transform / updatedToolOutput). |
| ntfy / Telegram operational push | ⬜ | TS-implementable + testable. |
| Timeline (bidi-isolation) + Scheduler + morning report | ⬜ | |

## Phases 2–4

⬜ Watchtower 3-layer + guard table · Isolation-Ring · VCS/CI loop · cost-ledger · Codex adapter · ACP generic · Relay Orchestrator · Cloud-delegated · full 23-language i18n · research track (parallelism/merge-queue).

## Verification snapshot

- `runner`: 11 tests passing, `tsc --noEmit` clean, 0 npm vulnerabilities.
