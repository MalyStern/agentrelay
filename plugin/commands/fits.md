---
name: agentrelay:fits
description: Would a unit of this length fit before the quota window closes?
---

Ask whether starting a unit of roughly N minutes fits in what remains of the quota
window:

```bash
agentrelay fits --minutes 30 --cwd "${CLAUDE_PROJECT_DIR}"
```

Exit 0 means go; exit 75 means wait — being cut off mid-unit leaves half-done,
unvalidated, uncheckpointed work, which costs more than waiting. With no quota window
reported yet, it says so and lets you start.
