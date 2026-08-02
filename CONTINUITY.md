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

CONTEXT OPTIMIZER (2026-08-02): context.rs — file receipts (sha256; hash = truth,
never a summary standing in for a file) + output compression (keeps failure lines,
always states what it cut) + ContextHealthEstimated (is_estimate serialised, per
plan §7.1 "Estimated only"). Receipts persist in a file_receipts table and are
reloaded by Daemon::receipt_book, so a resume after a quota reset does not re-read
the repo. Compression wired into the validator path. Two real bugs the tests
forced out: (1) plain 4KB truncation kept the banner and dropped the failure;
(2) run_shell on Windows escaped the shell string as argv, so any validator with
quotes (`npm test -- --grep "x y"`) broke — now uses raw_arg. 93 cargo tests.

MORNING REPORT (2026-08-02): report.rs — every line carries its provenance
(VERIFIED = a validator's exit code / RECORDED = the supervisor observed it /
ESTIMATED = a rule of thumb). Nothing the model claimed appears at all. `agentrelay
report` (+ --short for a phone), exits non-zero when a unit failed so a script can
decide whether to wake someone. saved_bytes is a real counter incremented at each
skip, not inferred from the receipts (which would overstate it every time). Two
real bugs caught by running it end to end: (1) complete_unit never wrote a
work_unit row, so the report saw no units and reported a night with a FAILED unit
as "all validators passed" — wrong in the reassuring direction; (2) git() trimmed
whole stdout, which eats the leading column of porcelain's FIRST line, so the first
modified file's path lost a character (".agentrelay/x" -> "agentrelay/x").
100 cargo tests.

PUSH NOTIFICATIONS (2026-08-02): notify.rs — ntfy (needs no account, self-hostable).
THE ONLY outbound network path in the daemon, NotifyConfig::None by default, and
`agentrelay status` prints describe_privacy() saying in plain words exactly what
would leave the machine (headline only — never file contents, diffs, prompts or
paths; the API takes a headline, not a report, so there is no accidental path).
Warns that a public ntfy.sh topic is readable by anyone who guesses it. Tested
against a REAL local TcpListener that captures the actual HTTP request — do
smoke tests that way too, not against ntfy.sh. 109 cargo tests.

NEXT: Telegram (two-way approve/abort) needs the credential broker for the bot
token — never in config.json. Then Phase-2: Mission-Contract state machine,
Isolation-Ring, cost ledger + budget kill-switch.

ADVERSARIAL QA + FIXES (2026-08-02): a read-only audit agent found 13 issues; the
worst were real and reproduced. Fixed in this pass:
  1. checkpoint used `git add --all`, sweeping the USER's uncommitted work into the
     agent's commit — and the advertised undo then deleted it. Now: begin_unit
     records a baseline, create_paths commits ONLY the agent's paths, dirty-tree
     with no baseline is REFUSED, and undo/rewind use `git reset --keep` (aborts
     rather than discarding uncommitted work) instead of `--hard`.
  7/8. report counted only units WITH validations, so a night with the DEFAULT
     config ("validators": []) reported "all validators passed", exit 0. Now
     units_unverified + units_uncheckpointed, needs_attention() drives the exit code.
  9. THE BIG ONE: claude_stream emitted Error{quota.rate_limited} but every quota
     consumer matches AgentEvent::RateLimited, which nothing outside tests built.
     Resume after a quota reset was dead code. Now emits RateLimited + parses reset_at.
  2/3/4. ReadOnly guard bypassed by "
", $(), backticks, `env`, and git
     branch/stash/config; unparsed write/shell tools produced 0 actions = ALLOW.
     Now ProposedAction::Unknown is refused by every guard (fail closed).
  5/6. forbid_path was case-sensitive (.ENV == .env on Windows/macOS); `C:file`
     (drive-relative) counted as relative and auto-approved.
  10. send_gate opened when condition_result was None (evaluated == not evaluated).
  11. truncate() sliced by byte index -> panic on multi-byte output (tsc/vitest/Hebrew).
119 cargo tests.

AUDIT ROUND 2 (2026-08-02): Unix ProcessGroup now has a Drop that kills the tree
(Windows had one via KILL_ON_JOB_CLOSE; Unix leaked `claude` and everything it
spawned on every interrupted turn). kill_tree no longer claims success when the
group was never populated (pgid==0), and treats ESRCH — already dead — as the
success it is. A failed Job assignment on Windows now kills the already-running
child instead of dropping it unreachable. IPC: gen_token() returns Result and the
server refuses to listen without real entropy (it used to fall back to 32 zero
bytes guarding POST /run); auth is checked BEFORE the body is read and the body is
capped at 1 MB (413). report scopes checkpoints to the mission's own units — git
history held every checkpoint ever made, so last week's showed up this morning and
`nothing_happened` could never be true again. 123 cargo tests.

AUDIT ROUND 3 (2026-08-02): guard violations are now appended to the event log as
`guard.violation` and surfaced in the morning report ("BLOCKED by no_push: the
agent tried … The turn was stopped") — they were enforced but never recorded, so
the morning showed an unexplained halt. dirty_paths now uses `-z` +
`core.quotePath=false`: default porcelain octal-escapes non-ASCII paths and writes
a rename as `old -> new` on one line, both of which reached the report verbatim.
125 cargo tests.

GUARDS NOW BLOCK (2026-08-02): hook.rs + `agentrelay hook` / `agentrelay
install-hook`. The agent runs the hook and WAITS for the answer before the tool
executes, so a forbidden call never starts — instead of the daemon spotting it on
an async NDJSON pipe and racing to kill the tree. Same evaluate_guards as the
daemon, so the two paths cannot drift. Denies on: unreadable input, empty tool
name, unparsable arguments, and a config it cannot load (never falls back to "no
guards"). Emits BOTH documented signals (exit 2 + the permissionDecision JSON).
install-hook merges into .claude/settings.json and leaves the user's other hooks
alone; installing twice is a no-op; malformed settings are refused, not rewritten.
Verified end-to-end through the real binary: `git push` -> exit 2 + deny JSON, an
in-workspace Write -> exit 0. 135 cargo tests.

STILL OPEN: the exact hook wire contract (field names / how the denial is read)
is a Phase-0 kill-test needing ONE run against a real Claude — both spellings and
both signals are emitted so a mismatch degrades rather than fails open. Also
keepawake is per-thread but held across awaits (needs PowerCreateRequest).

LIVE CONTRACT TEST (2026-08-02) - user installed claude 2.1.220, so the Phase-0
contract test finally ran against the real thing. Two findings:

1. WINDOWS BLOCKER: npm installs claude.cmd/claude.ps1 and NO claude.exe. Rust's
   Command::new only appends .exe, so the daemon could never spawn Claude on
   Windows and reported it as "not installed". New which.rs resolves via PATHEXT
   and routes .cmd/.bat through cmd.exe, .ps1 through powershell. doctor now
   prints the resolved path. Verified it finds ...
pm\claude.cmd.

2. THE PLAN'S CORE QUOTA ASSUMPTION WAS WRONG. The plan says Claude headless has
   no live counter so quota handling must be reactive. It does have one: every
   turn emits {"type":"rate_limit_event","rate_limit_info":{"status":"allowed",
   "resetsAt":<unix>,"rateLimitType":"five_hour"}} - where the wall is, BEFORE
   hitting it. Added AgentEvent::QuotaHint (deliberately distinct from RateLimited
   so a healthy heartbeat cannot abort a turn), parsed in claude_stream, persisted
   via store.save_quota_snapshot/latest_quota_reset. Also confirmed: the usage
   field names map_usage reads are correct, and assistant messages carry
   `thinking` blocks that must not become reply text. 145 cargo tests.

NEXT with claude installed: verify the PreToolUse hook actually blocks (run claude
with install-hook active and a prompt that would trip a guard), then use
latest_quota_reset to schedule units rather than walk into the wall.

KILL-TEST PASSED (2026-08-02): the PreToolUse hook BLOCKS a real Claude tool call.
Live run, claude 2.1.220, forbid_path guard on .env, --allowedTools Write:
  - Claude issued Write for .env
  - tool_result came back is_error:true with our text, and
    tool_result_meta.non_execution_kind == "permission-rule" (it never ran)
  - the file does not exist
  - Claude complied and did not attempt a workaround (the 'do not work around it'
    wording in the refusal is doing real work)
  - the result event carries permission_denials:[{tool_name,tool_use_id,tool_input}]
Watchtower layer-1 is now genuinely deterministic, not advisory. Also parsed
permission_denials -> AgentEvent::ToolDenied, stored as kind `tool.denied` and
reported as VERIFIED (the agent confirming a call never ran is stronger evidence
than our own note that we refused it). 147 cargo tests.

NEXT: use latest_quota_reset to schedule units around the wall instead of walking
into it; keepawake per-thread fix; Phase-2 (Mission-Contract state machine,
Isolation-Ring, cost ledger + budget kill-switch).

SCHEDULING AROUND THE WALL (2026-08-02): now that the provider reports resetsAt on
healthy turns, two things became possible that the plan said were not.
  1. A rate-limit message often omits the reset time; the planner used to fall
     back to a fixed guess. It now prefers the time we were told on an earlier
     healthy turn (store.latest_quota_reset), so a resume lands when the window
     actually reopens instead of hours early or late.
  2. quota::decide_start + Daemon::should_start_unit + `agentrelay fits --minutes N`
     answer "does this unit fit in what's left of the window?" - exit 0 go, exit 75
     wait, so a night script can gate on it. Unknown window = start (refusing to
     work for lack of information is a worse failure than being interrupted).
152 cargo tests.

PHASE-2 STARTED: COST LEDGER + BUDGET KILL-SWITCH (2026-08-02). cost.rs. The live
capture gave the real field name: the result event carries total_cost_usd, so the
ledger records what was CHARGED rather than an estimate from a price table that
goes stale. Budget{mission_usd,turn_usd} in config; unset = no limit and status
says so out loud. The cap is checked BEFORE a turn (run_turn refuses), using the
most expensive turn so far as the honest guess at the next one - checking after
only tells you how far past the line you are, and money is the one resource here
git cannot restore. A missing total_cost_usd is None, never 0.0 (0.0 would read as
'free' and disarm the cap). Spend is a running sum in the store so it survives a
restart. Also fixed the audit's keepawake finding: SetThreadExecutionState is
per-thread and the guard was dropped on whichever tokio worker happened to run it,
so the release did nothing while the original request stayed alive (a machine that
never sleeps again). The request now lives on a dedicated thread and Drop joins it.
164 cargo tests.

NEXT (Phase-2): Mission-Contract state machine, Isolation-Ring (container +
egress allowlist), CI loop. Remaining audit item: none open that reproduce.

MISSION STATE MACHINE (2026-08-02): state.rs + Daemon::transition/mission_state,
persisted in state_transitions so a process that was not there when the run
started can still tell where it is. Enforced properties: terminal is terminal (a
crash-restart cannot resume a mission the human stopped - the failure that makes
people distrust automation); `stopped` is only reachable through `stopping`, so
the record cannot claim processes were killed when nothing was; an illegal move is
REFUSED, never recorded (a plausible-but-wrong state is worse than a crash);
re-declaring the current state is a no-op, not an error. needs_a_person() is
deliberately narrower than !resumes_itself(): `stopping` moves on its own, and
telling the user to act when the answer is 'wait' trains them to ignore the
message. state_for_foreign_head implements the plan's rule - a HEAD commit that is
not ours means someone else worked here, so stop and ask instead of building on
changes the agent has never seen. Surfaced in `status`. 178 cargo tests.

NEXT (Phase-2): Isolation-Ring (container + egress allowlist) - needs Docker,
check availability first; VCS/CI loop.

## Done log (most recent first)

- 2026-07-28: Rust daemon — events/provider/error/adapter/guard/quota/send_gate/store/claude_stream/fake_adapter/claude_adapter ALL built+tested (44 cargo tests). Real subprocess works; MissionRunner supervises end-to-end (guards+quota+store). Store=bundled rusqlite; async=tokio.
- 2026-07-27: runner (TS) Phase 0 + core Phase 1 + full QA pass, 71 tests. See STATUS.md.
