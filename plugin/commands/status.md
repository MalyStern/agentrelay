---
name: agentrelay:status
description: Show the AgentRelay workspace status — guards, budget, state, isolation
---

Run the AgentRelay status for this project and show the result:

```bash
agentrelay status --cwd "${CLAUDE_PROJECT_DIR}"
```

It says, in plain words, what is armed: the spending cap (or that there isn't one),
whether the agent is isolated in a container, where the mission stands, and — loudly
— if it is waiting on you and will not continue by itself.
