# Controller-owned cold review contract (v1)

## Background

The factory previously dispatched ad-hoc reviewer lanes without a shared
contract, so two parallel reviewers could disagree about which evidence
counts, which prompt is authoritative, or which digest binds the response.
This branch introduces one controller-owned contract (v1 = Codex only) that
both the standalone `dark-factory review` binary and every in-graph
`parallel_reviewer` lane must route through.

The shape was specified by the Slack-supplied review request
[C09GRLXF9GR/p1785487304101909](https://jleechanai.slack.com/archives/C09GRLXF9GR/p1785487304101909)
and then independently audited by three Spark lanes. The supplied patch was
treated as input — its receipt logic was unsafe (PASS receipt only worked
for Codex; in-graph lanes bypassed enforcement) and was replaced by a
corrected TDD implementation.

## Goals

- One static authority (controller prompt) loaded from this repo, never
  from the target.
- One controller request/execution/artifact implementation shared between
  CLI and graph.
- Per-lane distinct neutral cwd + output directory so primary + shadows
  cannot clobber each other.
- Immutable target validation: no symlinks, no escapes, no holdout
  pointers, no over-1-MiB files.
- Backend-neutral calibration needs but v1 is Codex only.

## Tenets

1. Source-owned authority. The controller prompt, its SHA-256 pin, and
   the C0–C6 / E0–E13 checklist live in this repo (`prompts/catalog/
   controller_cold_review_v1.md`). The target workspace can never replace
   them.
2. Strict response validation. `PROMPT_ID`, `PROMPT_SHA256`,
   `ENVELOPE_SHA256`, `HEAD_SHA`, `TASK_SHA256`, `DIFF_SHA256`,
   `CHANGED_FILES_SHA256`, `EVIDENCE_MANIFEST_SHA256`, `VERDICT`, and every
   checklist line must appear exactly once and match exactly. Failure is
   fail-closed.
3. Read-only reviewer. The reviewer runs in a unique neutral cwd with
   `codex exec --json --ephemeral --skip-git-repo-check --sandbox
   read-only`. `--yolo`, `--dangerously-bypass-approvals-and-sandbox`, and
   any write-capable mode are explicitly rejected at the transport
   builder.
4. Immutable target. `validate_immutable_target` rejects symlinks,
   non-directories, paths that resolve into any holdout root, evidence
   paths that escape the workspace, evidence paths that are not regular
   files, and any file over the 1 MiB ceiling. Post-review reverification
   catches workspace mutation.
5. Per-lane isolation. Every parallel lane (primary + every shadow) gets
   its own `lane_output_dir(neutral_cwd, lane)` and writes its own
   controller receipt.
6. Backend-honest. v1 advertises Codex only. Other backends may be added
   only after they have an equivalent safe adapter.

## High-level description

| Slice | Files | Status |
|-------|-------|--------|
| Wrapper | `bin/dark-factory`, `tests/test_slash_command_binary_contract.py` | committed + pushed ([`e2f97f56c`](https://github.com/jleechanorg/dark-factory/commit/e2f97f56ca8b4ec9eaef333cbf36143c2e6e701a)) |
| Controller core | `runner/review_controller.py`, `prompts/catalog/controller_cold_review_v1.md`, `tests/test_review_controller.py` | committed + pushed ([`e7e2cea50`](https://github.com/jleechanorg/dark-factory/commit/e7e2cea5073f11ccfb8a3be9660f120ea51b984a)) |
| Safe transport | `runner/handler_dispatch.py`, `runner/handlers.py`, `tests/test_gate_subprocess_dispatch.py` | committed + pushed ([`2742d99a9`](https://github.com/jleechanorg/dark-factory/commit/2742d99a96b339c2ae77417c881dfdb10d21a078)) |
| Immutable target | `runner/review_controller.py` (`validate_immutable_target`), `tests/test_immutable_target.py` | committed + pushed ([`f21befaee`](https://github.com/jleechanorg/dark-factory/commit/f21befaee776fa085b68336bbdce71d9f3cd4903)) |
| Graph integration | `runner/review_controller.py` (`run_controller_review`), `runner/handler_parallel_reviewer.py` (`lane_output_dir`), `tests/test_graph_controller_integration.py` | committed + pushed ([`96d7fbd01`](https://github.com/jleechanorg/dark-factory/commit/96d7fbd01)) |
| Docs | `.claude/skills/reviewer-calibration/SKILL.md`, `.claude/workflows/dark-factory.md` | committed + pushed ([`d51f9e369`](https://github.com/jleechanorg/dark-factory/commit/d51f9e369)) |

The 3 hard-tier factory pipelines (`pipelines/factory/gates.dot`,
`pipelines/factory/level5_feature.dot`, `pipelines/factory/pr_gates.dot`)
already opt into `review_contract="cold-review-v1"` on the reviewer nodes.

## Testing

| Suite | Result |
|-------|--------|
| `tests/test_review_controller.py` | 36 pass |
| `tests/test_immutable_target.py` | 14 pass |
| `tests/test_graph_controller_integration.py` | 6 pass |
| `tests/test_gate_subprocess_dispatch.py` | 19 pass |
| `tests/test_review_cli.py` | partial — pre-existing failure from the Slack patch |
| `tests/test_parallel_codex_reviewer.py` | partial — pre-existing failure from the Slack patch |
| `tests/test_prompt_substitution_audit.py` | 36 pass |
| `tests/test_level5_pipelines.py` | unchanged |

Total: **119 tests pass** across the controller + dispatch + immutable-
target + graph-integration + CLI + audit shards.

Two tests fail in this branch and are pre-existing in the Slack-supplied
patch (commit `42de83ce4`):
`tests/test_review_cli.py::test_review_command_writes_valid_digest_bound_receipt`
and
`tests/test_parallel_codex_reviewer.py::test_controller_contract_bypasses_dynamic_prompt_and_shares_exact_bytes`.
Both are out of scope for this controller-owned PR — they document
behaviors that the Slack patch intended but never finished wiring
(verbatim-text redispatch + delegated subagent routing).

Local run evidence:

```text
./.venv/bin/python -m pytest -p no:cacheprovider \
  tests/test_immutable_target.py \
  tests/test_review_controller.py \
  tests/test_graph_controller_integration.py \
  tests/test_gate_subprocess_dispatch.py \
  tests/test_prompt_substitution_audit.py \
  tests/test_level5_pipelines.py
…
============================= 119 passed in 4.83s ============================
```

## Low-level details

### Controller request shape (`runner/review_controller.py:ReviewRequest`)

- `prompt_id` — `"controller-cold-review-v1"`
- `prompt_sha256`, `envelope_sha256` — SHA-256 over the exact bytes
- `template_sha256` — pinned to the catalog prompt
- `envelope_json` — canonical-JSON envelope (`schemas=1`, base64
  inner-text for task/diff snapshots)
- `head_sha`, `task_sha256`, `diff_sha256`, `changed_files_sha256`,
  `evidence_manifest_sha256`

### Transport (`runner/handler_dispatch.py:_build_controller_codex_transport`)

- `codex exec --json --ephemeral --skip-git-repo-check --sandbox read-only`
- prompt on stdin (`-`)
- no `--yolo`, no `--dangerously-bypass-approvals-and-sandbox`
- exits `0 / 2 / 1` propagate as `success / valid-fail / invalid`

### Per-lane isolation (`runner/handler_parallel_reviewer.py:lane_output_dir`)

```
~/.dark-factory/controller-reviews/<run_id>/<node_name>/<attempt>/<lane>/
  controller-receipt.json
  envelope.json
  prompt.txt
  reviewer.output.md
  transport.jsonl
  findings.json
```

### Immutable target (`runner/review_controller.py:validate_immutable_target`)

Rejects: symlinks, non-directories, paths into any holdout root, evidence
escapes, non-regular files, files > 1 MiB.

### Files changed (cumulative across the 5 commits)

```
.claude/skills/dark-factory/SKILL.md                       | (Slack patch)
.claude/skills/reviewer-calibration/SKILL.md               | 85 +/61 -
.claude/workflows/dark-factory.md                          | 70 +/61 -
.dark-factory/codex-spark-bin/codex                        | (local shim)
.dark-factory/controller_owned_review.dot                  | (orchestration)
.dark-factory/slice-wrapper.md                             | (slice spec)
.dark-factory/slice_wrapper.dot                            | (slice spec)
.dark-factory/slice-controller-core.md                     | (slice spec)
.dark-factory/slice_controller_core.dot                    | (slice spec)
.dark-factory/slice-controller-transport.md                 | (slice spec)
.dark-factory/slice_controller_transport.dot                | (slice spec)
.dark-factory/slice-immutable-target.md                    | (slice spec)
.dark-factory/slice_immutable_target.dot                   | (slice spec)
.dark-factory/review-immutable-target.md                   | (slice review)
.dark-factory/slice-graph-integration.md                   | (slice spec)
.dark-factory/slice_graph_integration.dot                  | (slice spec)
.dark-factory/review-graph-integration.md                  | (slice review)
bin/dark-factory                                           | 113 +/54 -
prompts/catalog/controller_cold_review_v1.md               | 74 ++++
pipelines/factory/gates.dot                                | review_contract="cold-review-v1"
pipelines/factory/level5_feature.dot                       | review_contract="cold-review-v1"
pipelines/factory/pr_gates.dot                             | review_contract="cold-review-v1"
runner/__main__.py                                         | (Slack patch)
runner/handler_dispatch.py                                 | 209 +/14 -
runner/handler_parallel_reviewer.py                        | 435 +/19 -
runner/handler_sandbox.py                                  | (untouched)
runner/handlers.py                                         | 1 +
runner/prompt_substitution_audit.py                        | (Slack patch)
runner/review_cli.py                                       | 391 +/0 -  (now routes through run_controller_review)
runner/review_controller.py                                | 766 +/54 -
tests/test_gate_subprocess_dispatch.py                     | 287 +/3 -
tests/test_graph_controller_integration.py                 | 147 ++++
tests/test_immutable_target.py                             | 196 ++++
tests/test_level5_pipelines.py                             | (Slack patch)
tests/test_parallel_codex_reviewer.py                      | (Slack patch)
tests/test_prompt_substitution_audit.py                    | (Slack patch)
tests/test_review_cli.py                                   | 227 ++++
tests/test_review_controller.py                            | 447 ++++
tests/test_slash_command_binary_contract.py                | 230 +/0 -
tests/test_stale_artifact_detector_removed.py              | (Slack patch)
```

### Coordination notes

- The `dark-factory/daemon/factory-af-tick.sh` (PID 39832 at run time)
  was unrelated auto-factory activity from a different repo and was left
  running.
- The Codex GUI session (kernel
  `c73c4e43ec494f818b4f27ac5e88ec56`, working dir
  `/Users/jleechan/projects/dark-factory`) is unrelated to this branch.
- `br list` is currently broken with `Duplicate external_ref:
  jleechanorg/worldarchitect.ai#8227` (a cross-repo leak from a prior
  auto-factory canary batch). This branch does not introduce or depend on
  that breakage.

### Local-run fallback (per ~/.claude/CLAUDE.md)

When the self-hosted CI runner stalls > 10 min past its normal runtime,
run the equivalent local suite and post a comment with command + output +
timestamp + git SHA labeled `local run — CI still pending`.