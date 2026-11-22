---
description: "Dark Factory — agent isolation, architecture, and commands for the DOT pipeline runner."
type: quality
execution_mode: none
---

# CLAUDE.md

Keep this file <= 200 lines; move detail to linked docs.

Detailed sections moved verbatim to [docs/agents-md-details.md](docs/agents-md-details.md); each moved section below links to its anchor.

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
7. **Deprecate human interactive hat**: All coding LLM work must be shifted to the auto-factory. Humans only define intent (create GitHub issues/beads) and write comments on PRs for feedback. The auto-factory autonomously processes and drives branches to green without human-interactive coding sessions (exception: explicitly authorized factory infrastructure repair missions may diagnose and repair factory infrastructure in separate worktrees/branches, but product/pilot branches remain strictly autonomous).

## `/af` product runs — ZERO direct work; monitoring only

Moved verbatim to [docs/agents-md-details.md](docs/agents-md-details.md#af-product-runs--zero-direct-work-monitoring-only).

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

Moved verbatim to [docs/agents-md-details.md](docs/agents-md-details.md#architecture).

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

## CLI account scoping — mandatory policy for every AI CLI launch

This is a mandatory launch policy, not a claim that every current runtime path
already enforces it. `Command::new` inherits the daemon environment unless the
launch code explicitly removes inherited variables and builds a scoped child
environment. A launch is compliant only when its account/provider scope is
validated before the process starts.

Every direct Claude launch (`claude` or `claude-sonnet`) MUST validate
`DARK_FACTORY_CLAUDE_CONFIG_DIR` as an existing project-scoped directory, pass
that directory to the child as `CLAUDE_CONFIG_DIR`, and scrub inherited Claude
and provider authentication variables. Keep this environment construction and
provider scrubbing centralized; do not rely on a bare host `~/.claude` account.

MiniMax is a separate provider lane. It MUST require a nonblank
`MINIMAX_API_KEY`, pin `ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic` and
`ANTHROPIC_MODEL=MiniMax-M3`, and remove Claude account state/configuration from
the child environment. A MiniMax launch must never inherit `CLAUDE_CONFIG_DIR`
or another host Claude login as an implicit credential.

Every Codex launch, including `codex exec`, MUST use an intended existing
`CODEX_HOME` or an explicitly supported provider credential/configuration such
as `OPENAI_API_KEY` or `CODEX_ACCESS_TOKEN` where that launch mode supports it.
Do not let Codex silently fall back to the operator's default `~/.codex`
account.

Unscoped or unsupported dispatch MUST fail closed. Fallback is allowed only to
another lane that has its own explicit, validated scope; never fall back to an
unscoped Claude, MiniMax, or Codex process.
