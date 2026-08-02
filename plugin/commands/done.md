---
name: agentrelay:done
description: Finish a unit — validate, and checkpoint to git only if it passes
---

Complete a unit of work. AgentRelay runs the validators and, only if they all pass,
commits the agent's work as a checkpoint:

```bash
agentrelay begin --cwd "${CLAUDE_PROJECT_DIR}" --unit <unit>   # before the agent starts
agentrelay done "what this unit did" --cwd "${CLAUDE_PROJECT_DIR}" --unit <unit>
```

`begin` first records what you already had uncommitted, so the checkpoint commits
ONLY the agent's work and never sweeps up your own edits. If the checks fail, nothing
is committed and the mission waits for you — or try `agentrelay fix` to have the
agent attempt a bounded, self-stopping repair.
