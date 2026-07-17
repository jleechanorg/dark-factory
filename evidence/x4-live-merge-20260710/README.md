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

## Independent third-party witness — journald (closes the circularity gap)

The corroboration above (the guard's own log line, and the guard-only
`~/.dark-factory/merge-timestamps` ledger) has a gap: both are **written by the
script under test**. Neither can distinguish "the guard's `gh pr merge` call
executed" from "a human ran `gh pr merge` by hand while authenticated as the
same `jleechan2015` GitHub identity the guard uses" — `merged_by` in the GitHub
API response is the account, not the calling process.

To close that gap, this bundle adds a witness that is **not written by
`auto-merge-guard.sh` at all**: `systemd`/`journald`, the OS-level process
supervisor that starts and stops the service unit. journald's timestamps and
unit attribution (`systemd[3285]: Starting/Finished dark-factory-merge-guard.service`)
come from the systemd user manager itself, independent of anything the script
prints or writes.

- [`journald-merge-guard-20260710-1340-1355.log`](./journald-merge-guard-20260710-1340-1355.log) —
  raw output of
  `journalctl --user -u dark-factory-merge-guard.service --since "2026-07-10 13:40" --until "2026-07-10 13:55" -o short-iso`,
  captured 2026-07-10. It shows the timer firing the service **every ~60s** through
  the merge window, with one tick starting `2026-07-10T13:48:00-07:00` and finishing
  `2026-07-10T13:48:11-07:00` — bracketing `merged_at: 2026-07-10T20:48:04Z`
  (= `13:48:04 PDT`) to the second. This is systemd's own record that the unit
  executed at exactly the time PR #228 merged; it is orthogonal proof to the
  guard's self-reported log and the `merge-timestamps` ledger.
- [`systemd-exec-main-start-timestamp.txt`](./systemd-exec-main-start-timestamp.txt) —
  output of `systemctl --user show dark-factory-merge-guard.service -p ExecMainStartTimestamp`
  (plus a few adjacent properties), captured ~19:43 PDT the same day. **Scope note:**
  this command only reports the unit's most recent invocation (systemd `show` does
  not retain per-tick history — journald does), so it does not itself date-stamp
  the 13:48 merge; its role here is narrower — it confirms the unit is a live,
  systemd-supervised service with `Result=success`, corroborating that the
  journald "Starting/Finished" lines above come from a real, currently-running
  systemd unit rather than a stale or removed one. The per-tick historical proof
  for the merge window is the journald log, not this file.

**Why this closes the circularity gap:** every other artifact in this bundle
(the guard's log, the rate-limit ledger, even the GitHub `merged_by` field) is
either self-reported by the script under test or silent on *who/what* invoked
`gh pr merge`. journald is written by systemd — a separate, independent OS
component that faithfully logs unit start/stop regardless of what the guard
script itself does or claims. It cannot be spoofed by the guard script writing
a flattering log line, and it is not the human operator's own account activity
(it is a record of process supervision, not of `gh` CLI invocations by a human).
Combined with the guard's log and the merge-timestamps ledger, this triangulates
on: the guard process ran at 13:48:00–13:48:11 PDT (journald, independent) → the
guard's own log shows `PR 228 MERGED` in that same run (self-reported, but now
time-anchored by an independent witness) → GitHub shows PR #228 merged at
13:48:04 PDT (external, independent). No single artifact here proves it alone;
the combination is what rules out a coincidental hand-merge landing in the same
60-second window as an unrelated timer tick.

**What this still does NOT prove:** journald confirms the *service ran* in that
window; it does not itself inspect the process's stdout to show `gh pr merge`
was invoked (that remains the guard's own log, `merge-guard-pr228-sequence.log`).
The claim being defended is narrower than "journald proves the merge command
ran" — it is "journald proves the guard's *process* was executing at the exact
minute the merge happened, independently of the guard's own self-reporting,"
which is what rules out the "human merged manually, guard was idle" alternative
explanation.

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

# Independent third-party witness (systemd/journald — not written by the guard script):
journalctl --user -u dark-factory-merge-guard.service \
  --since "2026-07-10 13:40" --until "2026-07-10 13:55" -o short-iso
systemctl --user show dark-factory-merge-guard.service -p ExecMainStartTimestamp
```
