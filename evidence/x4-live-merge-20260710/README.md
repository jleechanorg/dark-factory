# X4 live evidence — first zero-touch merge through the production merge function

**Date:** 2026-07-10 (PDT) · **Bead:** jleechan-vbbi · **Criterion:** [X4](../../docs/cutover-exit-criteria.md) (merge authority)

This bundle captures the **first observed live zero-touch merge** performed by the
production merge function `daemon/scripts/auto-merge-guard.sh`, together with the
concurrent refusals of other open `factory/*` PRs in the **same** guard pass. It is a
**passive live observation** of the operator-approved **Option A** path (an
externally-scheduled guard on a systemd timer), not a reviewer-executed X4 sign-off.
See "Scope / what this does NOT prove" below before citing it as X4 PASS.

## Production merge function under test

- File: `daemon/scripts/auto-merge-guard.sh`
- sha256: see [`auto-merge-guard.sh.sha256`](./auto-merge-guard.sh.sha256)
  (`263e9ce39e0193ae1c1910043ef3e63a376f66ead068e53d3bc4b38c74296604`)
- Caller (Option A): systemd user timer `dark-factory-merge-guard.timer`, 60s period,
  active since 2026-07-10 12:39:22 PDT — see [`systemd-timer-status.txt`](./systemd-timer-status.txt).
  This is the externally-scheduled guard the spec §4.2.8-vs-X4 contradiction was resolved
  toward: the Rust daemon itself never merges (`verifier.rs`: "never … merges"); merge
  authority lives in this one policy script, run externally on a timer.

## Green control — PR #228 MERGED (real work item)

The PR that actually merged was **#228** (`factory/ez-gh-actions-mw5a-r2`), a real work
item — not the green-fixture PR #207. Raw: [`gh-pr228-merged.json`](./gh-pr228-merged.json).

- `merged: true`, `merged_at: 2026-07-10T20:48:04Z` (= 13:48:04 PDT),
  `merge_commit_sha: f59b488808487d49ac6d3571478222fe73f95531`.
- `merged_by: jleechan2015` — this is the **GitHub identity the externally-scheduled guard
  authenticates as** (the guard shells `gh pr merge --squash`, line 86), NOT a human
  hand-merge. Corroboration it was the guard and not a manual merge:
  1. The guard's own success line in the log:
     `PR 228 MERGED, bead ez-gh-actions-mw5a closed+READY`
     (see [`merge-guard-pr228-sequence.log`](./merge-guard-pr228-sequence.log), last line).
  2. The rate-limit ledger `~/.dark-factory/merge-timestamps` — which **only** the guard
     writes (line 89, on merge success) — contains epoch `1783716480` =
     `Fri Jul 10 01:48:00 PM PDT 2026`, matching `merged_at` to the minute
     (see [`merge-timestamps.txt`](./merge-timestamps.txt)).

Full lifecycle in [`merge-guard-pr228-sequence.log`](./merge-guard-pr228-sequence.log)
(33 ticks): `CI pending — skip` ×N → `verifier assessment missing — refusing merge
(green CI is insufficient)` ×18 → `assessment no-fail (all gates cleared)` →
`not MERGEABLE (conflicts) — skip` (one transient tick) → `assessment no-fail` →
`gates red-free + mergeable — merging` → `PR 228 MERGED`. This shows the guard held the
merge until a daemon-native GATE_ASSESSMENT existed **and** the PR was mergeable — the
"green CI is insufficient" policy in action.

## Refusal control — PRs 205 / 207 / 208 stayed OPEN through the same pass

Raw current state: [`gh-pr205-207-208-open.jsonl`](./gh-pr205-207-208-open.jsonl) — all
three `state: open`, `merged: false`. Refusal tallies over the same live run
([`merge-guard-refusals-205-207-208.log`](./merge-guard-refusals-205-207-208.log)):

| PR | head ref | dominant refusal reason | count |
|----|----------|-------------------------|------:|
| 205 | `factory/jleechan-bpb6-r2` | `verifier assessment missing — refusing merge (green CI is insufficient)` | 2534 |
| 208 | `factory/jleechan-s3c-smoke-red-r1` | `verifier assessment missing — refusing merge (green CI is insufficient)` | 2569 |
| 207 | `factory/jleechan-s3c-smoke-green-r1` | `CI FAILED — skip (needs attention)` | 1949 |

PR 205 is notable: `mergeable_state: clean` (green CI) yet **refused every tick** because
no GATE_ASSESSMENT exists — the guard concretely declined a green-CI PR. That is the
"green-CI-is-insufficient" refusal branch working end-to-end, in the same function that
merged #228.

## Assessment source (do not overclaim)

The guard's assessment gate reads the **latest `GATE_ASSESSMENT` event** from the daemon
JSONL log (`auto-merge-guard.sh` `latest_assessment_no_red()`, lines 37–72). Per the
charter's R2, this is **implementer-authored telemetry** and is corroborating only. The
external anchors in this bundle are the GitHub PR state (`gh api`) and the git merge commit
SHA — those, not the JSONL, are what prove #228 merged and 205/207/208 did not.

## Scope / what this does NOT prove (X4 is NOT signed off by this bundle)

This is a live observation of the green-merge path and the refusal path through the
identical production function. It does **not** satisfy full X4, which additionally requires
a **reviewer-executed** run demonstrating:

- **Strict red control:** a PR carrying a GATE_ASSESSMENT with **≥1 gate = `red`/`fail`**,
  the guard refusing while **citing the gate name**. The refusals observed here are the
  *assessment-missing* branch (line 81) and the *CI-failed* branch (line 80) — adjacent
  policy branches, **not** the gate=red-citation branch.
- **Mislabel control** (`red` recorded as `unknown` must be caught).
- **TOCTOU** head-SHA binding (merge must fail if head moved between assessment and merge).
- **`safe-push-main.sh`** rebase+verify semantics under a mid-run `origin/main` advance.

Those remain to be exercised in a reviewer-personal run (charter R4) before X4 can be
marked PASS. This bundle documents the **first live green merge + live refusals** of the
production merge function under Option A — a milestone toward X4, not the sign-off.

## Reproduce

```bash
LOG=~/Library/Logs/dark-factory/merge-guard.out.log
grep -n "PR 228" "$LOG"                                   # green-control lifecycle
grep -E "PR (205|207|208):" "$LOG" | sort | uniq -c       # refusal tallies
env -u GITHUB_TOKEN -u GH_TOKEN gh api repos/jleechanorg/dark-factory/pulls/228 \
  --jq '{merged, merged_at, merged_by:.merged_by.login}'  # external anchor
cat ~/.dark-factory/merge-timestamps                      # guard-only rate ledger
systemctl --user status dark-factory-merge-guard.timer    # Option A caller
```
