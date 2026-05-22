---
description: "Dark Factory — agent isolation, architecture, and commands for the DOT pipeline runner."
type: quality
execution_mode: none
---

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Source material — read these to understand WHY this repo looks the way it does

This repo is a working implementation of the **Attractor pattern** described by these three primary sources. If you're new to the repo, skim them first; the architecture choices below are downstream of them, not invented here.

- **StrongDM, AttractorBench** — <https://github.com/strongdm/attractorbench> — defines the benchmark: agents read a public natural-language spec; the conformance tests, mock LLM server, and scoring harness are generated locally and **intentionally excluded from the public repo to prevent training-data contamination**. We mirror that "spec in, evaluator out" split.
- **Dan Shapiro, "You don't write the code"** — <https://www.danshapiro.com/blog/2026/02/you-dont-write-the-code/> — argues that humans must stop reading the code, not just stop writing it, and lays out the five-level automation ladder (Level 5 = the dark factory: nobody reviews code). Quality enforced by **CXDB + Healer + adversarial cross-review**, not by inspection.
- **2389, "The Dark Factory is a .dot file"** — <https://2389.ai/posts/the-dark-factory-is-a-dot-file/> — the Attractor pattern is StrongDM's open-source NLSpec set; four independent implementations (Kilroy, Mammoth, Smasher, Tracker) converged on the same three-layer architecture. Pipeline `.dot` files are the durable artifact; the runner code itself is *dorodango* — polish, discard, rebuild from spec.

## CRITICAL: Agent Isolation (read first)

The defining constraint is about **who** in the LLM DAG can read **what**, not a blanket prohibition on the human operator.

| Role | Sees | Does NOT see |
|---|---|---|
| **Implementing agent** — the *spawned coding agent under test* (the `codergen` worker spawned by a pipeline node: a Claude Code session, an AO worker, a `codex exec` invocation, etc.) | `specs/<feature>.md`, `prompts/`, the relevant `.dot` graph, its own worktree | `holdouts/`, `runner/evaluator.py`, the contents of any `_holdout/` test source. Reading these collapses the adversarial guarantee (the impl can pass tests by reading them, identical to AttractorBench's "don't ship the conformance tests" rule). |
| **Evaluator agent** — runs the sealed evaluator at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py` against the implementing agent's diff | `holdouts/`, `specs/`, the implementation diff | The implementing agent's chain-of-thought, prompt template internals, anything that would let it grade by inspecting the prompt rather than the artifact |
| **Operator / human (you, the engineer using this repo)** | Everything — runner code, holdouts, evaluator, tests, CXDB logs, this file | Nothing structurally hidden; the discipline is to not paste holdout content into prompts that ship to the implementing agent |

The isolation is enforced **operationally** (prompt construction never references holdout paths or their content) plus **mechanically** (the `_codergen` claude/codex subprocess runs under `sandbox-exec` with `deny file-read* (subpath "~/projects/dark-factory-holdouts")`; `_sanitized_env` strips `DARK_FACTORY_HOLDOUTS` and any `*HOLDOUT*` env vars from the implementing agent's environment).

Holdout scenarios are intentionally **absent from this repo** — they live in the sealed sibling repo at `~/projects/dark-factory-holdouts`, located at runtime via `$DARK_FACTORY_HOLDOUTS`. The CLI deliberately has no `--holdouts-path` flag, so scenario paths cannot leak into the implementing agent's argv. Do not invent a local `holdouts/` tree or a `runner/evaluator.py` inside this repo — both are off-limits by design.

> **As the operator you can — and should — read the sealed repo, the evaluator, and the tests.** What you must not do is hand any of that content (verbatim or paraphrased) to a `codergen` node's prompt template.

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
