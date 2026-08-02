---
name: agentrelay:report
description: The morning report — what happened overnight, and how each line is known
---

Show what the supervised run actually did, with every line labelled by its source:

```bash
agentrelay report --cwd "${CLAUDE_PROJECT_DIR}"
```

`VERIFIED` means a command ran and its exit code decided it; `RECORDED` means the
supervisor observed it; `ESTIMATED` is a rule of thumb. Nothing the model merely
claimed appears as fact. For a one-line headline (e.g. for a glance):

```bash
agentrelay report --short --cwd "${CLAUDE_PROJECT_DIR}"
```
