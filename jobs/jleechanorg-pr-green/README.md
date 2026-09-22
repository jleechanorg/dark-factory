# jleechanorg PR green repair job

This job is the repository-owned contract for the 30-minute PR repair sweep.
The runtime launcher installs or invokes `run.sh`; it must discover every
non-draft PR updated in the last 24 hours, select only red CI or merge conflicts,
and dispatch through AO with stale-session recovery. It must never merge or
force-push.

The launcher currently delegates to the host scheduler implementation while
the job is being migrated into this repository. The systemd unit must point at
this directory and record its exact commit before activation.
