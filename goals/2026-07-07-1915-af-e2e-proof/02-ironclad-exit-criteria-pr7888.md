# Ironclad exit criteria — PR #7888 driven to green via /af

Written 2026-07-10T~01:15 UTC per operator /goal directive: "keep iterating and fixing
factory as needed until it works and define your own ironclad exit criteria and follow
it." Scoped specifically to jleechanorg/worldarchitect.ai#7888 (bead jleechan-93ft), the
live test case for tonight's `/af` end-to-end proof. Supersedes nothing in
`01-success-criteria.md` (that doc's C1-C5 already closed or superseded by later waves)
— this is a new, narrower, currently-active criteria set.

**Ground rule, learned the hard way tonight**: every criterion below requires
independently-observed evidence from a source OTHER than an agent's self-report or a
status label already known to be unreliable tonight (AO's activity/status field lied
about wa-3084 being "killed" when it was alive; the daemon's own telemetry lied about
`EXISTING_PR_ADOPTED/ATTESTED` for an actually-HUMAN_HELD bead). "An agent said X" is not
evidence of X. Verification means: read the raw file, query the raw API, check the raw
process table, diff against a known-good baseline.

## E1 — Real code fix pushed to the real PR branch — ✅ CONFIRMED 2026-07-10T~01:20 UTC

- [x] `gh api repos/jleechanorg/worldarchitect.ai/pulls/7888 --jq .head.sha` = `8c678397d06...`.
      `git merge-base --is-ancestor bfe4b26d05e 8c678397d06` = rc 0 (true), 14 commits ahead.
      Verified independently by both main-session and sidekick, matching results.
- [x] Diff contains a real fix: `apt-get update` added before the Playwright system-lib
      `apt-get download` fallback step in auth-browser-tests.yml + mobile-auth-regression.yml
      (commit 2ec08998d49, 10 lines), correctly diagnosing stale apt indexes on self-hosted
      runners as the root cause of the Mobile-Auth/Playwright CI failures. Plus a real,
      resolved 102-commit merge-sync with origin/main (commit 8c678397d06, 14 files).
- [x] Coder session wa-3084 confirmed alive via live process table (PID 3483520) and a
      genuinely growing Claude Code transcript (~/.claude/projects/-home-jleechan--worktrees-worldarchitect-wa-3084/*.jsonl)
      — not operator-authored, not fabricated.

## E2 — CI actually re-runs and reports real results against the new SHA — ✅ CONFIRMED 2026-07-10T19:02 UTC

- [x] Final head SHA 25ae794b3e4adf31555b5f4f3736b34643f2949b. Full check-runs sweep:
      46 success, 0 failure, 8 cancelled (stale artifacts from earlier scoped reruns
      across mixed run IDs, all superseded), 1 neutral, 3 skipped. Verified via
      `gh api .../commits/<sha>/check-runs?per_page=100` (not the default-paginated
      30-result view, which earlier caused a false "missing check" reading).
- [x] Mobile Auth Same-Origin Regression: PASS. Playwright auth browser tests: PASS.
      Directory tests (mcp): PASS (real fix — missing Node.js setup for that matrix
      group, not a flake). PR Coverage Report: PASS (timeout raised 5->15min).
- [x] Green Gate Precheck (Gates 1-6): PASS. Bugbot Gate Wait (Gate 4): PASS.
      Smoke Gate Wait (Gate 8): PASS — required an explicit real-mode smoke test
      trigger (`gh workflow run mcp-smoke-tests.yml -f pr_number=7888 -f test_mode=real`)
      since nothing in /af's pipeline ever fires one automatically; the default
      auto-run smoke test is mock-mode and does not satisfy this gate. Genuine
      pipeline gap, not yet fixed at the /af level (see WAVE note in STATE.md).
- [x] Green Gate itself: SUCCESS (started/completed 19:01:38-19:01:42Z, within the
      same workflow run as the passing Precheck/Bugbot/Smoke jobs — confirmed not
      stale by timestamp).
- [x] `gh api repos/.../pulls/7888 --jq '{mergeable, mergeable_state}'` = 
      `{"mergeable": true, "mergeable_state": "clean"}` — first "clean" result all
      night (was "dirty"/"unstable" every prior check).

## E3 — Gate-7 (skeptic reviewer) produces a genuine, independently-checkable verdict — ✅ (partial) 2026-07-10T19:51 UTC

- [x] `GATE_ASSESSMENT` for jleechan-93ft at 2026-07-10T19:51:48Z, `all_green: false` —
      and the manual cross-check confirms this is CORRECT, not a daemon lie: PR #7888's
      last CodeRabbit review state is COMMENTED, not APPROVED
      (`gh api repos/.../pulls/7888/reviews`), so gate 3 legitimately fails even though
      CI (51 success/0 fail) and mergeable_state=clean are green. The earlier
      "both reviewers failed" spam was the pre-restart binary; the 12:39 PDT restart
      (with PR#216's multi-vendor fallback) cleared it.
- [ ] Reviewer vendor identity NOT confirmable from telemetry — GATE_ASSESSMENT logs
      only the all_green boolean (gap filed as jleechan-wzgl). Circumstantially the
      verdict came from claude (vendor2): codex quota-dead until Jul 13, agy returns
      empty stdout, gemini hard-broken (IneligibleTierError UNSUPPORTED_CLIENT, filed
      jleechan-yige). Claude is non-self for this minimax-coded bead, so the
      non-self-review property holds, but "genuinely ran" rests on elimination, not
      positive evidence. Full check-off requires jleechan-wzgl's vendor telemetry.
- Follow-on at 19:51:59Z: the correct all_green=false triggered an adopted-branch
  remediation re-roll whose spawn failed across the whole fallback chain
  (minimax/claude-code/agy) and parked the bead HUMAN_HELD (filed jleechan-r56m).

## E4 — /er (evidence review) passes on the real diff — ✅ daemon path live-verified 2026-07-10T20:42 UTC

- [x] The daemon's own evidence-review path produced a genuine fresh verdict against
      the ACTUAL current head: after PR#227 (jleechan-nplh staleness fix) was merged
      (bb79021) and deployed (daemon restart 13:19:05 PDT), the very next full cycle
      REFUSED the stale 2026-07-08 PASS and posted `/er PARTIAL — headline
      free-text-classifier E2E is captured at a stale SHA (12 commits behind) and not
      wired into CI; unit guard evidence is real and CI-green` (PR comment
      2026-07-10T20:42:34Z; daemon `ER_RUNNER_POSTED {attempt:1, verdict: Partial}` at
      20:42:35Z). Not silently downgraded — explicit PARTIAL with reasoning, exactly
      per this criterion's grammar. Operator acceptance of the PARTIAL (or a fix for
      the stale-SHA E2E capture it cites) is the remaining human decision.
- [ ] Evidence-artifact readability check for the verdict's citation (the stale-SHA
      E2E capture it flags) — the PARTIAL itself IS the evidence gap finding; open
      until the E2E capture is refreshed at the current head or the PARTIAL is
      explicitly accepted.
- Full cycle observed 20:42:35-20:42:54Z: ER_RUNNER_POSTED → GATE_ASSESSMENT
  all_green=false (correct: gate 3 CodeRabbit unapproved + gate 6 Partial) →
  REROLL_START → CIRCUIT_BREAKER_TRIGGERED (same reviewer + same semantic feedback
  hash as prior attempt) → PARKED_HUMAN_HELD. The circuit breaker escalating to human
  instead of burning attempt 5 is designed behavior, working.

## E5 — Mergeable and merged (or a documented, honest reason it isn't)

- [ ] `gh api repos/jleechanorg/worldarchitect.ai/pulls/7888 --jq '{state,merged,mergeable_state}'`
      shows `mergeable_state: clean` (not `dirty`/`unstable`/`unknown`) before any merge
      claim is made.
- [ ] EITHER: the PR is actually merged (`merged: true`, with `merge_commit_sha`
      independently verified to descend from both the PR branch and main), OR: if still
      open at end-of-session, the reason is documented honestly (e.g. "green but awaiting
      human MERGE APPROVED per this repo's human-merge-only rule for the product repo" —
      NOT "still fixing" if it's actually green and just needs a merge click).

## E6 — No regression in the daemon/factory itself

- [ ] `cargo test` (full daemon suite) still passes on whatever daemon HEAD is deployed
      at the end of this work — every PR merged tonight (#212-#217, and any further ones)
      must have landed via the established TDD + adversarial-review + squash-merge +
      deploy-verify discipline, not a shortcut.
- [ ] The daemon process is healthy (`systemctl --user show ai.dark-factory.daemon.service
      -p ActiveState,NRestarts` — active, 0 unexpected restarts) at session end.
- [ ] Every NEW bug found and fixed tonight (reviewer-vendor exhaustion, SHA-capture
      wrong-repo bug, zombie AO session, AO-not-running, misleading telemetry, AO
      worktree wrong-remote) has either a merged fix or a filed, prioritized bead — not
      silently dropped.

## Explicitly OUT of scope for "done" (anti-goalpost-moving)

- A local, unpushed commit in a coder's worktree does NOT satisfy E1.
- A daemon telemetry event that LOOKS like success (e.g. `EXISTING_PR_ADOPTED`) does not
  satisfy any criterion above without independent confirmation, per the ground rule.
- "The reviewer gate stopped erroring" does not satisfy E3 without a genuine verdict on
  the CURRENT PR state — the gate working again is necessary but not sufficient.
- Fixing daemon bugs (however real and valuable) is not a substitute for E1-E5 actually
  happening on the real PR. The daemon fixes are instrumental, not terminal.

## Status tracking

Check this file's boxes as each criterion is independently verified, with a one-line
evidence pointer (command run + result) next to each checked box. Do not check a box from
a sub-agent's or teammate's unverified claim alone.
