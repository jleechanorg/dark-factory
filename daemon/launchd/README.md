# dark-factory launchd agents

This directory owns the `~/Library/LaunchAgents/` plist templates for the
dark-factory auto-factory daemon poll loop.

## What's installed

- `ai.dark-factory.af-tick.plist.template` — poll-loop agent that fires
  `daemon/factory-af-tick.sh` every 240 seconds, with auto-restart on exit.

## Install

```bash
./install-launchagents.sh
```

The installer is **idempotent**: it re-renders the plist from the template,
bootouts any previously loaded instance, and bootstraps the new one.

`install-launchagents.sh` reads every `*.plist.template` in this directory
and installs each one, so adding a new agent is just dropping another
template next to these files.

## Uninstall

```bash
./install-launchagents.sh --uninstall
```

Boots out every installed dark-factory agent and removes the rendered plists.

## Dry-run

```bash
./install-launchagents.sh --dry-run
```

Prints every action that would be taken (mkdir, sed substitution,
launchctl bootstrap/bootout) without executing any of them. Use this to
verify the install plan before running it for real.

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

## Template placeholders

Every `*.plist.template` uses `@HOME@` as a placeholder for the user's
home directory. The installer substitutes `$HOME` at install time. Never
commit a plist with a real home directory baked in.

## Why templates live here

Per the launchd-plist-template skill, every plist installed to
`~/Library/LaunchAgents/` must have a template committed to the owning
repo. Without the template, `install-launchagents.sh` cannot clean up
or rotate the plist across machines.