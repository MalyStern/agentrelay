---
name: agentrelay:checkpoints
description: List the checkpoints the supervisor made, with how to undo one
---

List every checkpoint, newest first:

```bash
agentrelay checkpoints --cwd "${CLAUDE_PROJECT_DIR}"
```

Each is a real git commit of validated work, authored as AgentRelay so it is
distinguishable from yours. To see what a checkpoint changed:

```bash
git diff --stat <sha> HEAD
```

To undo a unit, AgentRelay prints the exact command when it checkpoints — it uses
`git reset --keep`, which refuses rather than discarding your uncommitted work.
