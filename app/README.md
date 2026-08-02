# AgentRelay — desktop app

A native window for AgentRelay, so you don't have to live in a terminal to run an
overnight agent and read the morning report.

It exists to remove the two things that make the CLI unfriendly to a non-terminal
user:

- **Starting the daemon.** One button spawns `agentrelay serve` for the folder you
  pick, and waits until it's actually listening.
- **The token.** The daemon protects its loopback API with a bearer token you'd
  otherwise paste by hand. Here the Rust backend reads it from `.agentrelay/ipc.json`
  and attaches it to every request itself. **The token never reaches the web page** —
  it is not displayed, stored in the DOM, or logged. That's the same rule the browser
  UI has to *ask* you to uphold; the desktop app just enforces it.

## What it shows

The same priorities as the rest of the product:

1. A banner that answers **"do I need to do something right now?"** before anything
   else — and its verdict comes straight from the daemon's own `needs_attention`, so
   the screen can never disagree with the exit code about whether a night went wrong.
2. The morning report, every line tagged by where it came from — **VERIFIED /
   RECORDED / ESTIMATED** — never by colour alone (a glyph, a border shape, the word,
   and an aria-label all carry it).
3. Status: state (loud when it will not continue by itself), spending vs cap,
   isolation, notifications.

## Architecture

The app talks to the daemon over its loopback HTTP API (`/status`, `/report`) — the
exact surface a browser UI uses. It deliberately **does not** compile against the
daemon crate, so the two can be built, shipped, and worked on independently. The
webview makes no network calls of its own; every request goes through a named Rust
command, and the CSP locks the page to its own origin. Nothing can phone home from
the screen that promises it won't.

## Run it

Prerequisites: Rust, the Tauri CLI (`cargo install tauri-cli`), and on Windows the
WebView2 runtime (already present on any recent Windows). Build the daemon once so
the app can find and launch it:

```bash
cd ../daemon && cargo build
```

Then, from `app/src-tauri`:

```bash
cargo tauri dev      # develop with hot reload
cargo tauri build    # produce a real installer / .exe
```

In the window: **Choose workspace** (a folder where you ran `agentrelay init`) →
**Start daemon** → the report and status appear. Closing the window stops the daemon
the app started — it never leaves a background process running that you can't see.

## Files

- `frontend/index.html` — the whole UI, self-contained, no build step, no CDN, no web
  font. Local-first to the letter.
- `src-tauri/src/lib.rs` — the backend: find the binary, start the daemon, hold the
  token, proxy the HTTP calls.
- `src-tauri/tauri.conf.json` — window, CSP, bundle.
- `src-tauri/capabilities/default.json` — the minimal set of permissions the window
  is granted (the folder dialog and the app's own commands; no filesystem/shell/http
  is exposed to the webview).
