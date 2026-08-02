# Contributing to AgentRelay

Thanks for looking. AgentRelay is a small, security-minded project, and it is built to
be contributed to: the core is one Rust crate with a large, fast test suite, and CI
runs that suite on Linux, Windows, and macOS on every push.

New here? The issues labelled
[**good first issue**](https://github.com/MalyStern/agentrelay/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
are scoped to be a clean first change — each says which file to touch and which test to
mirror. [**help wanted**](https://github.com/MalyStern/agentrelay/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)
is the slightly larger stuff.

## Build and test

You need Rust (stable), and `node` + `git` on your PATH (a couple of tests spawn a real
`node` as a stand-in agent and shell out to git).

```bash
cd daemon && cargo test          # the core — should print "N passed; 0 failed"
```

Before you open a PR, run exactly what CI runs, so there are no surprises:

```bash
cd daemon && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

- `cargo fmt` (no `--check`) reformats in place.
- `clippy` runs with **warnings as errors** — the same bar CI holds. If clippy suggests
  a fix, take it or explain why not.

There is also a portable end-to-end harness that drives the real built binary through a
whole lifecycle in a throwaway sandbox:

```bash
pwsh scripts/verify.ps1     # Windows / macOS / Linux
bash scripts/verify.sh      # Linux / macOS
```

## What makes a change land easily

1. **A test comes with it.** This codebase leans on its tests; almost every file has a
   `#[cfg(test)] mod tests` right there at the bottom. Add to it. A bug fix should come
   with a test that fails before your change and passes after.
2. **Match the surrounding style.** The tests read like sentences
   (`a_broken_config_is_an_error_not_a_silent_fallback`) and the comments explain *why*,
   not *what*. Keep that.
3. **Fail closed.** This is a supervisor. When in doubt, the safe direction is to block
   and tell the user, never to silently allow. If your change touches a guard or the
   completion gate, say in the PR how it behaves on the ambiguous input.
4. **Be honest in docs.** If something is a heuristic, call it a heuristic. The project
   would rather under-claim than over-claim — see [`SECURITY.md`](SECURITY.md) for the
   tone.

## Where things live

```
daemon/        the Rust supervisor — the product, and where most changes go
  src/guard.rs     the deterministic guard evaluator (blocks forbidden actions)
  src/report.rs    the completion gate + morning report
  src/isolation.rs the Docker isolation model
app/           the Tauri desktop app
plugin/        the Claude Code plugin (hooks + commands + skill + MCP)
i18n/          the message catalogue + its validator (check.mjs)
scripts/       verify.ps1 / verify.sh
docs/          the design docs (Hebrew) + ADRs
```

## Reporting a security issue

Please do **not** open a public issue for a vulnerability. Follow
[`SECURITY.md`](SECURITY.md) — a private GitHub security advisory is preferred.

## Commit and PR

- Small, focused commits with a message that explains the *why*.
- Open the PR against `main`. CI must be green (all three OSes).
- It's fine to open a draft PR early and ask for direction.
