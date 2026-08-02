# Security Policy

AgentRelay is a supervisor you leave running while an AI coding agent works
unattended. Its whole job is to be the thing standing between that agent and your
machine, so its own security posture matters more than most tools'. This document is
honest about what it defends, what it does not, and how to report a hole.

## Supported versions

AgentRelay is pre-1.0. Only the latest release line gets security fixes.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | ✅ latest — security fixes land here |
| < 0.1   | ❌ pre-release, unsupported |

Until 1.0 the recommendation is to run the latest `main`/release and expect breaking
changes between minor versions.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Report it privately:

- Preferred: open a **GitHub private security advisory** —
  <https://github.com/MalyStern/agentrelay/security/advisories/new>. This keeps the
  report private until a fix is out and lets us collaborate on it in one place.
- Or email the maintainer at the address on the GitHub profile
  (<https://github.com/MalyStern>), with `AgentRelay security` in the subject.

What to expect:

- An acknowledgement within **7 days**.
- An initial assessment (accepted / needs-info / declined, with reasoning) within
  **14 days**.
- If accepted, a fix and a released patch as fast as the fix allows, and credit in the
  advisory unless you ask to stay anonymous.

Because this is a small volunteer project, those are best-effort timelines, not an
SLA. Please give us a reasonable window to fix before any public disclosure.

## What is in scope

Things that would be real vulnerabilities in AgentRelay itself:

- A guard that can be **bypassed** so a forbidden action (a `git push`, a write to a
  `forbid_path`, a network call the config disallowed) reaches the tool anyway.
- The completion gate marking a unit **done without the test evidence** it claims.
- A checkpoint that commits **the user's own uncommitted work**, or that runs a
  destructive git operation (anything other than the `--keep`-style reset it promises).
- The local IPC endpoint being reachable **off the loopback interface**, or accepting
  requests without its token.
- Credential handling that **logs, echoes, or transmits** a secret it should only pass
  through.
- Any path that lets the daemon **execute agent-controlled input as a command** on the
  host outside the configured program.

## What is NOT a vulnerability (by design)

Being honest about the threat model is part of the security posture. These are known,
documented limits — not bugs:

- **Guards are not a sandbox.** They block the actions they *classify* as forbidden.
  An interpreter one-liner (`python -c ...`, `node -e ...`,
  `powershell Invoke-WebRequest ...`) can do things the classifier may not recognise.
  The strong, non-bypassable enforcement is **OS-level isolation with the network
  off** (`isolation` with an empty allowlist → `docker run --network none`), not the
  guards alone. Run untrusted work with isolation on.
- **Workspace-boundary checking is textual path normalisation.** It resolves `.`/`..`
  but does not chase every **symlink or Windows junction**. A workspace containing
  links that point outside it is outside what the boundary guard can promise; isolation
  is again the real containment.
- **An unknown tool name may be under-classified.** The guards fail *closed* for a
  known tool whose arguments they cannot parse, but a brand-new tool with a name the
  policy has never seen may produce no proposed actions and be allowed. Do not rely on
  the guards for tools you have not configured; use isolation.
- **Released binaries are unsigned.** Windows SmartScreen / macOS Gatekeeper will warn.
  Verify the published **SHA-256 checksums** against the release assets. Signing
  requires a certificate we do not yet have.
- Anything requiring an attacker who **already has the ability to run arbitrary code as
  your user** on the host — at that point they do not need AgentRelay.

## The short version

Treat the guards as a seatbelt: they catch the ordinary, known-shaped mistakes and
make the common case safe. For genuinely untrusted or high-value work, turn on
isolation with the network off — that is the belt-and-airbag, and it is what turns
"probably won't exfiltrate" into "cannot."
