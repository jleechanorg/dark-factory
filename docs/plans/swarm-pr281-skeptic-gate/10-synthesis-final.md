# E6 /swarm review — PR #281 skeptic gate — synthesis (FINAL)

- **PR**: https://github.com/jleechanorg/dark-factory/pull/281 — "[antig] feat(ci): add SHA-bound skeptic gate workflow for 7-green"
- **Head reviewed**: `e70b1ec6` (+4577/−0, 5 files)
- **Date**: 2026-07-15
- **Pipeline**: 3 mining lanes (sonnet) → 3-lens adversarial verify (sonnet, refute-by-default) → cross-model spot-check (codex CLI + main-session reproduction) → publishability gate
- **Goal criterion**: E6 of `~/roadmap/goals/2026-07-13-factory-work-continuation.md` (tracking bead `jleechan-goal-factory-continuation-2026-07-13-bsjh.1`, GH #292)

## Verdict: NOT MERGE-READY — 2 hard mechanical blockers + 1 automation-completeness High

The gate's *security design* is sound (SHA-bound verdict, fail-closed commit status), but the workflow **cannot execute as shipped**, and even if it could, **nothing calls it**.

## Confirmed findings (11 confirmed, 2 severity-downgraded, 0 refuted)

### Blockers (must fix before merge)

| ID | Finding | Evidence |
|----|---------|----------|
| **C-F1** | `runs-on: ${{ vars.SELF_HOSTED_RUNNER_LABELS }}` (workflow line 118) interpolates a JSON-array *string* (documented example `'["self-hosted","self-hosted-mikey"]'`) without `fromJson()` — GitHub treats it as one literal label no runner has; the job can never be scheduled | Independently reproduced by verify agent AND main session (`git show pr281-review:.github/workflows/skeptic-gate.yml \| grep -n runs-on`) |
| **C-F2** | `$SELF_HOSTED_RUNNER_LABELS` referenced in a bash step under `set -u` but never exported in the job `env:` block — unbound-variable crash on the first step that references it | Reproduced line-accurately by verify agent |

### High

| ID | Finding | Evidence |
|----|---------|----------|
| **C-F3** | Trigger posture is `workflow_call` + `workflow_dispatch` only, and **no caller workflow exists in the repo**. Codex cross-model refinement: a human CAN fire it manually via `workflow_dispatch`, so "can never fire" was too strong — but manual-only invocation is precisely what the automation-completeness rule forbids ("a script with only a manual invocation path is not automation"); the gate cannot fire automatically on the 7-green path it claims to protect | Confirmed via grep across all workflow files + beads store; refined by codex CHECK3 |

### Strong

| ID | Finding |
|----|---------|
| **A-F1** (downgraded from blocker) | `_publish_failure` posts diagnostics to issue #0 on two early-exit paths (`_pr_number_for_desc` regexes `"PR #(\d+)"` from a description that never contains it, returns 0). Merge protection is NOT compromised (commit status flips to failure unconditionally, separate try/except) — only the diagnostic PR comment silently vanishes. Fix: thread `args.pr_number` through; delete `_pr_number_for_desc`. |
| **A-F3** | Sequential reviewer timeout budget: reviewers run serially and each gets the full budget; worst case multiplies wall-clock. |
| **B-F2** (moderate) | GitHub-API coverage asymmetry: zero tests exercise `_publish_failure` / API-error paths. |

### Nits

A-F2 (redundant `_SHA_RE`/`_FULL_SHA_RE` pair — dead weight, 40-hex gate subsumes it; downgraded from strong), A-F4 (value-equality list mutation), A-F5 (file approaching 1k-line boundary), B-F1 (dead ternary with typo'd attribute), B-F3 (7× test boilerplate → parametrize).

## Positive findings

- SHA-binding design is correct: verdict parses `HEAD_SHA:` and hard-rejects mismatches (`re.fullmatch(r"[0-9a-f]{40}")` + single-match gates) — replaying a stale verdict on a new head fails closed.
- Commit-status fail-closed guarantee verified independently: no path produces a false PASS.
- Verdict vocabulary aligns with the canonical 7-gate keys (skeptic gate emits into the established contract).

## Cross-model / cross-lens provenance

- 3 sonnet mining lanes: 12 candidate findings
- Sonnet 3-lens verify (refute-by-default): 11 confirmed, 2 downgraded (A-F1 blocker→strong, A-F2 strong→nit), 0 refuted — every citation reproduced against live `pr281-review` source
- Rule-12 cross-model (codex CLI 0.144.1): CHECK1 (runs-on fromJson) CONFIRMED, CHECK2 (unbound env var) CONFIRMED, CHECK3 refined ("never fires" → "cannot fire automatically; manual dispatch works but violates automation-completeness"). Main session independently reproduced C-F1 + trigger block.
- Publishability gate: see `30-publishability-gate.md`

## Recommended actions for the PR author

1. `runs-on: ${{ fromJson(vars.SELF_HOSTED_RUNNER_LABELS) }}` (C-F1).
2. Export `SELF_HOSTED_RUNNER_LABELS` in the job `env:` block or drop the bash reference (C-F2).
3. Add the caller: a `pull_request`-triggered thin workflow that `uses:` this one with the trusted SHA, or wire it into the existing 7-green pipeline (C-F3) — otherwise the gate is documentation, not automation.
4. Thread `pr_number` through `_publish_failure` (A-F1).
5. Optional cleanups: A-F2/A-F4/B-F1/B-F3.