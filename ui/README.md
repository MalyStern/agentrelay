# AgentRelay — web UI

The screen you open at 8am to answer one question: **did the overnight run need me, or can I go back to my coffee?**

It is a single file, `index.html`, with no build step, no bundler, and no network requests except to a daemon you connect it to on `127.0.0.1`. No CDN, no web fonts, no telemetry — because this product's promise is that nothing leaves your machine, and a page that fetched a font from a CDN would break that promise on the very screen that advertises it.

## How to run it

**Just to look (demo mode):** double-click `index.html`, or open it from `file://`. It defaults to demo mode with realistic fixture data — no daemon required. Use the **scenario picker** in the header to walk through the states a real night can end in.

**Against a real daemon (live mode):**
1. In your project, run `agentrelay serve`. It writes the loopback address and a bearer token to `.agentrelay/ipc.json`.
2. Open `index.html`, click **Connect to a daemon…**, and paste the address (e.g. `127.0.0.1:52001`) and the token from that file.
3. The token is held in the page's memory only. It is never stored, never written to disk, and never rendered back into the page. Closing the tab forgets it; **Disconnect** drops it immediately.

The page checks `GET /health` (no auth) to confirm something is really there, then reads `GET /status` and `GET /report`. In live mode it renders the real morning report the daemon serves — the same layout you see in demo mode, with the daemon's own verdict driving the banner.

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

The daemon's HTTP surface (`daemon/src/ipc.rs`) serves `GET /report` — the full morning report as a `ReportEnvelope`: every provenance line, the unit tallies, the checkpoints, **and the daemon's own `headline` and `needs_attention`**. Those last two are decisions, not formatting, so the page takes the banner verdict from the daemon rather than recomputing one that could drift from it (`daemon/src/report.rs` explains why: one source of truth, or the screen eventually says "all good" on a night the exit code called a failure).

If you connect to an older daemon that answers `404` for `/report`, the page shows what `/status` gives (banner, state, spending, isolation), says plainly that this daemon did not serve the report, and points you at `agentrelay report` in your terminal. It never reconstructs units or checkpoints from status alone, because inventing evidence is the one thing this product exists to refuse.

The token is used for `/status` and `/report` alike, held in memory only, and is never stored, logged, or rendered back into the page.

## Accessibility

Keyboard reachable with a visible focus ring, real semantic HTML (`<header>`, `<main>`, `<section>`, `<dl>`, `role="status"`/`role="alert"` on the things that must be announced), works down to 320px wide, and respects `prefers-reduced-motion` and `prefers-color-scheme` (light and dark). Every provenance tag and every state is conveyed by text and shape, not colour, so it survives greyscale, colour blindness, and a screen reader.

## How this was verified (real rendering, not just code)

The browser preview pane in this environment could not render the page live (it renders `file://` outside the project as a static, no-JS snapshot, and blocks loopback by policy), so verification was done against **real pixels** from a headless Chrome driven over the DevTools Protocol:

- **Every demo scenario** (failed / green / unverified / nothing / blocked) screenshotted at **desktop light and dark**. Banner class per scenario was asserted from the live DOM: `attention` for failed/unverified, `calm` for green/blocked, `neutral` for nothing.
- **320px width**, light and dark: no horizontal overflow (`document.scrollWidth === window.innerWidth === 320`); the layout stacks and the command boxes scroll within themselves.
- **Greyscale** (`filter: grayscale(1)` applied to the whole page, then screenshotted): VERIFIED (double border, `✓✓`), RECORDED (solid border, `▣`) and ESTIMATED (dashed border, `≈`) stay distinguishable with colour removed, and the banner's `!` + "Yes — you're needed" carries the alarm without red.
- **Live `/report` end-to-end**: a mock daemon (shapes from `report.rs`/`daemon.rs`, with CORS so a `file://` page can read it) was connected through the real Connect dialog. The page rendered the report from the wire, took the banner headline from the daemon's `headline`, cleared the token field after connecting, and — against a mock that `404`s `/report` — fell back to the status-only view with the "did not serve the report" notice instead of fabricating one.
