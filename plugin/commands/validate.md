---
name: agentrelay:validate
description: Run the completion validators as evidence, without checkpointing
---

Run this project's configured validators and report PASS/FAIL per check:

```bash
agentrelay validate --cwd "${CLAUDE_PROJECT_DIR}" --unit <unit>
```

This is the evidence gate: a unit is "done" only if every validator passes — the
tool's word, not the model's. Exit code is non-zero if any check fails.
