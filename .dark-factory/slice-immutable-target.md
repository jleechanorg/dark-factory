# Slice: immutable / read-only target validation

Implement only the immutable-target validation layer that the controller contract must enforce at request-creation and post-review time. This slice closes the trust-boundary gap that lets a target repo escape containment or substitute a symlink/hardlink for the reviewed tree.

## Allowed production files

- `runner/review_controller.py` — add `validate_immutable_target(inputs, holdout_roots=...)` and wire it into `_normalized_inputs`. Reject symlinks, non-regular files, paths that escape `inputs.workspace_path`, paths that land under any `holdout_roots` entry, and files larger than the 1 MiB input ceiling.
- `runner/handler_sandbox.py` — extend `_holdout_denied_paths()` callers ONLY if a narrowly reusable read-only helper is necessary. Do NOT add a second sandbox implementation.

## Allowed tests

- `tests/test_immutable_target.py` (new file).

## Requirements

Strict vertical-slice TDD: add failing tests first, then minimum code to pass.

1. **workspace_path must be a real, non-symlink directory**
   - Pass: a regular directory under `/tmp` resolves cleanly.
   - Fail: a path whose final component is a symlink (even to a real dir).
   - Fail: a path that does not exist.
   - Fail: a path that exists but is a regular file, not a directory.

2. **workspace_path must not land under any holdout root**
   - Pass: `/tmp/work` while holdout roots are `[/home/u/projects/dark-factory-holdouts]`.
   - Fail: `/home/u/projects/dark-factory-holdouts/scenarios/hello` (target inside holdouts).
   - Fail: a symlink that resolves into a holdout root.

3. **Evidence paths must be regular files inside workspace_path**
   - Pass: a regular file under workspace.
   - Fail: a directory passed as evidence.
   - Fail: a symlink whose target is outside workspace.
   - Fail: a symlink whose target is inside holdout roots.
   - Fail: a non-existent path.
   - Fail: a file whose size exceeds the 1 MiB ceiling.

4. **1 MiB input ceiling**
   - task_text > 1 MiB: `ReviewContractError` (already exists in `_normalized_inputs`; keep).
   - diff_text > 1 MiB: `ReviewContractError` (already exists; keep).
   - evidence file size > 1 MiB: `ReviewContractError` from `validate_immutable_target`.

5. **Reviewer cannot write the target**
   - The transport builder is already read-only via Codex `--sandbox read-only` (see `runner/handler_dispatch.py:_build_controller_codex_transport`).
   - Add a regression test that mutates a file in workspace, calls `validate_immutable_target` (post-review state), and expects the size/digest check to fail.
   - Add a regression test for an evidence symlink being swapped to point outside workspace.

6. **Head/tree/diff/evidence reverify post-review**
   - `_verify_controller_workspace` already exists in `runner/handler_parallel_reviewer.py`. Add a test that drives `_verify_controller_workspace` end-to-end and asserts that workspace mutation between request creation and verification produces `ReviewContractError`.

7. **Doc + prompt updates are NOT this slice's job.**
   - Do not modify `.claude/skills/*`, `.claude/workflows/*`, or the prompt catalog in this slice.

## Verification

```bash
./.venv/bin/python -m pytest -p no:cacheprovider tests/test_immutable_target.py
./.venv/bin/python -m pytest -p no:cacheprovider tests/test_review_controller.py
```

## Existing code to reuse

- `runner/handler_sandbox.py:_holdouts_repo_path()` and `_holdout_denied_paths()` — supply the holdout roots without duplicating.
- `runner/review_controller.py:_normalized_inputs()` — call `validate_immutable_target` from inside, after textual validation.
- `runner/handler_parallel_reviewer.py:_verify_controller_workspace()` — the post-review reverification is already wired.

## Out of scope (later slices)

- Graph-integration slice (CLI + graph share one controller request/execution/artifact impl; per-lane distinct cwd/output dir; pipeline updates).
- Docs slice (skills, workflows, prompt catalog).