# dark-factory launchd agents

This directory owns the `~/Library/LaunchAgents/` plist templates for the
dark-factory auto-factory daemon poll loop, plus the launchd-wrapper that
plists invoke to source the user's interactive login environment.

## Files

| File | Purpose |
|------|---------|
| `ai.dark-factory.af-tick.plist.template` | poll-loop agent (StartInterval, KeepAlive, ThrottleInterval) |
| `launchd-wrapper.sh` | sources `~/.bash_profile` then exec's the target script |
| `local.dark-factory.qw5-pilot-dispatch.plist.template` | one-shot dispatch timer (NOT installer-managed — see below) |

## Install

Run from the **repository root** (not from this directory):

```bash
./install-launchagents.sh
```

The installer is idempotent: it re-renders every managed plist from its
template, bootouts any previously loaded instance, and bootstraps the new one.

`install-launchagents.sh` iterates over `daemon/launchd/*.plist.template`,
substitutes `@HOME@` (and the installer-managed `@TICK_INTERVAL@`) using
`sed`, writes the result to `$HOME/Library/LaunchAgents/<label>.plist` with
mode `0644`, then runs `launchctl bootstrap`.

## Uninstall

```bash
./install-launchagents.sh --uninstall
```

Boots out and removes every installer-managed plist. Add `--label` to limit
scope (repeatable):

```bash
./install-launchagents.sh --uninstall --label ai.dark-factory.af-tick
```

## Dry-run

```bash
./install-launchagents.sh --dry-run
```

Prints every action that would be taken (mkdir, sed substitution, chmod,
launchctl bootstrap/bootout) without executing any of them. Use this to
verify the install plan before running it for real.

## Selective install

By default the installer processes every `*.plist.template` in this
directory. To install a single label:

```bash
./install-launchagents.sh --label ai.dark-factory.af-tick
```

`--label` is repeatable; templates whose `<basename>` is not in the list
are skipped. The flag pairs with `--uninstall` for surgical removal.

## Verify

```bash
launchctl list | rg dark-factory
```

You should see a row with the label `ai.dark-factory.af-tick`.

## Logs

Tick stdout/stderr land under `~/Library/Logs/dark-factory/af-tick.{out,err}.log`.

Tail with:

```bash
tail -f ~/Library/Logs/dark-factory/af-tick.out.log
tail -f ~/Library/Logs/dark-factory/af-tick.err.log
```

## Tick interval (bead jleechan-57h0 acceptance)

`ai.dark-factory.af-tick.plist.template` declares `<integer>@TICK_INTERVAL@</integer>`,
substituted by the installer from the `AFD_TICK_INTERVAL_SEC` env var
(default `240` = 4 minutes). Validated as a positive integer ≥10.

```bash
AFD_TICK_INTERVAL_SEC=120 ./install-launchagents.sh   # 2-minute tick
```

## Crash-loop protection (bead jleechan-57h0 acceptance)

The plist declares `<key>ThrottleInterval</key><integer>60</integer>`.
launchd will not restart the agent more than once per 60 seconds, so a
1-second-exit crash loop cannot thrash the host.

## Template placeholders

The installer recognises exactly two placeholders:

- `@HOME@` — substituted with `$HOME` (use `%` as the sed delimiter, so
  paths containing `|` are safe).
- `@TICK_INTERVAL@` — substituted with `$AFD_TICK_INTERVAL_SEC` (default 240).

Any template containing a `@SOMETHING_ELSE@` placeholder is **skipped** at
install time with a clear error (e.g., `local.dark-factory.qw5-pilot-dispatch.plist.template`,
which uses `@YEAR@/@MONTH@/...` meant to be filled by an operator for
one-shot dispatch timers). Skipped templates are left untouched; you must
render them by hand and copy the result to `~/Library/LaunchAgents/`.

## Why the wrapper exists

launchd runs agents with a minimal `PATH=/usr/bin:/bin:/usr/sbin:/sbin`
and no sourced shell init. dark-factory tick scripts depend on homebrew/conda
tooling (`br`, `gh`, `sqlite3`, `python3`, `callpath`, `jq`) and on the
user's git/SSH configuration. Without a wrapper, the first tick fails
with `command not found` errors.

`launchd-wrapper.sh` sources `~/.bash_profile` (with `set +u/-u` guards
around the source so a strict-mode bashrc doesn't break us), prepends
`/opt/homebrew/bin` and `/usr/local/bin` to `PATH` if not already present,
then `exec`s the target script.

## Why templates live here

Per the launchd-plist-template skill, every plist installed to
`~/Library/LaunchAgents/` must have a template committed to the owning
repo. Without the template, `./install-launchagents.sh` cannot clean up
or rotate the plist across machines.

## Checkout drift protection (bead jleechan-vxs8)

The plist's `ProgramArguments` point at `@HOME@/projects/dark-factory` — a
dev working tree that is also used interactively, NOT a dedicated
deploy-only checkout. Historically this meant every `git checkout <branch>`
in that tree was an unaudited production deploy: on 2026-07-11 the tree sat
on a crashing feature branch for hours, then was silently switched to a
different branch by another session, and neither state was a deliberate
deploy.

Rather than splitting off a separate deploy-owned checkout path (more
moving parts: a second clone to keep in sync, a second directory to explain
in every runbook, and `install-launchagents.sh`/the plist would need
rewiring to point at it), `daemon/factory-af-tick.sh` embeds a **Gate-0-style
drift-refusal check** near the top of its main body (search for `Gate 0:
refuse to tick`). Before doing any dispatch work, every tick verifies its
own checkout:

1. is on branch `main`,
2. has no uncommitted changes,
3. matches `origin/main` (best-effort fetch — a transient network blip does
   not fail the tick, only a *confirmed* mismatch does).

Any violation makes the tick **refuse and exit 10** with a clear log line
(landing in `af-tick.err.log`) instead of silently running whatever code
happens to be on disk. `AFD_SKIP_DRIFT_CHECK=1` bypasses the gate for local
dev/test invocations of the script — the production plist never sets this.

### Deploying a change to the daemon

The daemon's checkout is only ever advanced by
`daemon/scripts/deploy-af-tick.sh`, the one sanctioned deploy step. It
refuses to run against a dirty or non-main checkout, fast-forwards to
`origin/main`, and logs the SHA transition (old -> new) both to stdout and
to a JSONL audit log (`~/Library/Logs/dark-factory/deploy.jsonl` by
default). See that script's header comment for the full contract
(exit codes, `--dry-run`, `--target-dir`). It does **not** restart the
launchd job — see "Single-writer rule" below.

## Single-writer rule — who runs the deploy step

Per CLAUDE.md policy, restarting the live `ai.dark-factory.af-tick` launchd
job is the exclusive responsibility of the session's designated
deploy-owner. A coder/PR-driving session must never bootstrap/bootout the
live job or touch `~/projects/dark-factory` directly — see the bead
jleechan-vxs8 non-goals. `deploy-af-tick.sh` mirrors this: it fast-forwards
the checkout only. The running daemon picks up new code on its next tick
(each tick execs `factory-af-tick.sh` fresh from disk), so no restart is
required for a normal deploy. A human deploy-owner would run, from any
machine with SSH/terminal access to the box that runs the launchd job:

```bash
# 1. Deploy: fast-forward the daemon's checkout to origin/main.
daemon/scripts/deploy-af-tick.sh
#    -> prints "deploy-af-tick: DEPLOYED <dir>: <old_sha> -> <new_sha>"
#       or "deploy-af-tick: no-op — already at origin/main (<sha>)"

# 2. Verify a scheduled tick actually ran the new code (wait up to one
#    AFD_TICK_INTERVAL_SEC, default 240s, for the next natural tick — do NOT
#    force a restart just to "prove" this; it's not required, see above):
tail -f ~/Library/Logs/dark-factory/af-tick.out.log
#    -> confirm a fresh tick line lands after the deploy timestamp and does
#       NOT contain "REFUSING TICK" (which would mean the drift check itself
#       is unexpectedly firing post-deploy — a bug, since deploy-af-tick.sh
#       leaves the checkout clean and on main by construction).

# 3. Cross-check the audit trail:
tail -1 ~/Library/Logs/dark-factory/deploy.jsonl
#    -> {"ts":"...","target_dir":"...","old_sha":"...","new_sha":"...","noop":false}
```

If step 2 shows a `REFUSING TICK` line instead of normal dispatch activity,
something external touched the checkout between the deploy and the next
tick (e.g. a stray interactive `git checkout` in the same tree) — treat
that as a defect in whatever touched the tree, not in the daemon.