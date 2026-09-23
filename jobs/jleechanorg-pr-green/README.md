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
