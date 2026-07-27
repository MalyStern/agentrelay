# ADR 0001 — Build environment constraint & the language split

**Status:** accepted · **Date:** 2026-07-27

## Context

The final plan ([`02`](../02-תוכנית-סופית.md)) specifies a **Rust + tokio** daemon as the single source of truth, with a Tauri 2 / React UI and Node subprocesses for the provider engines (Claude Agent SDK, Codex App-Server, ACP).

The build environment used for the initial autonomous work **cannot install the Rust toolchain**: HTTPS downloads to non-allowlisted hosts fail TLS certificate validation (rustup, winget's package CDN, and direct `curl` all fail — the same interception that returned `curl exit 35` earlier). `npm` works because it goes through a configured registry proxy.

The plan's own guiding discipline is: *don't write what you can't verify* (Phase-0 kill-tests, "hard to inject later", testing from day 1). Writing hundreds of lines of Rust that can't be compiled or tested here would violate exactly that principle.

## Decision

Split the work by what is **verifiable in this environment**:

1. **Build now, in TypeScript/Node, fully tested** — the parts the plan already places in Node, plus everything cross-language that the Rust daemon will mirror:
   - The **normalized contracts** (`AgentEvent`, `ProviderAdapter`, `QuotaState`, error taxonomy, event kinds). These are a language-neutral spec; the Rust `trait`/`enum` mirror the same shapes.
   - The **provider adapters** that are Node-native by design: Claude **Agent SDK** runner, Codex **App-Server** JSON-RPC client, **ACP** client. The plan (§ו) explicitly makes these Node subprocesses.
   - The **Phase-0 testing foundation**: fake `ProviderAdapter` + **record/replay** harness (so `resume`/`kill`/rate-limit paths are testable without burning provider quota — plan §יג).
   - The **i18n foundation**, the **SQLite schema** (hybrid append-log + state-tables per doc 02 §ה), and the **CLI skeleton**.

2. **Scaffold for the Rust machine** — the `daemon/` Cargo workspace (module layout, core types/traits mirroring the TS contracts, the SQLite schema, the error taxonomy). Kept to structure + declarations, **not** deep unverifiable logic, so it compiles cleanly when `cargo` is available and the tested TS contracts guide the port.

The TypeScript contracts + fake adapter thus act as an **executable specification** for the Rust daemon: the same event stream, verified by tests, that the Rust `ProviderAdapter` implementations must reproduce.

## Consequences

- Real, tested progress is possible now (the whole agent-runner + contract + test surface), without pretending to have compiled a Rust daemon.
- When the project moves to a Rust-capable machine, the daemon is ported against a green, executable contract suite rather than from prose.
- `STATUS.md` tracks, per component, what is **built + tested** vs **scaffolded (pending Rust)**.
- If Rust becomes installable here later, the daemon skeleton is already laid out to fill in.
