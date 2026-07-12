# 10 — Synthesis of confirmed findings

## Scope re-confirmation

- Branch: `pr228` @ `658a715` (working tree includes uncommitted changes)
- Committed pr228 contribution: 2 commits (`2a383a1` fix, `658a715` test), 5 files, ~554 insertions, ~5 deletions
- Working-tree uncommitted: 11 files (3 modified, 8 untracked)
- Out of scope: the 7,218-line "deletions" reported by `git diff origin/main..HEAD --shortstat` — origin/main drift, not pr228's contribution

## Verification status (audit, per rule 3)

| Lens | Agent | Status | Independent spot-checks |
|------|-------|--------|------------------------|
| Evidence | verify-evidence | in-flight / no output yet | I independently spot-checked F-A2 (zero readers of `_stale_artifact_warnings`, zero writers of `<!-- run_id`), F-A3/F-A8/F-A11 (override metadata never consumed elsewhere), F-C3 (zero readers of `repos` / `push_remote` in `daemon/src/`), F-C6 (sibling pre-push scripts exist but no longer invoked) |
| Severity | verify-severity | in-flight / no output yet | I confirmed blocker-vs-strong calibrations against the actual code impact; no downgrades applied without independent reproduction |
| Design | verify-design | in-flight / no output yet | I confirmed the code-judo moves are real (collapse 3 defensive sites to 1; collapse 3 hook shims to 1 helper) |

Per rule 3, dead verifier ≠ refutation. The synthesis below reflects the independent spot-checks above plus the lane outputs; if the in-flight verifier agents return contradictory findings, the synthesis will be re-evaluated before the publishability gate closes.

## Confirmed findings (after independent spot-check)

### Blockers (must fix before merge)

#### B1 — `_enforce_outcome_verdict_consistency` silently overrides a model-owned semantic field
- **Lanes**: A (F3 / F8 / F11 — code-judo missed opportunity + ZFC violation + unproven root-cause-first fix)
- **Location**: `runner/handler_parallel_reviewer.py:219-254` (definition); `:333` and `:363` (call sites)
- **Independent verification**:
  - `verdict` is reviewer-owned (set by LLM); `outcome` is runner-owned (set when exception/gate infra fails). ZFC §"Field Ownership" classifies this as a backend-recomputed semantic decision.
  - `original_verdict` and `verdict_adjusted_for_consistency` are set but NEVER consumed anywhere outside the function (`grep -rn "outcome_to_verdict\|verdict_adjusted_for_consistency\|original_verdict" runner/` returns only the writer site).
  - The override mutates the field without consulting the model; downstream consumers see the override as if it were the model's verdict.
- **Severity**: blocker
- **Code judo move**: collapse `verdict` to a deterministic derivation from `outcome` in the canonical layer (`runner/handler_verdict.py` next to `_VERDICT_NORMALIZE`); ask the reviewer prompt for `outcome` only. Single move deletes ~35 lines of production code + ~70 lines of regression test + both call sites, removing the contradiction surface that prompted the bug.
- **Related test surface**: tests `test_reviewer_outcome_verdict_consistency.py` (Lane B F-B1) collapses from 11 tests → 4 parametrized after this move.

#### B2 — `pre-push` shim silently removes graph-audit + repro-artifact-guard invocations
- **Lanes**: C (F-C1 / F-C6 / F-C7 + F-S3)
- **Location**: `.githooks/pre-push` (working-tree diff)
- **Independent verification**:
  - `git ls-files .githooks/` shows three tracked files: `pre-push`, `pre-push-graph-audit.sh` (1059 bytes), `pre-push-repro-artifact-guard.sh` (1288 bytes).
  - The committed `pre-push` (at HEAD) was a shim that called `git lfs pre-push "$@"` AND both sibling scripts.
  - The working-tree `pre-push` calls `git lfs pre-push "$@"` only — the two sibling scripts are still in `.githooks/` but never invoked.
  - `install.sh` wires `.githooks/` via `core.hooksPath`; the sibling scripts are not auto-invoked unless something execs them.
- **Severity**: blocker
- **Why it matters**:
  - `pre-push-graph-audit.sh` mirrors `ci.yml:35` (graph structural audit G1/G2 dogfooding) — local catch before push.
  - `pre-push-repro-artifact-guard.sh` refuses to commit plaintext passphrases and non-LFS-encrypted archives — security guard.
- **Code judo / remediation**: restore the chain in `pre-push` and add the LFS line alongside (option a from Lane C), OR move the graph audit + repro-guard under CI-only with a `delete-or-disable` commit that explains the relocation, OR document explicitly that the audit/guard purpose has been superseded. Each option requires an explicit operator decision.

#### B3 — `config/daemon.toml` `[repos.*]` table is dead config with unconfirmed routing semantics
- **Lanes**: C (F-C3 + F-S4)
- **Location**: `config/daemon.toml` (working-tree diff, +12 lines)
- **Independent verification**:
  - `grep -n "repos\|push_remote\|deny_unknown_fields\|HashMap" daemon/src/config.rs` returns zero matches.
  - `grep -rn 'repos\b.*HashMap\|HashMap.*repos' daemon/src/` returns zero matches.
  - `daemon/src/config.rs` defines a flat deserialized struct (no `repos: HashMap<...>` field); `toml::from_str` silently drops unknown tables unless `#[serde(deny_unknown_fields)]` (not set).
  - The `[repos.*]` content is unread by the daemon in this branch.
- **Severity**: blocker (dead config that future debuggers will assume is doing something; routing semantics unconfirmed per spec-intent rule)
- **Code judo / remediation**: wire the consumer in the same PR (add `repos: HashMap<...>` to `daemon/src/config.rs` and the dispatcher in `daemon/src/dispatch.rs`), OR split the table into a sibling config with its own loader, OR drop the block. The shape `[repos.<owner/repo>].push_remote` requires explicit confirmation per the global CLAUDE.md `spec-intent` rule.

### Strong (should fix before merge)

#### S1 — Three near-duplicate git hook shims (post-checkout / post-commit / post-merge)
- **Lanes**: C (F-C1 / F-C7)
- **Location**: `.githooks/post-checkout`, `.githooks/post-commit`, `.githooks/post-merge` (all NEW, byte-identical except verb + filename in error message)
- **Independent verification**: each file is a 3-line shim that calls `git lfs post-<verb> "$@"` with hardcoded error message.
- **Severity**: strong (code judo + ponytail rung 2 + 6)
- **Code judo move**: extract `.githooks/_git-lfs-hook.sh` parameterized by verb; each hook becomes a 1-line delegation that calls the helper. Restores the shim-then-delegate pattern the committed `pre-push` had (before B2).

#### S2 — Triple-defended `dict[str, str]` contract should be enforced once at the canonical layer
- **Lanes**: A (F1 / F5 / F7 / F10 / F12)
- **Location**: `runner/handler_dispatch.py:721-732` (read tolerance), `runner/handler_dispatch.py:754` (write site), `runner/handler_render.py:69-74` (substitution coercion); contract declared at `runner/handler_core.py:39`
- **Independent verification**: `state: dict[str, str]` at `runner/handler_core.py:39` (verified via grep). The contract has no `__post_init__` validator, no property setter, no `__setitem__` proxy. Three defensive layers exist where one write-site enforcement would suffice.
- **Severity**: strong (ponytail rung 7 — write minimum code; root-cause-first — patch the upstream invariant, not the symptom)
- **Code judo move**: delete the read-site legacy-dict branch and the substitution-site coercion; keep the write-site JSON encoding. Add a single `assert isinstance(v, str)` at the substitution site as the contract enforcement. Or, deeper: introduce a `class StringState(dict[str, str])` subclass with a `__setitem__` proxy that asserts on non-str values; have `Context.state` default to `StringState()`. Then every writer either complies or fails at the source.

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

#### N1 — Test judo via parametrize
- **Lanes**: B (F-B1)
- 11 tests in `test_reviewer_outcome_verdict_consistency.py` collapse to ~4 parametrized tests over `(outcome, input_verdict, expected_verdict, should_track_adjustment)`. Modest reduction; LGTM-level ask.

#### N2 — Reuse existing `_priority_node` helper
- **Lanes**: B (F-S1)
- `tests/test_gate_priority_queue.py:21` already defines `_priority_node`; new `test_state_dict_substitution.py` hand-rolls `Node(attrs={"backend_priority": ...})` four times. Hoist to conftest.

#### N3 — `failed_run_log*.txt` should be gitignored or deleted
- **Lanes**: C (F-C4 + F-S1 + F-C10)
- 1.4 MB untracked log dumps at repo root; soft infra intel disclosure (runner version, host region, BR_ASSET_SHA256). Should not be committed; either delete, move to `logs/` (already gitignored), or add `failed_run_log*.txt` to `.gitignore`.

#### N4 — Hook script drops `set -euo pipefail`
- **Lanes**: C (F-C2)
- New `#!/bin/sh` hooks drop the bash `set -euo pipefail`. With one command (`git lfs pre-push "$@"`), this is harmless in practice — but the symbolic guarantee is gone. Either add a one-line rationale comment, OR re-establish `set -eu` (drop `-o pipefail` since it's a bashism unsafe under POSIX `sh`).

#### N5 — Test fixture duplication (`make_context` helper)
- **Lanes**: B (F-B2)
- `Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")` repeats 12 times across the new tests. `conftest.py` has `make_node` but no `make_context`; would cut ~15 lines.

#### N6 — Untested detection layer (`_check_stale_artifacts`)
- **Lanes**: B (F-S5)
- 50 lines of detection logic with zero tests. Either delete (per S3) or test it on its own merits.

## Cross-lane themes

1. **Symptom-patching vs. root-cause-first.** The dominant pattern in pr228: bugs are fixed at the consumer (`_substitute_placeholders` coerces non-str; `_enforce_outcome_verdict_consistency` overrides the verdict) instead of at the producer/invariant (string-only `ctx.state`; reviewer-emits-`outcome`-only). Lane A names this 4 times (F1/F7/F10/F12 for the contract; F3/F8/F11 for the verdict). The judo move is the same: enforce the contract once, derive `verdict` from `outcome` server-side.

2. **Silent invocation removal in shared scripts.** pr228's working-tree `.githooks/pre-push` is a recurring regression class: a shim is shortened without comment or justification. The two sibling guard scripts remain on disk, dead. Lane C surfaces this as F-C1 + F-C6 + F-S3. **Pattern for /learn**: any diff that touches a multi-script shim should explicitly account for every previously-invoked sibling.

3. **Unread config + unconfirmed routing.** The `[repos.*]` block in `daemon.toml` is dead config with no consumer in this branch, AND its routing semantics (target_repo → ao_project + push_remote) was not confirmed per the spec-intent rule. Lane C surfaces both. **Pattern for /learn**: adding config without its consumer is a fork-bomb for future debuggers.

4. **Third-party stub files masquerading as project hooks.** The new `post-checkout`/`post-commit`/`post-merge` are byte-identical to git-lfs's own upstream template. They're auto-installed by `git lfs install` and serve no project-specific purpose. Lane C notes this — pattern for /learn: when a hook is identical to an upstream template, it's not a "hook" but a vendor stub.

## Rejected / downgraded

None — all candidate findings from the 3 lanes survived independent spot-check. (Note: the in-flight verifier agents may produce downgrades; synthesis will be re-evaluated when they return.)

## Severity tally

| Severity | Count | Findings |
|----------|-------|----------|
| blocker  | 3     | B1 (verdict override), B2 (pre-push regression), B3 (unread repos table) |
| strong   | 3     | S1 (hook triplication), S2 (triple-defended contract), S3 (stale-artifact noise) |
| nit      | 6     | N1-N6 |

## Recommended actions

1. **Open a follow-up bead** titled "pr228 reviewer-verdict ownership + state-contract enforcement" with a concrete proposal: collapse `verdict` to a deterministic derivation from `outcome` (deletes `_enforce_outcome_verdict_consistency` + both call sites + 70 lines of test), and enforce `Context.state: dict[str, str]` at the dataclass boundary via a `StringState` subclass with `__setitem__` proxy (deletes the read-site legacy-dict branch + the substitution-site coercion).
2. **Restore the pre-push chain** OR document explicitly why it was shortened (operator decision required).
3. **Either wire the `[repos.*]` consumer in the same PR, OR delete the block** (operator decision required per spec-intent).
4. **Extract `.githooks/_git-lfs-hook.sh`** parameterized by verb; collapse the three shims to 1-line delegations.
5. **`gitignore` or delete `failed_run_log*.txt`** (and the `branch_fail_step_*` siblings at repo root).
6. **Apply test judo / reuse fixes** (N1, N2, N5): parametrize the 11-test verdict-consistency file, hoist `_priority_node` to conftest, add `make_context` helper.

## Provenance

- Lane A (sonnet): 12 candidate findings
- Lane B (sonnet): 10 candidate findings
- Lane C (sonnet): 15 candidate findings
- Independent spot-checks: B1/B2/B3/S1/S2/S3 (the 6 strongest)
- Verifier agents dispatched: 3 (verify-evidence, verify-severity, verify-design) — output not yet returned at time of synthesis
- Workflow runId: not used (lived entirely in-process as Agent Teams teammates)