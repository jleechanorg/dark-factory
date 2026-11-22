# AGENTS.md details

Detail moved verbatim from [AGENTS.md](../AGENTS.md) to keep it under 200 lines. These rules remain in force.

## `/af` product runs — ZERO direct work; monitoring only

When the requested object is to make `/af` work, that is a factory-infrastructure
repair mission, not a product run through `/af`. The repair request authorizes
normal scoped coding, testing, and deployment of factory infrastructure,
including direct or delegated coding as needed. Keep intake quiesced, isolate
the repair in its own worktree/branch, and do not count repair evidence as pilot
evidence. This boundary does not authorize hand-driving selected product or
pilot branches.

During ordinary intake (or when an `/af` goal directs product work), the session
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

**Factory Infrastructure Repair Exception**:
When the operator explicitly authorizes a factory repair mission (a natural-language goal to make /af work counts as explicit authorization), the session is permitted to diagnose and repair factory infrastructure/harness code in a separate worktree and branch. Operational rules for repair missions:
- Strictly isolate infrastructure repairs from selected pilot/product PR branches; never hand-drive pilot PRs.
- Preserve existing logs and telemetry; create and hash backups before modifying files.
- Independently validate all repairs, then deploy only through the canonical Linux workflow (`jeff-ubuntu` via SSH).
- After deployment, resume a clean, real /af pilot to prove end-to-end operation.
- Infrastructure changes never count as autonomous pilot evidence.
- Safety invariants remain absolute: existing repository-specific merge authorization gates remain unchanged, and AO code writes still require verbatim `AO CODE APPROVED`.

**Interactive Developer Override**: This zero-direct-work constraint applies strictly
to hands-off autonomous `/af` goal runs. When the operator directly commands you
to code, repair, unpark beads, re-run gates, drive verifications, or finish tasks
(e.g., "you are coding", "keep going and finish 1-4", "repair this"), you are in
active engineering mode. Act with full authority to diagnose, unpark, fix, run
evidence commands, and drive the task to completion. Never refuse a direct
interactive coding instruction by misclassifying it as a hands-off `/af` run.

**Outside /af** (ad hoc repo work, audits, reviews), follow
`~/.claude/skills/parallelize-to-ceiling/SKILL.md`: fan out independent
read-only and audit work concurrently, and route actual coding to subagents or
cheaper-tier lanes rather than the main session. **Inside /af**, that policy is
superseded — no in-session coding sub-agent lanes; all coding routes through
the external bead → daemon → AO pipeline per the ZERO-direct-work rule above.

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
