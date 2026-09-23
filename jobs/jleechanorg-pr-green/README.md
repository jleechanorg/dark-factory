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

`report.sh` produces the 8-hour Slack and daily email summaries. It reads
`runs.jsonl` plus per-PR `outcomes.jsonl`; a repair is counted only when an
outcome says `result=fixed` and `verified=true`. Dispatches and AO session reuse
remain attempts. Its report-state file advances only after delivery succeeds,
so manual service starts cannot spam Slack or email. The supplied reporting
service/timer templates are named `jleechanorg-pr-green-*-report.*`.
