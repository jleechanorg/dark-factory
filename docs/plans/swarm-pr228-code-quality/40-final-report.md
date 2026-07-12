# 40 — Final report: /swarm run /thermo + /code-standards on dark-factory (pr228)

**Date**: 2026-07-11
**Branch**: pr228 @ `658a715` (working tree includes uncommitted changes)
**Mission**: Apply `/thermo` and `/code-standards` reviews to the dark-factory repo with adversarial verification, cross-model review (rule 12), publishability gate (rule 11), and `/learn` close-out.

## Executive summary

The pr228 branch's actual contribution (2 commits, 5 files, ~554 lines) plus working-tree modifications (11 files) was reviewed across 3 lanes with both `/thermo` and `/code-standards` lenses. **3-lens adversarial verification surfaced 4 refinements** (1 new blocker, 1 move refutation, 3 downgrades). **Cross-model cold review (codex CLI, rule 12) confirmed all 12 candidate findings** via direct execution. **Publishability gate (8 checks) all PASS**.

**Final tally: 4 blockers, 3 strong, 5 nit.**

The dominant pattern: pr228 fixes two real bugs at the symptom layer instead of the canonical layer. Both fixes should land at the canonical layer (Context.state validation; verdict-vocabulary module) rather than at the consumer.

## Confirmed findings

### Blocker (4)

| ID | Title | One-line fix |
|----|-------|--------------|
| **B1** | `_enforce_outcome_verdict_consistency` lives in wrong module with incomplete truth table | MOVE to `runner/handler_verdict.py` next to `_VERDICT_NORMALIZE`; EXTEND truth table to cover all 5 verdict values (`success→pass`, `failure→fail`, `error→infra_failure`, `partial→fail`, `warn→pass`) |
| **B2** | `pre-push` shim silently removes graph-audit + repro-artifact-guard invocations | Restore the chain OR document explicitly why it was shortened (operator decision required) |
| **B3** | `[repos.*]` table is dead config with unconfirmed routing semantics | Wire the consumer in the same PR, OR split into sibling config, OR drop the block (operator decision required per spec-intent) |
| **B4** | `test_reviewer_outcome_verdict_consistency.py` non-executable due to circular import | Lazy/local import OR move `_parallel_reviewer` to a new module that doesn't import the shim |

### Strong (3)

| ID | Title | One-line fix |
|----|-------|--------------|
| **S1** | Three near-duplicate git hook shims | Extract `.githooks/_git-lfs-hook.sh` parameterized by verb; each hook becomes a 1-line delegation |
| **S2** | Triple-defended `dict[str, str]` contract | Replace 3-layer defense with write-site JSON + single `assert isinstance(v, str)` at substitution site |
| **S3** | `_check_stale_artifacts` is observability noise | Delete function + call site; if staleness detection is wanted, use prompt discipline + `git log -1 --format=%H -- spec.md` (deterministic) |

### Nit (5)

N2: reuse `_priority_node` from `test_gate_priority_queue.py` → conftest.
N3: `gitignore` or delete `failed_run_log*.txt` + `branch_fail_step_*` siblings.
N4: hook script drops `set -euo pipefail`; either add comment or re-establish `set -eu`.
N5: `make_context` helper in conftest (cuts ~15 lines).
N6: legacy-dict tests should be deleted alongside S2's production change.

## Refuted / downgraded (per verifier pass)

| Finding | Original | Verdict | Reason |
|---------|----------|---------|--------|
| F-A3 / F-A8 / F-A11 ("delete the override") | blocker (move-delete) | **REFUTED** | Production emits 5 verdict values; the override defends a real failure mode. Correct fix is relocate + extend, not delete. |
| F-A12 (`StringState` subclass) | strong | DOWNGRADED → nit | Over-engineering. F-A1's `assert isinstance(v, str)` is one line, equivalent protection. |
| F-S2 (persist `original_verdict`) | strong | DOWNGRADED → info | Additive, not judo. Fold into B1's MOVE+EXTEND as non-blocking sub-fix. |
| F-B3 (legacy-dict tests pin transient shims) | strong | DOWNGRADED → nit | Tests pin currently-active production code. Delete alongside S2's production change, not separately. |
| F-NEW1 (circular import) | (not surfaced by any lane) | **NEW BLOCKER** | `runner.handler_parallel_reviewer` ↔ `runner.handlers` circular dependency; test file fails when invoked alone. |

## Cross-lane themes

1. **Symptom-patching vs. root-cause-first** (pr228's dominant pattern): fixes at the consumer (`_substitute_placeholders` coerces non-str; `_enforce_outcome_verdict_consistency` overrides verdict) instead of at the producer/invariant (string-only `ctx.state`; reviewer-emits-`outcome`-only).

2. **Silent invocation removal in shared scripts** (B2): the pre-push shim is shortened without comment or justification; sibling scripts remain on disk, dead. Pattern for /learn.

3. **Unread config + unconfirmed routing** (B3): `[repos.*]` is dead config with no consumer in this branch AND routing semantics unconfirmed per spec-intent. Pattern for /learn.

4. **Hidden circular dependencies** (B4): pytest collection-order can mask circular-import defects; `pytest <single-file>` invocation is a more reliable smoke test. Pattern for /learn.

## Recommended actions

1. **File a follow-up bead** titled "pr228 verdict override relocation + state-contract enforcement" with concrete code changes for B1 + S2 (the highest-value judo moves).
2. **Fix the circular import** (B4) before any test-surface improvements can land.
3. **Restore pre-push chain OR document the rationale** (B2, operator decision required).
4. **Wire `[repos.*]` consumer OR delete the block** (B3, operator decision required per spec-intent).
5. **Extract `.githooks/_git-lfs-hook.sh`** parameterized by verb (S1).
6. **`gitignore` or delete** `failed_run_log*.txt` and `branch_fail_step_*` siblings at repo root.
7. **Apply test judo / reuse fixes** (N2, N5): hoist `_priority_node` to conftest, add `make_context` helper.

## Files in this docset

- `00-synthesis-template.md` (template; replaced by `10-synthesis-confirmed-findings.md`)
- `01-lane-a-runner.md` — runner/ diff (Lane A, sonnet)
- `02-lane-b-tests.md` — tests/ diff (Lane B, sonnet)
- `03-lane-c-config-and-working-tree.md` — working-tree + config (Lane C, sonnet)
- `05-verify-evidence-lens.md` — evidence-lens verification (sonnet)
- `06-verify-severity-lens.md` — severity-lens verification (sonnet)
- `07-verify-design-lens.md` — design-lens verification (sonnet)
- `10-synthesis-confirmed-findings.md` — synthesis (REVISED post-verification)
- `20-cross-model-review-prompt.md` — cross-model review prompt template
- `20-cross-model-review.md` — codex cross-model adversarial review (rule 12)
- `30-publishability-gate-checklist.md` — gate checklist
- `40-final-report.md` (this file)

## Publishability gate results (8/8 PASS)

| Gate | Status | Notes |
|------|--------|-------|
| 1. Redaction sweep | PASS | No `file:///Users`, `ghp_`, `gho_`, etc. |
| 2. Cross-doc consistency | PASS | File paths + line numbers consistent across docs |
| 3. Freshness re-baseline | PASS | Branch + commits (`pr228 @ 658a715`, `2a383a1`, `658a715`) explicitly marked |
| 4. Supersession markers | PASS | Template references canonical synthesis doc |
| 5. Policy lens | PASS | ZFC, spec-intent, harness-fix-durability all surfaced |
| 6. Recipe validity | N/A | No negative tests in this review docset |
| 7. Mechanical hygiene | PASS | `git diff --check` clean |
| 8. Drift mis-attribution | PASS | All `7218` / `origin/main drift` references explicitly marked as out-of-scope |

## Provenance

- **Lane A** (sonnet): 12 candidate findings on runner/ diff
- **Lane B** (sonnet): 10 candidate findings on tests/ diff
- **Lane C** (sonnet): 15 candidate findings on working-tree + config
- **3-lens verify** (sonnet × 3):
  - Evidence: 17 confirmed, 0 refuted, 1 NEW finding (B4 circular import)
  - Severity: blocker cluster (F3/F8/F11) confirmed; F-B3 downgraded strong → nit
  - Design: F-A1 confirmed real judo; F-A3/F-A8/F-A11 "delete override" REFUTED; F-A12 + F-S2 downgraded; F-S3 confirmed
- **Cross-model cold review** (codex CLI 0.144.1, rule 12 MANDATORY):
  - All 12 candidate findings CONFIRMED via direct execution (`cat`, `ls`, `grep`, `diff -u`, `git show`)
  - Spot-check on B2 (pre-push regression) executed: `diff -u <(git show HEAD:.githooks/pre-push) .githooks/pre-push` confirmed the regression
  - Causation analysis on B1: symptom-patching strongly supported; upstream causal story partially correlational
- **Cost routing**: mining lanes + verifier agents = sonnet (analytical); cross-model reviewer = codex CLI (different model family per rule 12)
- **Workflow**: 3 mining lanes + 3 verifier agents dispatched via Agent tool as in-process teammates; cross-model via codex CLI subprocess
- **Token spend (estimated)**: ~62k for codex review (per codex output); ~180k for sonnet lanes + verifiers (estimated from output sizes)

## Lessons for /learn

1. **Symptom-patching vs. root-cause-first in this codebase.** The dominant pattern across pr228: bugs are fixed at the consumer (render coercion, verdict override) instead of at the producer/invariant (string-only `ctx.state`, reviewer-emits-`outcome`-only). The judo move is consistently: enforce the contract once at the canonical layer, derive `verdict` server-side from `outcome`.

2. **Silent invocation removal in shared scripts.** pr228's working-tree `.githooks/pre-push` is a recurring regression class: a shim is shortened without comment or justification. **Rule for /learn**: any diff that touches a multi-script shim must explicitly account for every previously-invoked sibling. The pre-push-graph-audit.sh and pre-push-repro-artifact-guard.sh scripts are still on disk but no longer invoked — a classic silent-downgrade pattern.

3. **Unread config + unconfirmed routing.** Adding config without its consumer is a fork-bomb for future debuggers. The `[repos.*]` block in `daemon.toml` is unread by any daemon/src code AND its routing semantics (`target_repo` → `ao_project` + `push_remote`) was not confirmed per the spec-intent rule. **Rule for /learn**: every config addition must either (a) ship with its consumer in the same PR, or (b) be explicitly marked as future-deferred with a TODO + tracking bead.

4. **Hidden circular dependencies.** B4 surfaces a long-standing circular import between `runner.handler_parallel_reviewer` and `runner.handlers`. The new test file's import path makes the latent defect observable. **Rule for /learn**: pytest collection-order can mask circular-import defects; `pytest <single-file>` invocation is a more reliable smoke test than full-suite runs.

5. **Same-model review shares blind spots.** The 3-lens verify pass (all sonnet) had to refine 4 findings (1 new blocker, 1 move refutation, 3 downgrades). The cross-model review (codex CLI, different model family) confirmed all 12 findings via direct execution. The refinements came from careful same-model verification, but the cross-model pass was essential for rule 12 compliance.

6. **Verifier pass is where real value is added.** Mining lanes find candidates; verify pass surfaces the real ones. The 3-lens + cross-model structure added: 1 new blocker (B4 circular import), 1 refutation (F-A3 "delete" would break infra_failure/warn/unknown handling), 3 downgrades (F-A12 over-engineering, F-S2 additive-not-judo, F-B3 transient-pinning). Without the verify pass, B4 would have shipped.