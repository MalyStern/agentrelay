# AgentRelay — web UI

The screen you open at 8am to answer one question: **did the overnight run need me, or can I go back to my coffee?**

It is a single file, `index.html`, with no build step, no bundler, and no network requests except to a daemon you connect it to on `127.0.0.1`. No CDN, no web fonts, no telemetry — because this product's promise is that nothing leaves your machine, and a page that fetched a font from a CDN would break that promise on the very screen that advertises it.

## How to run it

**Just to look (demo mode):** double-click `index.html`, or open it from `file://`. It defaults to demo mode with realistic fixture data — no daemon required. Use the **scenario picker** in the header to walk through the states a real night can end in.

**Against a real daemon (live mode):**
1. In your project, run `agentrelay serve`. It writes the loopback address and a bearer token to `.agentrelay/ipc.json`.
2. Open `index.html`, click **Connect to a daemon…**, and paste the address (e.g. `127.0.0.1:52001`) and the token from that file.
3. The token is held in the page's memory only. It is never stored, never written to disk, and never rendered back into the page. Closing the tab forgets it; **Disconnect** drops it immediately.

The page checks `GET /health` (no auth) to confirm something is really there, then reads `GET /status`.

## What each part of the screen answers

| Part | The question it answers |
|---|---|
| **The banner at the very top** | *Do I need to do something right now?* Red "Yes — you're needed" only when a unit failed, went unverified, lost its restore point, or the mission is waiting on a person. Green "No action needed" otherwise — and it says *why* it is safe, including whether the run will continue on its own. It never nags when nothing is needed. |
| **How to read the evidence** | The provenance key: **VERIFIED** (a command ran and its exit code decided it), **RECORDED** (the supervisor saw it directly), **ESTIMATED** (a rule of thumb, never a measurement). Each is shown by icon, border shape, *and* the word — never by colour alone. |
| **Units overnight** | Done-and-checkpointed / failed / not-verified / no-restore-point. "Not verified" is kept separate from "done" on purpose: nobody proved it works, so it is not counted as if they had. |
| **Every line, and where it came from** | The full report, each line tagged with its provenance. Nothing the agent merely *claimed* appears here. |
| **State** | Where the mission is, in the daemon's own words. The "it will not continue by itself" case (waiting for you / paused) is loud and unmistakable. |
| **Spending** | What was actually charged (a recorded ledger figure) against the cap. When there is no cap, it says so plainly — a budget you believe is set but isn't is worse than none. |
| **Checkpoints** | Each restore point with the exact `git reset --keep <sha>~1` command to undo it. That command refuses rather than discards if it would lose uncommitted work, so it cannot delete anything — and `git show` is offered to read a checkpoint first. |
| **What can leave this machine** | Isolation, notifications, engine, keep-awake — in plain words. |

## An honesty note about live mode

The daemon's HTTP surface (`daemon/src/ipc.rs`) exposes `GET /status` only. The unit-by-unit morning report, its provenance lines, and the checkpoint list are produced by the command line — `agentrelay report` — and are **not** served over IPC.

So in live mode this page shows exactly what `/status` returns (the banner, state, spending, isolation) and says plainly that the full report is not on the wire. It does **not** reconstruct units or checkpoints from status alone, because inventing evidence is the one thing this product exists to refuse. To see the full report layout, use demo mode — or run `agentrelay report` in your terminal.

## Accessibility

Keyboard reachable with a visible focus ring, real semantic HTML (`<header>`, `<main>`, `<section>`, `<dl>`, `role="status"`/`role="alert"` on the things that must be announced), works down to 320px wide, and respects `prefers-reduced-motion` and `prefers-color-scheme` (light and dark). Every provenance tag and every state is conveyed by text and shape, not colour, so it survives greyscale, colour blindness, and a screen reader.
