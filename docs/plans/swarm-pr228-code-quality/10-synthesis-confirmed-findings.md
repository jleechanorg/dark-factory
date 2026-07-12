# 10 — Synthesis of confirmed findings (REVISED post-verification)

## Scope re-confirmation

- Branch: `pr228` @ `658a715` (working tree includes uncommitted changes)
- Committed pr228 contribution: 2 commits (`2a383a1` fix, `658a715` test), 5 files, ~554 insertions, ~5 deletions
- Working-tree uncommitted: 11 files (3 modified, 8 untracked)
- Out of scope: the 7,218-line "deletions" reported by `git diff origin/main..HEAD --shortstat` — origin/main drift, not pr228's contribution

## Verification status (per rule 3)

| Lens | Agent | Status | Result |
|------|-------|--------|--------|
| Evidence | verify-evidence | completed | 17 of 17 Lane A + Lane B findings confirmed; **NEW F-NEW1 found**: circular import in `test_reviewer_outcome_verdict_consistency.py` |
| Severity | verify-severity | completed | F3/F8/F11 confirmed blocker cluster; F-B3 downgraded strong → nit (tests pin currently-active production code); no promotions |
| Design | verify-design | completed | F-A1 confirmed real judo; **F-A3/F-A8/F-A11 REFUTED** ("delete the override" would break `infra_failure`/`warn`/`unknown` handling); F-A12 (`StringState` subclass) downgraded — F-A1's `assert` is simpler; F-S3 (move to handler_verdict.py) confirmed |
| Cross-model (rule 12) | codex exec --yolo | completed | All 12 candidate findings CONFIRMED via direct execution + grep + `git show`; spot-check on B2 (pre-push regression) executed with `cat`/`ls`/`diff -u` against `git show HEAD:.githooks/pre-push` |

The verifier pass **refined** 4 findings, escalated 1 to blocker (F-NEW1), and refuted 1 move (F-A3 deletion). The synthesis below reflects all refinements.

## Confirmed findings (after 3-lens + cross-model)

### Blockers (must fix before merge)

#### B1 — `_enforce_outcome_verdict_consistency` is in the wrong module with an incomplete truth table
- **Lanes**: A (F3 / F8 / F11) + design-lens refutation of the "delete" move
- **Location**: `runner/handler_parallel_reviewer.py:219-254` (definition); `:333` and `:363` (call sites)
- **Independent verification**:
  - The override is real: it silently rewrites `verdict` when it disagrees with `outcome`-derived expected verdict (lines 230-244).
  - `verdict` field is reviewer-owned (LLM-emitted); `outcome` is runner-owned. ZFC §"Field Ownership" flags this as a backend-recomputed semantic decision.
  - `original_verdict` and `verdict_adjusted_for_consistency` are set but NEVER consumed anywhere outside the function (`grep -rn "outcome_to_verdict\|verdict_adjusted_for_consistency\|original_verdict" runner/` returns only the writer site).
  - Two competing canonical vocabularies exist: `_VERDICT_NORMALIZE` in `runner/handler_verdict.py:28` and the new `outcome_to_verdict` in `handler_parallel_reviewer.py:230-235`. They disagree on `warn` (`_VERDICT_NORMALIZE` maps warn → success; override maps warn → fail).
- **Design-lens refutation** (critical): the override defends a real failure mode. Production code emits at least 5 verdict values:
  1. `infra_failure` — `handler_dispatch.py:820`; read by `_is_gate_infra_failure` (`runner/handlers.py:91,107`) for Healer clustering. Deleting the override would collapse `infra_failure` to generic `fail`, losing this signal.
  2. `unknown` — `handler_audit.py:323`, `handler_special_gates.py:124/147/157/171/180`; consumed by `engine_run.py:182` as `_last_verdict` for downstream routing.
  3. `warn` — explicitly handled by `_parse_verdict` (`runner/handler_verdict.py:28-43`) and `pre_review_lint.py:15`.
  4. `pass` / `fail` — the only two values the override table covers.
- **Severity**: blocker (the override is a real defect in module + completeness, NOT a function to delete)
- **Code judo move** (corrected after refutation):
  1. **Move** `_enforce_outcome_verdict_consistency` to `runner/handler_verdict.py` next to `_VERDICT_NORMALIZE` (per F-S3).
  2. **Extend** the truth table to cover all 5 verdict values: `{"success": "pass", "failure": "fail", "error": "infra_failure" (preserve!), "partial": "fail", "warn": "pass" (preserve _VERDICT_NORMALIZE behavior)}`.
  3. **Wire** `_VERDICT_NORMALIZE` and the new `outcome_to_verdict` to share a single canonical source of verdict vocabulary.
  4. **Persist** `original_verdict` as a real consumer-visible audit field (F-S2 partial).
  Net effect: same bug-fix behavior, but the override lives in the canonical verdict module, the truth table is complete, and there's one source of truth for verdict vocabulary.

#### B2 — `pre-push` shim silently removes graph-audit + repro-artifact-guard invocations
- **Lanes**: C (F-C1 / F-C6 / F-C7 + F-S3)
- **Location**: `.githooks/pre-push` (working-tree diff)
- **Independent verification**:
  - `git ls-files .githooks/` shows three tracked files: `pre-push`, `pre-push-graph-audit.sh` (1059 bytes), `pre-push-repro-artifact-guard.sh` (1288 bytes).
  - The committed `pre-push` (at HEAD) was a shim that called `git lfs pre-push "$@"` AND both sibling scripts.
  - The working-tree `pre-push` calls `git lfs pre-push "$@"` only — the two sibling scripts are still in `.githooks/` but never invoked.
- **Cross-model spot-check (codex)**: `diff -u <(git show HEAD:.githooks/pre-push) .githooks/pre-push` confirmed the regression. CONFIRMED.
- **Severity**: blocker
- **Why it matters**: `pre-push-graph-audit.sh` mirrors `ci.yml:35` (graph structural audit G1/G2 dogfooding). `pre-push-repro-artifact-guard.sh` refuses plaintext passphrases and non-LFS-encrypted archives — security guard.
- **Remediation**: restore the chain in `pre-push` and add the LFS line alongside (option a from Lane C), OR move the graph audit + repro-guard under CI-only with a `delete-or-disable` commit that explains the relocation, OR document explicitly that the audit/guard purpose has been superseded. Operator decision required.

#### B3 — `config/daemon.toml` `[repos.*]` table is dead config with unconfirmed routing semantics
- **Lanes**: C (F-C3 + F-S4)
- **Location**: `config/daemon.toml` (working-tree diff, +12 lines)
- **Independent verification**:
  - `grep -rn 'repos\|push_remote\|deny_unknown_fields\|HashMap' daemon/src/config.rs` returns zero matches.
  - `daemon/src/config.rs` defines a flat deserialized struct (no `repos: HashMap<...>` field); `toml::from_str` silently drops unknown tables unless `#[serde(deny_unknown_fields)]` (not set).
  - The `[repos.*]` content is unread by the daemon in this branch.
- **Severity**: blocker
- **Remediation**: wire the consumer in the same PR (add `repos: HashMap<...>` to `daemon/src/config.rs` and the dispatcher in `daemon/src/dispatch.rs`), OR split the table into a sibling config with its own loader, OR drop the block. Routing semantics (`target_repo` → `ao_project` + `push_remote`) requires explicit confirmation per the global CLAUDE.md `spec-intent` rule.

#### B4 — `test_reviewer_outcome_verdict_consistency.py` is non-executable due to circular import (NEW, found by evidence-lens)
- **Location**: `tests/test_reviewer_outcome_verdict_consistency.py` (line 16: `from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency`)
- **Evidence**:
  ```
  $ .venv/bin/python -m pytest tests/test_reviewer_outcome_verdict_consistency.py --no-header -q
  ImportError: cannot import name '_parallel_reviewer' from partially initialized module
  'runner.handler_parallel_reviewer' (most likely due to a circular import)
  ```
- **Root cause**: `runner/handler_parallel_reviewer.py:19` does `import runner.handlers as _handlers_shim`, and `runner/handlers.py:161` does `from .handler_parallel_reviewer import (_parallel_reviewer,)`. When the test file imports from `runner.handler_parallel_reviewer`, the partial-init guard fires and `_parallel_reviewer` is not yet bound.
- **Why CI hides it**: pytest collection-order matters. When `runner.handlers` is loaded first by another test file, the circular dependency resolves. But file-only invocation or CI jobs that hit this file first fail.
- **Severity**: blocker (the entire test file is non-executable)
- **Fix**: lazy/local import at the call site, OR move `_parallel_reviewer` to a new `runner/handler_parallel_reviewer_top.py` that does NOT import the shim, OR break the cycle by relocating the shim into the parallel_reviewer module (not the reverse).
- **Escalates F-B1**: the parametrize judo is moot until the file can execute.

### Strong (should fix before merge)

#### S1 — Three near-duplicate git hook shims (post-checkout / post-commit / post-merge)
- **Lanes**: C (F-C1 / F-C7)
- **Location**: `.githooks/post-checkout`, `.githooks/post-commit`, `.githooks/post-merge` (all NEW, byte-identical except verb + filename in error message)
- **Code judo move**: extract `.githooks/_git-lfs-hook.sh` parameterized by verb; each hook becomes a 1-line delegation that calls the helper. Restores the shim-then-delegate pattern the committed `pre-push` had.

#### S2 — Triple-defended `dict[str, str]` contract should be enforced once at the substitution site (refined by design-lens)
- **Lanes**: A (F1 / F5 / F7 / F10) + design-lens refutation of F-A12's `StringState` subclass
- **Location**: `runner/handler_dispatch.py:721-732` (read tolerance), `runner/handler_dispatch.py:754` (write site), `runner/handler_render.py:69-74` (substitution coercion); contract declared at `runner/handler_core.py:39`
- **Independent verification**: `state: dict[str, str]` at `runner/handler_core.py:39` (verified via grep). The contract has no `__post_init__` validator, no property setter, no `__setitem__` proxy.
- **Design-lens refutation of F-A12** (critical): the suggested `class StringState(dict[str, str])` subclass is over-engineering. The codebase has 6+ call sites that use `ctx.state.get(...)` and 12+ that use `ctx.state[...]`. A subclass breaks any code that constructs `Context(state=some_dict)`. The simpler move: a single `assert isinstance(v, str)` at the substitution site.
- **Code judo move** (corrected): delete the read-site legacy-dict branch and the substitution-site coercion; keep the write-site JSON encoding. Add a single `assert isinstance(v, str)` at the substitution site as the contract enforcement. Net: -15 lines.

#### S3 — `_check_stale_artifacts` is observability noise on the only path that matters
- **Lanes**: A (F2) + B (F-S5)
- **Location**: `runner/handler_parallel_reviewer.py:60-111` (definition), `:262-276` (call site)
- **Independent verification**:
  - `grep -rn "<!-- run_id" runner/ prompts/ tests/` returns only the detector itself at `runner/handler_parallel_reviewer.py:80` — no prompt ever writes the marker.
  - `grep -rn "_stale_artifact_warnings" runner/ tests/ prompts/` returns only the writer at `runner/handler_parallel_reviewer.py:265` — zero readers.
  - The 5-minute mtime window at `:106` is the OPPOSITE of stale — anything modified in the last 5 minutes is fresh, not stale. Logic inversion.
- **Severity**: strong
- **Suggested fix**: delete the function and its call site. If staleness detection is genuinely wanted, the right home is (a) prompt discipline (have the plan/explore prompt emit `<!-- run_id: <id> -->` markers), (b) `git log -1 --format=%H -- spec.md` against `ctx.run_id` (deterministic, no magic number, no clock dependency).

### Nits (nice-to-fix, not blocking)

#### N1 — Test judo via parametrize (escalated to BLOCKER by B4)
- **Lanes**: B (F-B1)
- 11 tests in `test_reviewer_outcome_verdict_consistency.py` collapse to ~4 parametrized tests over `(outcome, input_verdict, expected_verdict, should_track_adjustment)`. Moot until B4 is fixed.
- **Severity**: blocker (via B4), strong on its own merits.

#### N2 — Reuse existing `_priority_node` helper
- **Lanes**: B (F-S1)
- `tests/test_gate_priority_queue.py:21` already defines `_priority_node`; new `test_state_dict_substitution.py` hand-rolls `Node(attrs={"backend_priority": ...})` 4 times. Hoist to conftest.

#### N3 — `failed_run_log*.txt` should be gitignored or deleted
- **Lanes**: C (F-C4 + F-S1 + F-C10)
- 1.4 MB untracked log dumps at repo root; soft infra intel disclosure (runner version, host region, BR_ASSET_SHA256). Should not be committed.

#### N4 — Hook script drops `set -euo pipefail`
- **Lanes**: C (F-C2)
- New `#!/bin/sh` hooks drop the bash `set -euo pipefail`. Either add a one-line rationale comment, OR re-establish `set -eu`.

#### N5 — Test fixture duplication (`make_context` helper)
- **Lanes**: B (F-B2.2)
- `Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")` repeats 12 times across the new tests. `conftest.py` has `make_node` but no `make_context`; would cut ~15 lines.

#### N6 — Test judo via delete (legacy-dict tests) — DOWNGRADED by severity-lens
- **Lanes**: B (F-B3)
- Originally strong (test judo via delete). Downgraded to nit: tests pin currently-active production code (`handler_dispatch.py:723-732`); they should be deleted alongside the F-S2 code change, not as a separate concern.

## Cross-lane themes

1. **Symptom-patching vs. root-cause-first.** pr228 fixes bugs at the consumer (`_substitute_placeholders` coerces non-str; `_enforce_outcome_verdict_consistency` overrides the verdict) instead of at the producer/invariant (string-only `ctx.state`; reviewer-emits-`outcome`-only). The judo move is to enforce the contract once (S2) and to move + extend the override to live in the canonical verdict module with a complete truth table (B1).

2. **Silent invocation removal in shared scripts.** pr228's working-tree `.githooks/pre-push` is a recurring regression class: a shim is shortened without comment or justification. **Pattern for /learn**: any diff that touches a multi-script shim should explicitly account for every previously-invoked sibling.

3. **Unread config + unconfirmed routing.** `[repos.*]` is dead config with no consumer in this branch AND its routing semantics was not confirmed per the spec-intent rule. **Pattern for /learn**: adding config without its consumer is a fork-bomb for future debuggers.

4. **Hidden circular dependencies.** B4 surfaces a long-standing circular import between `runner.handler_parallel_reviewer` and `runner.handlers`. The new test file's import path makes the latent defect observable. **Pattern for /learn**: pytest collection-order can mask circular-import defects; `pytest <single-file>` invocation is a more reliable smoke test.

## Rejected / downgraded (per verifier pass)

| Finding | Original severity | Verdict | Reason |
|---------|-------------------|---------|--------|
| F-A3 / F-A8 / F-A11 ("delete the override") | blocker (move-delete) | REFUTED | Production emits 5 verdict values (`infra_failure`, `unknown`, `warn`, `pass`, `fail`); the override defends a real failure mode. Correct move is relocate + extend, not delete. |
| F-A12 (`StringState` subclass) | strong | DOWNGRADED to nit | Over-engineering for a one-violation problem. F-A1's `assert isinstance(v, str)` is one line and equivalent protection. |
| F-S2 ("persist `original_verdict` for audit") | strong | DOWNGRADED to info | Real DX improvement but additive, not judo. Fold into B1's MOVE+EXTEND as a non-blocking sub-fix. |
| F-B3 ("legacy-dict tests pin transient shims") | strong | DOWNGRADED to nit | Tests pin currently-active production code. Delete them alongside the S2 production change, not as separate concern. |

## Severity tally (after verifier pass)

| Severity | Count | Findings |
|----------|-------|----------|
| blocker  | 4     | B1 (override wrong module + incomplete), B2 (pre-push regression), B3 (unread repos table), B4 (circular import — also escalates N1 to blocker) |
| strong   | 3     | S1 (hook triplication), S2 (triple-defended contract), S3 (stale-artifact noise) |
| nit      | 5     | N2-N6 |

## Recommended actions

1. **Open a follow-up bead** titled "pr228 verdict override relocation + state-contract enforcement" with a concrete proposal:
   - Move `_enforce_outcome_verdict_consistency` to `runner/handler_verdict.py` next to `_VERDICT_NORMALIZE`.
   - Extend the truth table to cover all 5 verdict values (`success→pass`, `failure→fail`, `error→infra_failure`, `partial→fail`, `warn→pass`).
   - Persist `original_verdict` as a real consumer-visible audit field.
   - Replace 3-layer `dict[str, str]` defense with write-site JSON + read-site `assert isinstance(v, str)`.
2. **Restore the pre-push chain** OR document explicitly why it was shortened (operator decision required).
3. **Either wire the `[repos.*]` consumer in the same PR, OR delete the block** (operator decision required per spec-intent).
4. **Fix the circular import** (B4) before any of the test-surface improvements can land.
5. **Extract `.githooks/_git-lfs-hook.sh`** parameterized by verb; collapse the three shims to 1-line delegations.
6. **`gitignore` or delete `failed_run_log*.txt`** (and the `branch_fail_step_*` siblings at repo root).
7. **Apply test judo / reuse fixes** (N2, N5): hoist `_priority_node` to conftest, add `make_context` helper.

## Provenance

- Lane A (sonnet): 12 candidate findings
- Lane B (sonnet): 10 candidate findings
- Lane C (sonnet): 15 candidate findings
- 3-lens verify (sonnet × 3): 1 NEW finding (F-NEW1 / B4 circular import), 1 refutation (F-A3 "delete" move), 3 downgrades (F-A12, F-S2, F-B3)
- Cross-model cold review (codex CLI 0.144.1, rule 12): all 12 candidate findings CONFIRMED via direct execution; spot-check on B2 (pre-push regression) executed with `cat`/`ls`/`diff -u` against `git show HEAD:.githooks/pre-push`; scope-creep checks clean
- Workflow: 3 mining lanes + 3 verifier agents, all sonnet; cross-model via codex CLI (different model family per rule 12)