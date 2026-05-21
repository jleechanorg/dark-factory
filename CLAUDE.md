---
description: "Dark Factory — agent isolation, architecture, and commands for the DOT pipeline runner."
type: quality
execution_mode: none
---

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## CRITICAL: Agent Isolation (read first)

This repo's defining constraint: **the implementing agent must never read `holdouts/`, `runner/evaluator.py`, or `tests/`.** Those belong to the separate evaluator context. Violating it destroys the adversarial guarantee that makes the holdout signal meaningful.

What the implementing agent **sees**: `specs/<feature>.md`, `prompts/`, `pipelines/`.
What the implementing agent **never sees**: `holdouts/`, `runner/evaluator.py`, `tests/`.

The evaluator agent runs in a separate context with access to `holdouts/` + `specs/` + the implementation diff, and returns normalized verdicts (PASS / WARN / FAIL).

Holdout scenarios are intentionally **absent from this repo** — they live in a sealed sibling repo at `~/projects/dark-factory-holdouts`, located at runtime via `$DARK_FACTORY_HOLDOUTS`. Do not invent a local `holdouts/` tree or a `runner/evaluator.py` inside this repo; both are off-limits by design. The CLI deliberately has no `--holdouts-path` flag — the path must come from the env var so scenario files cannot leak into argv.

## Common commands

```bash
# Setup
python -m venv .venv && source .venv/bin/activate && pip install -r requirements.txt

# Smoke pipeline — echo backend, no LLM calls
python -m runner --pipeline pipelines/factory/hello.dot --goal "smoke test"

# Full gated pipeline with CXDB recording
python -m runner \
  --pipeline pipelines/factory/gates.dot \
  --goal "<feature description>" \
  --backend claude \
  --feature <feature_name> \
  --cxdb ~/.dark-factory/cxdb.sqlite

# Cluster CXDB failures into a Healer diagnosis
python -m runner.healer --cxdb ~/.dark-factory/cxdb.sqlite

# Visualize a pipeline graph
dot -Tpng pipelines/factory/gates.dot -o gates.png

# Tests (full suite, single file, single test)
python -m pytest tests/
python -m pytest tests/test_engine.py -k green
python -m pytest tests/test_gates.py::test_parse_verdict_pass_warn_fail
```

## Architecture

### Three-layer convergence
1. **Pipeline engine** (`runner/`) — DOT parser, graph runner, checkpointing, human gates, CXDB.
2. **Agent loop** (external) — AO / Claude Code / Codex CLIs, invoked per node.
3. **LLM client** (external) — OpenClaw gateway / thinclaw MCP.

This repo is layer 1 only.

### Durable artifacts vs. dorodango
The `.dot` files under `pipelines/` are the artifact worth versioning — they encode the development process. The Python under `runner/` is treated as throwaway code: polish, discard, rebuild from spec. Learning accumulates in the **CXDB** event log, not in the runner code.

### Pipeline execution model
- `runner/parser.py` reads `.dot` via pydot into `Graph(nodes, edges)`. Every pipeline must contain both `start` and `exit` — `parse` raises if either is missing.
- `runner/engine.py:run` walks from `start`, calls the handler resolved per node, then picks the next edge via `_edge_matches` (supports `condition="key=value"` and `key!=value`). Conditional edges win over unconditional ones.
- Loop bounds come from a node's `max_visits` attribute (e.g. `fix [max_visits="3"]`). Exceeding it emits a synthetic `exhausted` step and terminates.
- Each step is appended to `history` and, when `--cxdb` is set, to the SQLite event log. The CXDB sequence (`seq`) is tracked independently of `len(history)` so refactors can't desync.

### Handlers (`runner/handlers.py`)
Lookup order in `resolve(node)`:
1. Explicit `type="..."` → `TYPE_REGISTRY`
2. Node name `start` / `exit` → built-ins
3. Node `shape` (Mdiamond/Msquare/hexagon) → `REGISTRY`
4. Default → `_codergen`

Handler types:
- `codergen` — render `prompt="@path"` (with `${goal}` and `${state.*}` substitution) and dispatch to `ctx.backend` (`echo` | `claude` | `codex`).
- `tool` — shell out to a `command="..."` attribute with optional `timeout`.
- `human_gate` — block on stdin, or accept pre-seeded `ctx.state["<node>.outcome"]` for tests.
- `conditional` — hexagon decision node; outcome comes from `ctx.state[decision_key]`.
- `holdout_eval` — run the sealed evaluator at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py`. Parses the last JSON line of stdout for `{verdict: pass|...}`.
- `gate_es` / `gate_er` / `gate_code_standards` — shell out to `claude --print /<slash>`. Verdicts are normalized by `_parse_verdict`: the anchored marker regex (`verdict:`/`overall:`/`normalized:` + token) takes priority; the standalone-line fallback only fires when no marker is present; `pass|warn → success`, `fail|partial|inconclusive → failure`. Unknown verdict combined with `rc!=0` becomes `error` (distinct from real failures so the Healer can group infra crashes separately).

### CXDB + Healer feedback loop
- `runner/cxdb.py` records `(run_id, seq, node, outcome, ts, output_hash, output_head, metadata)` per step. WAL mode + 5s busy_timeout so concurrent pipelines into one CXDB don't collide on `database is locked`.
- `runner/healer.py` reads CXDB, clusters terminal failures (`failure | fail | exhausted | stuck | partial | inconclusive`) by `(node, outcome, output_hash)`, and emits a Markdown report with a per-cluster prescription. The Healer's prefix logic (`gate_*`, `plan|implement|fix`, `holdout`) is internal data routing over its own node namespace — not user input — so it is not a ZFC violation.

### Adding a new node type
1. Implement `_my_handler(node, ctx) -> Result` in `runner/handlers.py`.
2. Register in `TYPE_REGISTRY` (preferred — keyed by `type="..."`) or `REGISTRY` (keyed by DOT shape).
3. Reference from a `.dot` file with `mynode [type="my_handler", ...]`.
4. Echo-backend tests should drive paths via `ctx.state["<node>.outcome"]` — see `tests/test_gates.py`.

## Pipeline authoring rules

1. `.dot` files are first-class — version them, review them in PRs.
2. Pipelines must include both `start` and `exit` nodes; the parser enforces this.
3. Edge conditions are simple: `condition="key=value"` or `key!=value`. The runtime does *not* evaluate arbitrary expressions; encode richer logic in a handler.
4. Prompt references use `prompt="@relative/path.md"` (the `@` is stripped by the parser). Templates support `${goal}` and `${state.<key>}` substitution only — no Jinja, no conditionals.
5. Backends are swappable per run via `--backend`; never hardcode a backend in a `.dot` file or handler.
