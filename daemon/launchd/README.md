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