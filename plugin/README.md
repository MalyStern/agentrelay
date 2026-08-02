# AgentRelay — Claude Code plugin

Installing this plugin **installs the guards**. Instead of editing `.claude/settings.json`
by hand (what `agentrelay install-hook` does), the plugin registers AgentRelay's
PreToolUse hook for you, so every tool call Claude Code makes is evaluated against the
guards in your project's `.agentrelay/config.json` — and a forbidden one is **blocked
before it runs**, not noticed after.

This is the same `agentrelay hook` that was verified end-to-end against a live Claude:
asked to write a forbidden `.env`, Claude's Write call came back refused, the file was
never created, and Claude did not try to work around it.

## What it adds

- **A PreToolUse hook** (`hooks/hooks.json`) — the real enforcement. Empty matcher, so
  it sees every tool; AgentRelay decides what is relevant.
- **Slash commands** wrapping the CLI: `/agentrelay:status`, `/agentrelay:init`,
  `/agentrelay:report`, `/agentrelay:validate`, `/agentrelay:done`,
  `/agentrelay:checkpoints`, `/agentrelay:fits`.

`defaultEnabled` is `false` on purpose: guards are a decision (cost and responsibility),
so you opt in.

## Install

**1. Install the `agentrelay` binary** (the plugin calls it; it is not bundled — a Rust
binary is per-OS/arch, and a plugin cannot carry every platform's build):

```bash
# from the repo, for now:
cd daemon && cargo build --release
# then put target/release/agentrelay(.exe) on your PATH
```

**2. Add the marketplace and install the plugin:**

```bash
/plugin marketplace add MalyStern/agentrelay
/plugin install agentrelay@malystern
```

**3. Set up a project:**

```
/agentrelay:init
```

Edit `.agentrelay/config.json` — the guards, the validators (your real `npm test` etc.),
a spending cap, the scope — then work as normal. The hook does the rest.

## If the engine isn't installed, the plugin does no harm

The hooks run through a small Node wrapper (`bin/guard.mjs`), so the plugin is
**fail-safe** when the `agentrelay` binary is missing: the guard **allows every tool
call quietly** (it does not error on each call, and it does not block your work), and
a **SessionStart notice fires once** to say supervision is off and how to arm it. So
installing the plugin before the binary is a no-op with one clear warning — not the
per-call noise an earlier version would have produced.

This means the plugin's hooks need **Node** on PATH (which Claude Code coding users
almost always have). If you don't use Node, install the binary and run
`agentrelay install-hook` instead — that writes a direct hook with no Node in the path.

## What a plugin can and cannot do here — honestly

- **The guard works fully as a plugin.** The hook is stateless (one process per tool
  call) and needs no running daemon. This is the core value and it is complete.
- **Reports, checkpoints, cost tracking, the fix loop** are the slash commands, and they
  talk to the local `agentrelay` CLI / a running `agentrelay serve`. They work, but they
  are convenience wrappers around the standalone daemon, not something the plugin itself
  hosts.
- **The binary is not bundled.** You install it once (above). A `SessionStart` hook could
  auto-download a per-OS build from releases, but that is left out until there are signed,
  per-platform release binaries to point at — shipping an unsigned auto-downloader would
  be the wrong trade for a security tool.

## Verified

The hook contract in `daemon/src/hook.rs` was checked field-by-field against Claude Code's
documented PreToolUse schema — input (`tool_name`/`toolName` both accepted), output
(`hookSpecificOutput.permissionDecision` = `deny`/`allow`, `permissionDecisionReason`),
and exit codes (0 allow, 2 deny) — and matches. The live block above is the end-to-end
proof.
