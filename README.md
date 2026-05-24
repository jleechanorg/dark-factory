# Dark Factory — Attractor-Pattern DOT Pipeline Runner

Python implementation of the **Attractor pattern**: a DOT-based pipeline runner
that orchestrates multi-stage AI workflows using directed graphs.

## Source material

- StrongDM, **AttractorBench** — <https://github.com/strongdm/attractorbench> — the benchmark we mirror: agents read a public natural-language spec, the conformance harness is generated locally and held out of the public repo to prevent training-data contamination.
- jleechanorg, **AttractorBench fork** — <https://github.com/jleechanorg/attractorbench> — public fork used for spec-validation experiments and cross-repo validation.
- Dan Shapiro, **"You don't write the code"** — <https://www.danshapiro.com/blog/2026/02/you-dont-write-the-code/> — Level 5 = the dark factory, lights off, nobody reviews the code; quality enforced by observability + adversarial review, not by reading.
- 2389, **"The Dark Factory is a .dot file"** — <https://2389.ai/posts/the-dark-factory-is-a-dot-file/> — four independent Attractor implementations (Kilroy, Mammoth, Smasher, Tracker) converged on the same three-layer architecture; pipeline `.dot` files are the durable artifact, the runner code is *dorodango* (polish, discard, rebuild from spec).

## Role boundary

**The *spawned coding agent* (the `codergen` worker — Claude / Codex / AO session) never reads `holdouts/`, `runner/evaluator.py`, or the source of any `_holdout/` tests.** That's the AttractorBench rule, enforced here by `sandbox-exec` deny rules + `_sanitized_env` stripping in `runner/handlers.py`.

You, the **operator**, can and should read all of it — the runner, the sealed `dark-factory-holdouts/` sibling, the evaluator, the tests, the CXDB logs. The discipline is that nothing you read leaks into the prompt template a `codergen` node ships to the worker.

> **For AI coding agents:** read [`CLAUDE.md`](CLAUDE.md) (or [`AGENTS.md`](AGENTS.md),
> identical content) before touching code. It defines the operator-vs-implementing-agent
> isolation rule, architecture, handler-registry contract, and the CXDB/Healer feedback loop.

## Architecture (3-Layer Convergence)

```
Layer 3: Pipeline Engine (DOT parser + graph runner + checkpointing + human gates)
    ↓
Layer 2: Agent Loop (AO/Claude Code/Codex — existing external tools)
    ↓
Layer 1: Unified LLM Client (OpenClaw gateway / thinclaw MCP)
```

## Dark Factory Mode

The target mode is Level 5: humans own specs, holdouts, validation economics,
and outcome audits; agents write code; independent agents and sealed evaluators
review behavior. Human diff review is not the product-quality gate.

The repo supports that mode with:

- Public NLSpecs in `benchmarks/*/spec.md`.
- Versioned DOT graphs in `pipelines/` and `benchmarks/*/pipelines/`.
- Runner CLI coder backends (`echo`, `claude`, `codex`, `ao`).
- Per-node codergen routing can also use `mock_llm` for test/conformance lanes
  through node `backend`/`model` attributes or model stylesheets.
- Reviewer/evaluator lanes as separate nodes: `tool` nodes can invoke
  `codex exec --yolo`, AO workers, or other reviewer CLIs; sealed
  `holdout_eval` nodes invoke the evaluator from `$DARK_FACTORY_HOLDOUTS`.
- Sealed evaluator execution via `$DARK_FACTORY_HOLDOUTS/evaluator/run.py`.
- Independent reviewer tool nodes, including `codex exec --yolo` or AO reviewers.
- CXDB + Healer failure clustering for outcome auditing.
- Spec-validation graphs copied into `benchmarks/attractor-spec-review/`.

Validation should be adversarial and expensive enough to matter. If a benchmark
is only doing cheap deterministic smoke, it is not yet a real dark factory run.

## Directory Layout

```
dark-factory/
├── pipelines/         # .dot files — the durable artifacts worth sharing
│   └── factory/      # Full factory pipeline (seed→architect→specify→implement→expand→sync)
├── benchmarks/        # Public NLSpecs, starter scaffolds, benchmark DOT graphs
│   ├── amazon-clone/  # Full-stack commerce benchmark
│   ├── airbnb-clone/  # Sprinted Firebase emulator benchmark
│   └── attractor-spec-review/ # Spec validation graph + validator copied into repo
├── specs/            # Feature specs — agent DOES see these
│   └── <feature>.md
├── prompts/           # Prompt templates referenced by .dot nodes
│   └── factory/       # Per-pipeline prompt directories
├── runner/            # Python DOT pipeline engine
│   ├── __init__.py
│   ├── __main__.py    # CLI entry point
│   ├── parser.py      # DOT → graph model
│   ├── engine.py      # Graph traversal + checkpointing
│   ├── handlers.py    # Node handlers + backend dispatch (echo|mock_llm|ao|claude|codex)
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

## Spec Validation Benchmark

The general Attractor spec validator lives in:

- `benchmarks/attractor-spec-review/starter/scripts/validate_spec.py`
- `benchmarks/attractor-spec-review/pipelines/review_slim.dot`
- `benchmarks/attractor-spec-review/pipelines/review_full.dot`
- `benchmarks/attractor-spec-review/scripts/review_with_codex.sh`

Run it locally:

```bash
bash benchmarks/attractor-spec-review/scripts/run_matrix_deterministic.sh /tmp/attractor-review-matrix
```

Run the full-stack smoke node:

```bash
cd benchmarks/attractor-spec-review
bash starter/scripts/fullstack_smoke.sh
```

## Quick Start

```bash
# Setup: Python 3.13 is the supported runtime for this repo.
PYTHON_BIN="${PYTHON_BIN:-$(command -v python3.13)}"
"$PYTHON_BIN" -m venv .venv
.venv/bin/python -m pip install -r requirements.txt

# Smallest end-to-end pipeline (echo backend; no LLM calls)
.venv/bin/python -m runner --pipeline pipelines/factory/hello.dot --goal "smoke test"

# Pipeline with gates, recording every step to CXDB
.venv/bin/python -m runner \
  --pipeline pipelines/factory/gates.dot \
  --goal "Add rate limiting to campaign creation" \
  --backend claude \
  --feature rate_limit \
  --cxdb ~/.dark-factory/cxdb.sqlite

# Visualize a pipeline
dot -Tpng pipelines/factory/gates.dot -o gates.png

# Run holdout evaluation against the sealed sibling repo
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
.venv/bin/python -m runner --pipeline pipelines/factory/gates.dot --goal "Add rate limiting" --feature rate_limit

# After one or more runs, diagnose failures
.venv/bin/python -m runner.healer --cxdb ~/.dark-factory/cxdb.sqlite
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
