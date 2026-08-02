---
name: agentrelay:graph
description: Build a knowledge graph of this project with graphify and show where it went
---

Build (or refresh) a knowledge graph of this project and print where the outputs are:

```bash
agentrelay graph --cwd "${CLAUDE_PROJECT_DIR}"
```

This uses `graphify update` — an AST-only build that costs **no tokens** — and writes
an interactive `graph.html`, a plain-language `GRAPH_REPORT.md`, and a GraphRAG-ready
`graph.json` into `graphify-out/`. Add `--deep` to use graphify's LLM extraction
instead (richer edges, but it **spends tokens**):

```bash
agentrelay graph --cwd "${CLAUDE_PROJECT_DIR}" --deep
```

AgentRelay invokes graphify as an external tool and never installs it for you. If it
isn't on PATH the command says exactly how to add it
(`uv tool install graphifyy`) and does nothing else.

Once the graph exists, you can query it directly with graphify — e.g.
`graphify query "how does the guard decide to block a call?"` or
`graphify god-nodes` to list the architectural hubs.
