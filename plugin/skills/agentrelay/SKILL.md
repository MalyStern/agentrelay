---
name: agentrelay
description: Use when supervising an overnight or unattended coding run with AgentRelay — setting up guards/validators/budget, finishing a unit safely (validate → checkpoint), reading the morning report, or when the user mentions agentrelay, supervised runs, guards, checkpoints, or "don't touch these files". Teaches the correct order of operations so work is gated on evidence and the user's own uncommitted work is never lost.
---

# Supervising a run with AgentRelay

AgentRelay is a local supervisor for coding-agent runs. Its whole value is that a
claim of "done" is backed by evidence, a forbidden action is blocked before it runs,
and every passing unit is a git checkpoint you can undo. As the agent, your job is to
use it in the right ORDER — the order is the safety.

Everything is the `agentrelay` CLI (also exposed as `/agentrelay:*` slash commands and,
if installed, as MCP tools). If the binary is not on PATH, say so and stop — do not
pretend the guards are active.

## The one rule that prevents lost work

**Always run `agentrelay begin` BEFORE you start changing files for a unit**, and
`agentrelay done` when the unit is finished. `begin` records what the user already had
uncommitted, so the checkpoint commits ONLY your work and never sweeps up their edits.
Skipping `begin` means a dirty tree with no baseline, and AgentRelay will (correctly)
refuse to checkpoint rather than guess whose work is whose.

## The workflow, in order

1. **Set up once per project** (if `.agentrelay/config.json` does not exist):
   ```bash
   agentrelay init
   # or start from a ready-made config:
   agentrelay templates
   agentrelay init --template fix-failing-tests
   ```
   Then make sure `.agentrelay/config.json` has the RIGHT validators — the actual
   commands that prove this project works (e.g. `npm test`, `cargo test`, a `qa.mjs`).
   A template ships example commands; replace them with the project's real ones. Do
   not leave the examples and claim they validate anything.

2. **Confirm what is armed** before working:
   ```bash
   agentrelay status
   ```
   It says, in plain words: the guards, whether the blocking hook is installed
   (`Guards BLOCK` vs `ADVISORY only`), the spending cap, isolation, and — loudly — if
   the mission is waiting on the user.

3. **Mark the start of a unit**, then do the work:
   ```bash
   agentrelay begin --unit <short-id>
   # ... make the changes for this unit ...
   ```

4. **Finish the unit — validate, then checkpoint only if it passes:**
   ```bash
   agentrelay done "what this unit did" --unit <short-id>
   ```
   If the validators fail, nothing is committed and the unit is not done. Either fix
   it yourself and run `done` again, or hand it to the bounded self-repair loop:
   ```bash
   agentrelay fix --unit <short-id>
   ```
   `fix` stops after a few attempts, or the moment two attempts fail the same way —
   it will not burn a night repeating itself. When it gives up, the mission goes to
   `waiting_input` and a person is needed.

5. **Check the window before a long unit** (if a quota applies):
   ```bash
   agentrelay fits --minutes 30
   ```
   Exit 0 = go; exit 75 = wait, because being cut off mid-unit leaves half-done,
   unvalidated work.

6. **The morning report** — what actually happened, each line sourced:
   ```bash
   agentrelay report          # full, with VERIFIED / RECORDED / ESTIMATED labels
   agentrelay report --short  # one headline
   ```
   Trust `VERIFIED` (a command's exit code), treat `ESTIMATED` as a guess, and never
   present anything the model merely claimed as fact.

7. **Undo a unit** if needed — never guess the command:
   ```bash
   agentrelay checkpoints          # list them, newest first
   # AgentRelay prints the exact undo command when it checkpoints; it uses
   # `git reset --keep`, which refuses rather than discarding uncommitted work.
   ```

## What NOT to do

- **Do not** edit the tests, disable a validator, or widen the unit's scope to make a
  build go green. A green build that means nothing is worse than an honest failure —
  if a unit cannot pass without cheating, stop and say so.
- **Do not** work around a guard that blocked you. A block is the user's configured
  intent; choose a different approach or report it.
- **Do not** call a unit done on your own say-so. Only `agentrelay done` / `validate`,
  with passing validators, makes it done.
- **Do not** claim guards are active if `agentrelay status` says `ADVISORY only` or the
  binary is missing. Say plainly that enforcement is not on.

## Cloud delegation is different

`agentrelay` can delegate a task to a hosted agent (`cloud` config). That run is
**fire-and-review — NOT supervised**: the guards, checkpoints, and budget do not apply
to it. Never describe a cloud run as guarded.
