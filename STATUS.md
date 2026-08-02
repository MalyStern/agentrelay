# AgentRelay — build status

> **This file used to describe an early stage** — a Node `runner` as an executable
> spec, with the Rust daemon "scaffolded, pending the Rust machine," 71 tests. That
> is no longer where the project is, and leaving the old text here was misleading.
> The daemon is built and tested. This is the current, honest picture.

_Last updated 2026-08-02._

## What actually runs today

| Component | State | Where |
|---|---|---|
| **Daemon** (Rust) — the product | ✅ built + tested, **284 tests** | [`daemon/`](daemon/) |
| Blocking PreToolUse guard (denies forbidden calls *before* they run) | ✅ | `daemon/src/hook.rs`, `policy` |
| "Done" gated on real test evidence (no green without proof) | ✅ | `daemon/src/report.rs`, `templates.rs` |
| Checkpoints to git — only validated work, never your uncommitted files | ✅ | `daemon/src/report.rs` |
| Spend cap + provenance-labelled morning report (VERIFIED/RECORDED/ESTIMATED) | ✅ | `daemon/src/report.rs` |
| Local HTTP IPC (status/report over loopback) | ✅ | `daemon/src/ipc.rs` |
| Context optimizer (file receipts = hash truth; output compression, states what it cut) | ✅ | `daemon/src/context.rs` |
| **Desktop app** (Tauri v2) — talks to the daemon over loopback | ✅ builds on all 3 OSes | [`app/`](app/) |
| **Claude Code plugin** — hooks + slash commands + skill + MCP, fail-safe when the engine is absent | ✅ | [`plugin/`](plugin/) |
| **i18n** message catalogue (keys, interpolation, bidi, plurals validated) | ✅ | [`i18n/`](i18n/) |
| `runner/` — the original Node executable spec | ✅ kept as a reference spec | [`runner/`](runner/) |

Everything above is exercised by CI on **Linux, Windows, and macOS** on every push —
see [`.github/workflows/ci.yml`](.github/workflows/ci.yml). The daemon job runs
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` (warnings are errors),
and the full test suite.

## Honest limits (what this is *not*)

- The guards **block known-forbidden actions**; they are not a sandbox. Real
  enforcement is guards **plus** OS-level isolation with the network off. An unknown
  tool name a guard has never heard of produces no proposed actions and is allowed —
  so the strong claim is "blocks the actions it classifies," not "stops anything."
- The Windows release binaries are **unsigned**. SmartScreen will warn. Checksums are
  published with each release; code-signing needs a certificate and is out of scope
  for now.

## History

The phase-by-phase build log and the adversarial QA passes live in
[`CONTINUITY.md`](CONTINUITY.md). The design docs are under [`docs/`](docs/).
