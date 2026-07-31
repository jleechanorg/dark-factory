# Slice: graph integration — shared controller path + per-lane isolation

Wire the existing controller request/execution/artifact logic so the CLI (`runner/review_cli.py`) and the graph lane (`runner/handler_parallel_reviewer.py`) share one implementation, and so every parallel lane (primary + shadows) gets a distinct neutral cwd + output directory. Update the three hard-tier factory pipelines to opt into `review_contract="cold-review-v1"`.

## Allowed production files

- `runner/review_controller.py` — add a small helper `run_controller_review(request, *, backend, neutral_cwd, output_dir, ...)` that performs the subprocess invocation, JSONL parsing, response validation, receipt writing, and output file emission. Both the CLI and the graph handler must call it.
- `runner/review_cli.py` — replace its bespoke subprocess block with a call to the new helper.
- `runner/handler_parallel_reviewer.py` — replace its primary dispatch path with the new helper, and ensure each lane (primary + every shadow) gets its own `output_dir = neutral_cwd / lane_name` subdirectory.
- `runner/handler_dispatch.py` — only if a narrowly reusable read-only Codex transport wrapper is needed.
- `pipelines/factory/gates.dot`, `pipelines/factory/level5_feature.dot`, `pipelines/factory/pr_gates.dot` — add `review_contract="cold-review-v1"` to the reviewer nodes.

## Allowed tests

- `tests/test_graph_controller_integration.py` (new file).

## Requirements

1. **Add failing tests first.** They should fail on the current branch and pass after this slice lands.

2. **Shared helper exists.** `runner.review_controller.run_controller_review` (or a sibling module) is importable and returns a typed dataclass with the controller request, response, receipt, and per-lane output paths.

3. **CLI calls the shared helper.** `runner/review_cli.py:_run_controller_review` no longer builds its own subprocess argv; it delegates to the helper. The CLI continues to write the same six artifacts to its `--output-dir`.

4. **Graph calls the shared helper per lane.** Each lane (primary + every shadow lane) writes to a distinct `output_dir = neutral_cwd / lane_name`. A regression test constructs a primary + 2 shadows and asserts three distinct output directories exist with non-empty receipts.

5. **Three pipelines opt in.** `pipelines/factory/{gates,level5_feature,pr_gates}.dot` carry `review_contract="cold-review-v1"` on the reviewer nodes. Pre-flight (`bin/dark-factory --pipeline ... --preflight`) must still pass.

6. **No regression on the existing dispatch/audit/parallel/prompt-audit/controller-core/immutable-target tests.** All 111 existing tests must continue to pass.

## Verification

```bash
./.venv/bin/python -m pytest -p no:cacheprovider tests/test_graph_controller_integration.py
./.venv/bin/python -m pytest -p no:cacheprovider tests/test_immutable_target.py tests/test_review_controller.py tests/test_review_cli.py tests/test_gate_subprocess_dispatch.py tests/test_parallel_codex_reviewer.py tests/test_prompt_substitution_audit.py
DARK_FACTORY_HOME="$PWD" ./.venv/bin/python bin/dark-factory --pipeline pipelines/factory/gates.dot --preflight
DARK_FACTORY_HOME="$PWD" ./.venv/bin/python bin/dark-factory --pipeline pipelines/factory/level5_feature.dot --preflight
DARK_FACTORY_HOME="$PWD" ./.venv/bin/python bin/dark-factory --pipeline pipelines/factory/pr_gates.dot --preflight
```

## Existing code to reuse

- `runner/review_controller.py` — `create_review_request`, `validate_review_response`, `parse_codex_jsonl`, `validate_execution_receipts`, `validate_immutable_target`.
- `runner/handler_dispatch.py:_build_controller_codex_transport` — the safe Codex transport builder; do NOT bypass it.
- `runner/handler_sandbox.py:_holdouts_repo_path` / `_holdout_denied_paths` — for sandbox env stripping.
- `runner/review_cli.py` — the per-lane artifact layout (`prompt.txt`, `envelope.json`, `reviewer.output.md`, `transport.jsonl`, `controller-receipt.json`) is the canonical set; preserve it.

## Out of scope (this slice)

- New backends. Controller v1 remains Codex-only.
- Adjudication / `comparison.json` / `findings.json` cross-lane merge. The current per-lane receipt is the published artifact.
- Holdout denial hardening. Already in place via `handler_sandbox.py` and `validate_immutable_target`.