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
- **jleechanorg AttractorBench fork** — <https://github.com/jleechanorg/attractorbench> — public fork used for local spec-validation experiments. The spec-validation benchmark copied into this repo lives at `benchmarks/attractor-spec-review/`.
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

## Dark Factory operating mode

The target operating model is Level 5: humans define intent, specs, tenets,
holdouts, and outcome audits; agents write code; independent agents and sealed
evaluators review behavior. Human code review is a fallback for maintaining the
factory itself, not the product-quality gate.

Operational rules:

1. Public specs are the scarce artifact. Make them detailed enough that a coding
   agent can build without hidden product requirements.
2. Hidden evaluator cases must cover exact data, adversarial payloads, role
   attacks, race cases, service failures, viewport sizes, and scoring weights.
3. Every non-trivial pipeline should have at least one independent reviewer node
   or tool invocation (`codex exec --yolo`, AO worker, or equivalent) separate
   from the implementing agent.
4. Merge confidence should come from outcome artifacts: public spec validation,
   deterministic tests, sealed holdouts, independent reviewer reports, CXDB
   history, and evidence bundles.
5. Do not optimize for cheap validation. If the factory is not spending serious
   token budget on adversarial validation, it is probably under-testing.
6. Treat `.dot` graphs as the durable process code. Runner code is disposable;
   graph shape, specs, holdouts, and scoring contracts are the important assets.
7. **Deprecate human interactive hat**: All coding LLM work must be shifted to the auto-factory. Humans only define intent (create GitHub issues/beads) and write comments on PRs for feedback. The auto-factory autonomously processes and drives branches to green without human-interactive coding sessions.

## /af — ZERO direct work; monitoring only (operator hard rule)

When the operator directs work through /af (or sets an /af goal), the session
does **ZERO direct work** — no product code, no factory code, no hand-fixes,
no coding sub-agent lanes. The session's ONLY jobs:

1. File/refine `factory`-labeled beads (SHORT descriptions — AO's spawn
   prompt cap is 4096 characters; a long bead body makes its own dispatch
   fail) with `target_repo:` / `existing_pr:` / `existing_branch:` fields
   when driving an existing PR.
2. Monitor daemon telemetry (`~/Library/Logs/dark-factory/daemon.jsonl`) and
   report each lifecycle stage.
3. Escalate blockers the factory cannot self-fix to the OPERATOR, naming the
   exact blocker — never hand-fix them. "The factory is broken so I'll do it
   myself" is the forbidden move: it hides factory gaps and makes the
   label→merge E2E proof unfalsifiable (2026-07-11/12 incidents: hand-driven
   PRs masked a dead coder loop for a full day).

## Factory host placement (Linux-only)

`jeff-ubuntu` is the sole Auto-Factory host. Start, stop, inspect, and deploy
the daemon only through `/linux` (`ssh jeff-ubuntu ...`) and its user systemd unit
`ai.dark-factory.daemon.service`. AO worker dispatch runs on that Linux host only.
This Mac is an operator client: do not load or start a Dark Factory LaunchAgent,
a local daemon, or local AO workers from factory intake on macOS.
Always use SSH to Linux for telemetry (`/home/jleechan/Library/Logs/dark-factory/daemon.jsonl`)
and operational control.

## Setup

```bash
# One-time install (uv-managed Python + venv + binaries on PATH)
./install.sh

export DARK_FACTORY_HOME=~/projects/dark-factory
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
export PATH="$HOME/.local/bin:$PATH"
```

## Common commands

```bash
# Smoke pipeline — echo backend, no LLM calls
dark-factory --pipeline pipelines/factory/hello.dot --goal "smoke test" --backend echo

# Full gated pipeline with CXDB recording (run from target repo cwd)
dark-factory \
  --pipeline pipelines/factory/gates.dot \
  --goal "<feature description>" \
  --backend ao \
  --ao-agent antigravity \
  --feature <feature_name> \
  --cxdb ~/.dark-factory/cxdb.sqlite

# Cluster CXDB failures into a Healer diagnosis
df-healer --cxdb ~/.dark-factory/cxdb.sqlite

# Visualize a pipeline graph
dot -Tpng pipelines/factory/gates.dot -o gates.png

# Tests (full suite, single file, single test)
.venv/bin/python -m pytest tests/
.venv/bin/python -m pytest tests/test_engine.py -k green
.venv/bin/python -m pytest tests/test_gates.py::test_parse_verdict_pass_warn_fail
```

Prefer **`dark-factory`** / **`df-healer`** on PATH for `/f` and `/factory`.
Legacy dev-only: `.venv/bin/python -m runner ...` from `$DARK_FACTORY_HOME`.

When `--pipeline` is omitted, `/f` and `/factory` default to `pipelines/slim/two_node.dot` (generic worker + fresh fully tooled Codex reviewer with verbatim feedback). Pass explicit `--pipeline <name>` to opt into non-default pipelines (see [docs/pipeline-selection.md](docs/pipeline-selection.md)).

## Architecture

### Three-layer convergence
1. **Pipeline engine** (`runner/`) — DOT parser, graph runner, checkpointing, human gates, CXDB.
2. **Agent loop** (external) — AO / Claude Code / Codex CLIs, invoked per node.
3. **LLM client** (external) — OpenClaw gateway / thinclaw MCP.

This repo is layer 1 only.

### Agent Orchestrator (AO) Repository Policy
- The canonical AO engine used by dark-factory is upstream **Golang `agent-orchestrator`** (`https://github.com/strongdm/agent-orchestrator` / `jleechanorg/agent-orchestrator`). We do NOT use `agent-orchestrator-ts`.
- The AO repository is **read-only / reference only**. We almost never want to modify or open PRs against AO; all session liveness interpretation, reaping triggers, timeout logic, and promotion handling must live within `dark-factory` itself (e.g. in `daemon/src/adapters.rs` and `daemon/src/tick.rs`).
- **Hard Safety Gate**: Agents must **NEVER** write or modify `agent-orchestrator` code unless the human operator explicitly provides the verbatim authorization: `AO CODE APPROVED`.
- **AO CLI Wrapper Fidelity**: Host CLI wrappers for `ao` or any external tool must never modify, strip, or suppress machine-readable flags (such as `--json`, `--format`, `--porcelain`). Wrappers must pass through all arguments transparently to preserve downstream parsing contracts.
- **Project Scoping**: All `ao status` and session management queries from `dark-factory` must be project-scoped using `-p <project>` to avoid scanning all registered host repositories and hitting rate limits or incurring unnecessary latency.


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
- `codergen` — render `prompt="@path"` (with `${goal}` and `${state.*}` substitution) and dispatch to the node's `backend`/`model` attribute or `ctx.backend`. The runner CLI accepts `echo` | `ao` | `claude` | `codex` | `agy`; per-node/model-stylesheet routing also supports `mock_llm` for test/conformance lanes. The default is `ao` (running `antigravity` agent under the hood via Agent Orchestrator).
- `agy` backend — run Antigravity CLI directly and headlessly with `agy --print --dangerously-skip-permissions`; node `timeout="..."` maps to `agy --print-timeout`.
- Reviewer/evaluator lanes are separate nodes: `tool` nodes can invoke `codex exec --yolo`, AO workers, or another reviewer CLI; `holdout_eval` runs the sealed Python evaluator from `$DARK_FACTORY_HOLDOUTS`.
- `tool` — shell out to a `command="..."` attribute with optional `timeout`.
- `human_gate` — block on stdin, or accept pre-seeded `ctx.state["<node>.outcome"]` for tests.
- `conditional` — hexagon decision node; outcome comes from `ctx.state[decision_key]`.
- `holdout_eval` — run the sealed evaluator at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py`. Parses the last JSON line of stdout for `{verdict: pass|...}`.
- `gate_es` / `gate_er` / `gate_code_standards` — shell out to `claude --print /<slash>`. Verdicts are normalized by `_parse_verdict`: the anchored marker regex (`verdict:`/`overall:`/`normalized:` + token) takes priority; the standalone-line fallback only fires when no marker is present; `pass|warn → success`, `fail|partial|inconclusive → failure`. Unknown verdict combined with `rc!=0` becomes `error` (distinct from real failures so the Healer can group infra crashes separately).

### Spec validation benchmark

`benchmarks/attractor-spec-review/` is the local copy of the Attractor-style
spec-validation experiment. Its public graph and validator are intentionally
visible:

- `benchmarks/attractor-spec-review/pipelines/review_slim.dot`
- `benchmarks/attractor-spec-review/pipelines/review_full.dot`
- `benchmarks/attractor-spec-review/starter/scripts/validate_spec.py`
- `benchmarks/attractor-spec-review/scripts/review_with_codex.sh`

The full graph validates every reviewable spec line, runs a full-stack smoke
node, then invokes an independent reviewer through `codex exec --yolo`.
Use this benchmark as the template for general spec-validation lanes.

### CXDB + Healer feedback loop
- `runner/cxdb.py` records `(run_id, seq, node, outcome, ts, output_hash, output_head, metadata)` per step. WAL mode + 5s busy_timeout so concurrent pipelines into one CXDB don't collide on `database is locked`.
- `runner/healer.py` reads CXDB, clusters terminal failures (`failure | fail | exhausted | stuck | partial | inconclusive`) by `(node, outcome, output_hash)`, and emits a Markdown report with a per-cluster prescription. The Healer's prefix logic (`gate_*`, `plan|implement|fix`, `holdout`) is internal data routing over its own node namespace — not user input — so it is not a ZFC violation.

### Performance logging (`~/Library/Logs/dark-factory`)

Enabled by default. Logs are organized by **target workdir git identity** (prefers `ao.worktree` when set):

```text
~/Library/Logs/dark-factory/<repo-slug>/<branch-slug>/
  <run_id>.jsonl          # structured node_enter / node_exit / transition / run_end
  <run_id>.log              # human-readable ENTER/EXIT lines with outcome + duration_ms
  latest.jsonl -> <run_id>.jsonl
  latest.log -> <run_id>.log
  runs.index.jsonl          # one summary line per run (branch rollup)
```

The default lives under `~/Library/Logs/` (Apple's standard per-app log location) so runs survive reboots, macOS `/tmp` periodic sweeps, and AO retag cycles. **Do not pass `/tmp/...` as `--perf-log-dir`** — that's the failure mode that lost v9 (2026-06-11).

- **Disable:** `--no-perf-log`
- **Custom root:** `--perf-log-dir /path/to/root` (default `~/Library/Logs/dark-factory`)
- CLI JSON summary includes `perf_log.jsonl`, `perf_log.log`, `repo`, `branch` when enabled.

Monitor a branch in real time:

```bash
tail -f ~/Library/Logs/dark-factory/worldarchitect.ai/feat_my-feature/latest.log
tail -f ~/Library/Logs/dark-factory/worldarchitect.ai/feat_my-feature/runs.index.jsonl
```

Each node emits `ENTER` on visit and `EXIT` with classified outcome (`success`, `failure`, `error`, `partial`) plus engine-level `duration_ms`. Parallel branch nodes are included.

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
5. Coder backends are swappable per run via `--backend` or per codergen node via
   `backend="codex"` / `backend="claude"` / `backend="ao"` / `backend="agy"`. The recommended default is `backend="ao"` with `--ao-agent antigravity` (running Antigravity CLI through Agent Orchestrator). Use reviewer
   `tool` nodes when separating coder and reviewer/evaluator CLIs.
6. Use `model_stylesheet="path.model.css"` when a graph needs CSS-like
   backend/model routing without cluttering every node.
