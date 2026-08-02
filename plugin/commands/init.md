---
name: agentrelay:init
description: Scaffold .agentrelay/ config for this project, optionally from a template
---

Initialize an AgentRelay workspace here:

```bash
agentrelay init --cwd "${CLAUDE_PROJECT_DIR}"
```

To start from a ready-made config instead of assembling one by hand, list the
templates and init from one:

```bash
agentrelay templates
agentrelay init --template <name> --cwd "${CLAUDE_PROJECT_DIR}"
```

Then open `.agentrelay/config.json` and set the guards, the validators (the real
commands that prove a unit is done — e.g. your `npm test`), a spending cap, and the
scope. Nothing runs until you do.
