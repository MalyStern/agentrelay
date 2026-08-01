# CONTINUITY — autonomous-work resume protocol

> This file is the single source of truth for picking up the AgentRelay build after a
> context reset. The user is away (~1 week) and wants continuous autonomous progress.
> **Read this first, then `STATUS.md`, then continue from "NEXT STEP".**

## The protocol (how to not lose work / not get stuck on token limits)

Work in **small, verifiable increments**. Each increment:
1. Build ONE thing (a module / a fix).
2. **Verify it** — `cargo test` (daemon) or `npm test` (runner). Never commit red.
3. **Commit** with a clear message (git identity below).
4. **Update this file's "NEXT STEP" + the STATUS/memory** so the next session resumes cleanly.
5. Repeat. Keep each response focused so a context compaction never straddles unverified work.

Git identity (the repo has no global user set):
```
git -c user.email=5829404@gmail.com -c user.name=MalyStern commit ...
```
Repo: private `MalyStern/agentrelay`. Push after each commit.

## Environment (verified 2026-07-28)

- **Rust works here.** Prepend the cargo bin to PATH in every shell:
  - PowerShell tool: `$env:Path += ";$env:USERPROFILE\.cargo\bin"` then `cargo test` in `daemon/`.
  - `~/.cargo/config.toml` already sets `check-revoke=false` + `git-fetch-with-cli=true` (the network blocks OCSP/CRL; without it cargo can't fetch crates.io).
- Node 24 + `node:sqlite` work for the `runner/` package (`npm test`).
- Both `daemon/` (Rust) and `runner/` (TS) build & test in this environment.

## Build / test commands

```
# Rust daemon
$env:Path += ";$env:USERPROFILE\.cargo\bin"   # PowerShell only
cd daemon ; cargo test

# TS runner
cd runner ; npm test ; npm run typecheck
```

## Where things are

- `daemon/` — Rust, the real product. Done+tested: events, provider, error, **guard** (8 tests).
- `runner/` — TS, the executable spec + Node-native adapters + test harness (71 tests). Keep in lockstep with the Rust port.
- `docs/02-*.md` — the authoritative plan. `docs/adr/0001-*` — the language split.

## NEXT STEP

Port the remaining pure-logic modules from the (QA-hardened) TS to Rust, one at a time, each with tests:
1. ~~`daemon/src/quota.rs`~~ ✅ DONE (7 tests).
2. ~~`daemon/src/send_gate.rs`~~ ✅ DONE (8 tests).
3. ~~`daemon/src/store.rs`~~ ✅ DONE (6 tests, bundled rusqlite).
4. ~~Claude stream-json parser~~ ✅ DONE as `daemon/src/claude_stream.rs` (8 tests).
5. ~~Async ProviderAdapter trait + fake adapter~~ ✅ DONE (adapter.rs + fake_adapter.rs, 4 async tests; tokio/futures/async-stream).
6. ~~Real Claude subprocess adapter~~ ✅ DONE (claude_adapter.rs: spawn_stream_json + ClaudeAdapter; 3 tests incl. a real node-subprocess integration test).
7. ~~MissionRunner (mission.rs)~~ ✅ DONE (3 tests: clean turn logs+accumulates, guard violation interrupts, rate-limit -> resume plan). Ties adapter+guard+quota+store.
8. ~~Process Manager (process.rs)~~ ✅ DONE — ProcessGroup: Windows Job Object (KILL_ON_JOB_CLOSE) / Unix process-group; kill_tree kills the whole tree, verified with a grandchild integration test. 49 cargo tests.
9. ~~Wire ProcessGroup into the adapter~~ ✅ DONE — ClaudeAdapter.interrupt() kills the tree; verified killing the group ends the stream. 50 cargo tests.
10. ~~Keep-awake~~ ✅ DONE (keepawake.rs, 51 tests; ES_CONTINUOUS+ES_SYSTEM_REQUIRED).
11. ~~Credential Broker~~ ✅ DONE (credential.rs, 54 tests) — SecretStore trait (InMemory for tests / OsKeychain via keyring for prod), CredRef (DB-safe), env_for injection, redact().
12. ~~Daemon binary + CLI~~ ✅ DONE — daemon.rs (Daemon: config+store+keep-awake+MissionRunner) + main.rs (agentrelay CLI: init/status/run/doctor). 58 tests; CLI smoke-tested (init/status/doctor work). `run` works the moment `claude` is installed. IPC surface (loopback+token) still TODO for the UI.
13. **NEXT options** (pick per value): (a) IPC surface (loopback HTTP+token) so a UI/remote can drive it; (b) Codex adapter (app-server JSON-RPC) — 2nd provider; (c) Watchtower layer-2 validators (run tests/typecheck as completion evidence); (d) improve the shipped CLIs (patchproof/repodoctor/lastgood). ~~(c) validators~~ ✅ DONE (validator.rs, 61 tests) — run real commands, exit code = verdict, all_passed gate (empty set = not proven). ~~(a) IPC surface~~ ✅ DONE (ipc.rs, 64 tests) — loopback HTTP+token, GET /health /status, POST /run; `agentrelay serve` writes .agentrelay/ipc.json. Store made Sync (Mutex<Connection>). Next recommend (b) Codex adapter (needs Codex for contract-test) or git-checkpoint. A background agent is improving the shipped CLIs.
14. Later: Isolation-Ring (docker/bollard), VCS/CI loop, ACP generic, Tauri UI, 23-lang i18n, Cloud-delegated.
5. In parallel / when Rust is blocked: improve the shipped CLIs (patchproof/repodoctor/lastgood) and other products per the viral-oss memory.

Update this "NEXT STEP" after each increment.

COMPLETION GATE WIRED (2026-08-02): validators run in the daemon (Daemon.run_validators), persist to the validations table, gate completion (all_passed); CLI `agentrelay validate`; Config.validators. Fixed: ported SCHEMA was missing the validations+quota_snapshots tables. 65 cargo tests.

CONFIG TRANSPARENCY SWEEP (2026-08-02): the same fail-silent config bug existed in
all four products — a broken/typo'd config was caught and ignored, so the user's
settings were never applied and nothing said so. Fixed everywhere: loud errors
naming the file and the valid values, UTF-8 BOM accepted (Windows Notepad/
PowerShell), JSONC comments in the CLIs, `init` commands that write a commented
config listing every option. patchproof 32->42, repodoctor 28->38, lastgood
23->33, daemon 65->70 tests. All pushed.

CHECKPOINTS WIRED (2026-08-02): checkpoint.rs — real git commits made by the
supervisor, authored as AgentRelay <agentrelay@localhost> with an
`AgentRelay-Checkpoint:` trailer so resume can tell our commits from the agent's
and the human's. Daemon::complete_unit joins the two halves: validators run, and
the tree is committed ONLY if they all pass (no validators = not a pass). CLI:
`agentrelay done "<summary>"` and `agentrelay checkpoints`. Never resets/cleans/
pushes — undo is handed back as a command. Caught in the process: `git add --all`
would have swept our own DB into the user's commits, so init now writes a
self-ignoring .agentrelay/.gitignore that keeps only config.json. 80 cargo tests.

NEXT: Context-Optimizer core (file receipts + output compression) — the piece that
actually saves tokens; then the morning report (reads checkpoints + validations).

## Done log (most recent first)

- 2026-07-28: Rust daemon — events/provider/error/adapter/guard/quota/send_gate/store/claude_stream/fake_adapter/claude_adapter ALL built+tested (44 cargo tests). Real subprocess works; MissionRunner supervises end-to-end (guards+quota+store). Store=bundled rusqlite; async=tokio.
- 2026-07-27: runner (TS) Phase 0 + core Phase 1 + full QA pass, 71 tests. See STATUS.md.
