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

`report.sh` produces the 8-hour Slack and daily email summaries. It reads
`runs.jsonl` plus per-PR `outcomes.jsonl`; a repair is counted only when an
outcome says `result=fixed` and `verified=true`. A green new-head observation
does not prove this job pushed the head unless the record contains an explicit
push receipt; session action alone is never attribution. Dispatches and AO
session reuse remain attempts. A successful AO send is also not delivery proof:
the native Codex rollout must record the exact submitted prompt, otherwise the
outcome is `delivery_unconfirmed` and duplicate replay is suppressed. Its
report-state file advances only after delivery succeeds, so manual service
starts cannot spam Slack or email. The supplied reporting service/timer
templates are named `jleechanorg-pr-green-*-report.*`.
