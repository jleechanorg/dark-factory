# Dark Factory — Attractor-Pattern DOT Pipeline Runner

Python implementation of StrongDM's Attractor pattern: a DOT-based pipeline runner
that orchestrates multi-stage AI workflows using directed graphs.

**The coding agent never sees this repo.** Holdout scenarios live in `holdouts/`;
the agent only sees `specs/`. The evaluator runs holdouts independently.

> **For AI coding agents:** read [`CLAUDE.md`](CLAUDE.md) (or [`AGENTS.md`](AGENTS.md),
> identical content) before touching code. It defines the agent-isolation rule,
> architecture, handler-registry contract, and the CXDB/Healer feedback loop.

## Architecture (3-Layer Convergence)

```
Layer 3: Pipeline Engine (DOT parser + graph runner + checkpointing + human gates)
    ↓
Layer 2: Agent Loop (AO/Claude Code/Codex — existing external tools)
    ↓
Layer 1: Unified LLM Client (OpenClaw gateway / thinclaw MCP)
```

## Directory Layout

```
dark-factory/
├── pipelines/         # .dot files — the durable artifacts worth sharing
│   └── factory/      # Full factory pipeline (seed→architect→specify→implement→expand→sync)
├── holdouts/          # Blind evaluation scenarios — agent NEVER sees these
│   └── <feature>/    # Per-feature holdout directory
│       ├── scenario_001.yaml
│       └── ...
├── specs/            # Feature specs — agent DOES see these
│   └── <feature>.md
├── prompts/           # Prompt templates referenced by .dot nodes
│   └── factory/       # Per-pipeline prompt directories
├── runner/            # Python DOT pipeline engine
│   ├── __init__.py
│   ├── __main__.py    # CLI entry point
│   ├── parser.py      # DOT → graph model
│   ├── engine.py      # Graph traversal + checkpointing
│   ├── handlers.py    # Node handlers + inline backend dispatch (echo|claude|codex via ctx.backend)
│   ├── cxdb.py        # CXDB — SQLite event log of every step (for Healer)
│   └── healer.py      # Healer — clusters CXDB failures into a diagnosis report
│
│   # Note: the holdout evaluator lives in a sealed sibling repo
│   # (~/projects/dark-factory-holdouts/evaluator/run.py); its path is
│   # supplied via the DARK_FACTORY_HOLDOUTS environment variable so the
│   # coding agent never sees scenario files.
├── tests/
├── CLAUDE.md          # Agent guidance (auto-loaded by Claude Code)
├── AGENTS.md          # Same content as CLAUDE.md, for non-Claude agents
└── README.md
```

## Quick Start

```bash
# Smallest end-to-end pipeline (echo backend; no LLM calls)
python -m runner --pipeline pipelines/factory/hello.dot --goal "smoke test"

# Pipeline with gates, recording every step to CXDB
python -m runner \
  --pipeline pipelines/factory/gates.dot \
  --goal "Add rate limiting to campaign creation" \
  --backend claude \
  --feature rate_limit \
  --cxdb ~/.dark-factory/cxdb.sqlite

# Visualize a pipeline
dot -Tpng pipelines/factory/gates.dot -o gates.png

# Run holdout evaluation against the sealed sibling repo
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
python -m runner --pipeline pipelines/factory/gates.dot --goal "Add rate limiting" --feature rate_limit

# After one or more runs, diagnose failures
python -m runner.healer --cxdb ~/.dark-factory/cxdb.sqlite
```

`--feature <name>` selects the holdout subdirectory; the sealed evaluator repo
is located via the `DARK_FACTORY_HOLDOUTS` environment variable. The CLI
intentionally does not accept a holdouts path argument — scenario files must
stay out of the agent's view.

## CXDB + Healer

Every node execution is appended to a SQLite event log (CXDB) when `--cxdb`
is set. The Healer reads that log, clusters terminal failures by
`(node, outcome, output_hash)`, and emits a Markdown diagnosis with a
prescription per cluster (which prompt template, holdout, or gate to inspect).
This is the loop that lets dorodango runner code stay disposable while
learning accumulates in the log, not the code.

## Key Innovation

The `.dot` files are the durable artifact. The runner code is dorodango —
polish it, throw it away, rebuild from spec. The pipeline definitions encode
entire development processes and are worth versioning and sharing.
