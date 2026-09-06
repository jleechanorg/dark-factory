---
description: "Dark Factory — agent isolation, architecture, and commands for the DOT pipeline runner."
type: quality
execution_mode: none
---

# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Source material — read these to understand WHY this repo looks the way it does

This repo is a working implementation of the **Attractor pattern** described by these primary sources (architecture choices are downstream of them, not invented here):

- **StrongDM, AttractorBench** — <https://github.com/strongdm/attractorbench> — defines the benchmark: agents read a public natural-language spec; the conformance tests, mock LLM server, and scoring harness are generated locally and **intentionally excluded from the public repo to prevent training-data contamination**. We mirror that "spec in, evaluator out" split.
- **jleechanorg AttractorBench fork** — <https://github.com/jleechanorg/attractorbench> — public fork used for local spec-validation experiments. The spec-validation benchmark copied into this repo lives at `benchmarks/attractor-spec-review/`.
- **Dan Shapiro, "You don't write the code"** — <https://www.danshapiro.com/blog/2026/02/you-dont-write-the-code/> — argues that humans must stop reading the code, not just stop writing it, and lays out the five-level automation ladder (Level 5 = the dark factory: nobody reviews code). Quality enforced by **CXDB + Healer + adversarial cross-review**, not by inspection.
- **2389, "The Dark Factory is a .dot file"** — <https://2389.ai/posts/the-dark-factory-is-a-dot-file/> — the Attractor pattern is StrongDM's open-source NLSpec set; four independent implementations (Kilroy, Mammoth, Smasher, Tracker) converged on the same three-layer architecture. Pipeline `.dot` files are the durable artifact; the runner code itself is *dorodango* — polish, discard, rebuild from spec.

## CRITICAL: Agent Isolation (read first)

The defining constraint is about **who** in the LLM DAG can read **what**, not a blanket prohibition on the human operator.

| Role | Sees | Does NOT see |
|---|---|---|
| **Implementing agent** — the *spawned coding agent under test* (e.g. Claude Code session, AO worker, `codex exec` invocation) | `specs/<feature>.md`, `prompts/`, the relevant `.dot` graph, its own worktree | `holdouts/`, `runner/evaluator.py`, `_holdout/` test sources (reading these collapses the adversarial guarantee, mirroring AttractorBench's "don't ship conformance tests" rule). |
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
the daemon only through `/linux` and its user systemd unit
`ai.dark-factory.daemon.service`. AO worker dispatch is allowed on that Linux
host only. This Mac is an operator client: do not load or start a Dark Factory
LaunchAgent, a local daemon, or local AO workers from factory intake. Use SSH
to Linux for telemetry and operational control.

## Setup

```bash
# One-time install (uv-managed Python + venv + binaries on PATH)
./install.sh

export DARK_FACTORY_HOME=~/projects/dark-factory
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
export PATH="$HOME/.local/bin:$PATH"
```

## Common commands

See [.claude/skills/dark-factory-commands/SKILL.md](.claude/skills/dark-factory-commands/SKILL.md).

## Pipeline selection

**Pick the `.dot` for the task** — do not reuse one default for every run.
Full decision table: [docs/pipeline-selection.md](docs/pipeline-selection.md).

Quick guide:

| Task | Pipeline |
|------|----------|
| Smoke / wiring | `pipelines/factory/hello.dot` |
| New feature (full) | `pipelines/slim/minimal_feature.dot` |
| PR iteration | `pipelines/slim/minimal_pr.dot` |
| Gates + holdout | `pipelines/factory/gates.dot` |
| PR gates only | `pipelines/factory/pr_gates.dot` |
| Spec review | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` |

`/f` and `/factory` must classify the goal (factory-spec Step 0) and choose a
pipeline before invoking `dark-factory`, unless the user passed `--pipeline`.

## Architecture

### Agent Orchestrator (AO) Repository Policy
- The canonical AO engine used by dark-factory is upstream **Golang `agent-orchestrator`** (`https://github.com/strongdm/agent-orchestrator` / `jleechanorg/agent-orchestrator`). We do NOT use `agent-orchestrator-ts`.
- The AO repository is **read-only / reference only**. We almost never want to modify or open PRs against AO; all session liveness interpretation, reaping triggers, timeout logic, and promotion handling must live within `dark-factory` itself (e.g. in `daemon/src/adapters.rs` and `daemon/src/tick.rs`).
- **Hard Safety Gate**: Agents must **NEVER** write or modify `agent-orchestrator` code unless the human operator explicitly provides the verbatim authorization: `AO CODE APPROVED`.

### Durable artifacts vs. dorodango
The `.dot` files under `pipelines/` are the artifact worth versioning — they encode the development process. The Python under `runner/` is treated as throwaway code: polish, discard, rebuild from spec. Learning accumulates in the **CXDB** event log, not in the runner code.

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

See [.claude/skills/dark-factory-node-type/SKILL.md](.claude/skills/dark-factory-node-type/SKILL.md).

## Dispatch-health triage (when beads queue but nothing dispatches)

Check in THIS order — each layer can silently starve the ones below (2026-07-08 incident, bead jleechan-la67):

1. **Telemetry error tail**: `grep DISPATCH ~/Library/Logs/dark-factory/daemon.jsonl | tail` — read the FULL error string (config warnings are noise; the real error is at the end).
2. **AO session cap**: `ao session ls -p <project> | wc -l` vs config `max_workers`. Idle-but-alive coder sessions hold slots (jleechan-tnri/d0wn) — revive via `ao send <session> "<continuation nudge>"` before killing; kill only wedged/superseded sessions via `ao session kill`.
3. **AO spawn admission queue**: `~/.agent-orchestrator/<instance>/sessions/spawn-queue-<project>.json` — `MAX_PENDING_REQUESTS=100`, fork-local code (NOT upstream AgentWrapper), no dedup/TTL: the daemon re-enqueues every retry cycle, so a stalled consumer fills it in hours. `/callpath run dark-factory` now probes depth (`ao_spawn_queue` hop). Flush = backup the file, then atomically write `{"pending":[]}` — AO treats reset state as start-fresh and the daemon re-requests on demand.
4. **Error classification trap**: "Spawn queue is full" arrives as rc=1 Tool error (counter-incrementing), NOT the `REQUEST=` Deferred shape — until jleechan-la67 lands, sustained queue-full burns `spawn_failure_count` toward mass HumanHeld park.

Design rule: any queue between the daemon and AO must have dedup (one pending request per bead+attempt), TTL, depth telemetry, and a flush command. A queue with an unbounded producer and a stallable consumer is an outage timer.

## Automation script config convention (empty-by-default, fail closed)

Any new `daemon/scripts/auto-*.sh` or `daemon/scripts/*-merge-*.sh` script
that performs an irreversible/outward-facing action (merge, push, delete,
publish) must ship a matching `config/*_allowlist.json` with an **empty**
default list — fail closed, not fail open. See
[docs/automation-config-convention.md](docs/automation-config-convention.md)
for the full rule (canonical example: `config/auto_merge_repo_allowlist.json`,
from the 2026-08-23 PR-merge-storm incident). CI enforces it via
`scripts/check_auto_script_configs.sh` (run locally with no args to dry-run
against the real `daemon/scripts/`).

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
7. Every code-producing graph must route a reviewer `outcome!=success` to a bounded
   `fix` loop, not to `exit`; CI enforces this via `runner.graph_audit`. G4: reviewer
   prompts that need the implementing agent's diff should reference `${diff}` in their
   template; `_codergen` captures it automatically and stashes in
   `ctx.state['<node>.diff']` and `ctx.state['_last_diff']`.

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
