# jleechanorg PR green repair job

This job is the repository-owned contract for the 30-minute PR repair sweep.
The runtime launcher installs or invokes `run.sh`; it must discover every
non-draft PR updated in the last 24 hours, select only red CI or merge conflicts,
and dispatch through AO with stale-session recovery. It must never merge or
force-push.

`run.sh` executes the adjacent `jleechanorg-pr-green-daily.sh`, which is the
tracked implementation. The systemd unit points at `run.sh`; deployments must
activate a reviewed commit from this directory rather than an untracked copy
under `$HOME/bin`.

Each selected PR records an exact GitHub blocker snapshot before AO is contacted.
`$PR_GREEN_METRICS_DIR/outcomes.jsonl` is append-only and contains `ts`, `run_ts`,
`repo`, `number`, `url`, `head_before`, `head_after`, `blocker_before`,
`blocker_after`, `classification`, `session_action`, `result`, and `verified`.
`result=fixed` is emitted only when `verified=true`: a fresh GitHub read saw a
new head with both the original conflict and failed checks cleared. Dispatch or
session reuse is never counted as a fix.

To prevent a 30-minute scan from repeatedly spending inference on an unchanged
concrete blocker, `no_change` and `pushed_still_blocked` outcomes impose an
eight-hour cooldown for the same head and conflict/failed-check signature. The
PR remains analyzed and counted as actionable, with
`session_action=cooldown_deferred`, but AO/Codex is not contacted. A head or
blocker change and `pushed_ci_pending` always bypass this guard. Override the
default with `PR_GREEN_SAME_HEAD_COOLDOWN_SECONDS`.

`report.sh` produces the 8-hour Slack and daily email summaries. Its first
screen is intentionally ordered as unique analyzed PRs, unique PRs with
verified job-owned remote commits, and verified green state; the repair funnel
and exceptions follow. Per-run totals are labelled as such, so repeated scans
are not presented as unique PRs.

Discovery identities are deduplicated from matching `discovery-<run_ts>.tsv`
snapshots only when the snapshot row count equals both that run's `discovered`
and `analyzed` totals. Historical snapshots outside the reporting window are
ignored. If some, but not all, window runs have complete snapshots, the report
shows an `at least N unique (X/Y runs covered)` floor; with no complete snapshot
it shows `unknown (coverage incomplete)`. Missing history is never reported as
zero.

The successful-remote-commit count requires an explicit, independently
verifiable `push_receipt` object with `verified: true`, `push_exit_code: 0`,
non-empty differing `before_sha`/`after_sha`, a non-empty `commit_url`, and
non-empty `repo` plus `session_id` (or `session`) provenance. Empty, false, or
partial receipts are rejected. Receipt-backed PRs are listed with their PR URL
and commit/evidence URL regardless of whether CI is green. A green new-head
observation does not prove this job pushed the head; the report labels green
state as unattributed unless the receipt is separately present.

A repair is counted as green only when an outcome says `result=fixed` and
`verified=true`. Dispatches and AO session reuse remain attempts. A successful
AO send is also not delivery proof: the native Codex rollout must record the
exact submitted prompt, otherwise the outcome is `delivery_unconfirmed` and
duplicate replay is suppressed. The report-state file advances only after
delivery succeeds, so manual service starts cannot spam Slack or email. The
supplied reporting service/timer templates are named
`jleechanorg-pr-green-*-report.*`.

The five-lane report review (truth, economy, readability, AI-tell, and
operability) is captured at `/tmp/pr-green-email-ds-review-20260923.md`; this
revision preserves its load-bearing attribution and coverage caveats while
consolidating the first-screen output.
