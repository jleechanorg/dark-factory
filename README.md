# Dark Factory — Attractor-Pattern DOT Pipeline Runner

Python implementation of StrongDM's Attractor pattern: a DOT-based pipeline runner
that orchestrates multi-stage AI workflows using directed graphs.

**The coding agent never sees this repo.** Holdout scenarios live in `holdouts/`;
the agent only sees `specs/`. The evaluator runs holdouts independently.

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
│   ├── parser.py      # DOT → graph model
│   ├── engine.py      # Graph traversal + checkpointing
│   ├── handlers.py    # Node type handlers (codergen, tool, conditional, fan-in, human-gate)
│   ├── backends.py    # Agent backends (AO, Claude Code, Codex)
│   └── evaluator.py   # Holdout scenario runner
├── tests/
├── .claude/           # Agent config (scoped to this repo only)
│   └── CLAUDE.md
└── README.md
```

## Quick Start

```bash
# Run a pipeline
python -m runner --pipeline pipelines/factory/implement.dot --goal "Add rate limiting to campaign creation"

# Run with holdout evaluation
python -m runner --pipeline pipelines/factory/implement.dot --goal "Add rate limiting" --holdouts holdouts/rate_limit/

# Visualize a pipeline
dot -Tpng pipelines/factory/implement.dot -o implement.png
```

## Key Innovation

The `.dot` files are the durable artifact. The runner code is dorodango —
polish it, throw it away, rebuild from spec. The pipeline definitions encode
entire development processes and are worth versioning and sharing.
