# Automation script config convention: empty-by-default, fail closed

## Why this exists

`config/auto_merge_repo_allowlist.json` shipped in PR #735, the day after the
2026-08-23 PR-merge-storm production incident: `daemon/scripts/auto-merge-guard.sh`
had no repo scoping or human-approval gate, and an unattended burst of 42
merges in 12 hours across multiple repos shipped real regressions (a
whitelist strip on `PATCH /api/campaigns/<id>`, and wiped context-compression
pruning). The fix was config-only by design: which repos the guard may
auto-merge in is controlled entirely by an allowlist file that **defaults to
an empty list** — absence of the config file, an empty list, or the target
repo simply not being present in it all mean "no merges this pass." Re-enabling
a repo is a one-line config edit, never a code change or redeploy.

That pattern — an irreversible/outward-facing script gated by a config file
that ships empty and must be explicitly opted into — was implemented once,
for one script, in response to one incident. Nothing stopped the next script
from reopening the same class of risk. This doc makes the pattern a standing
rule instead of tribal knowledge, and `scripts/check_auto_script_configs.sh`
enforces it in CI.

## The rule

Any script under `daemon/scripts/` matching `auto-*.sh` or `*-merge-*.sh`
that performs an irreversible or outward-facing action (merge, push, delete,
publish, or anything else that mutates state outside this repo's own working
tree) **must** ship a matching `config/*_allowlist.json` (or equivalent
policy file) that:

1. Is referenced from the script via a literal, greppable `config/*.json`
   path (a default-expanded `${VAR:-.../config/whatever.json}` pattern is
   fine — see `daemon/scripts/auto-merge-guard.sh`'s `AMG_REPO_POLICY_FILE`
   for the canonical example).
2. Defaults to an **empty** list for every allow/deny array it commits to
   the repo. Fail closed, not fail open: absence of the config, a missing
   entry, or an empty list must all mean "do nothing," never "do everything."
3. Is loaded and checked *before* the script takes any irreversible action —
   the gate must run first, not as an afterthought.

A script that only performs the mechanical part of an already-gated action
(e.g. `daemon/scripts/gh-pr-merge-wrapper.sh`, invoked exclusively by
`auto-merge-guard.sh` *after* that script's own allowlist gate has already
passed) does not need its own duplicate config. Such scripts must be listed,
with a documented reason and reference to the actual caller + gate, in
`config/auto_script_config_check_exceptions.json`. Exceptions are the
narrow escape hatch, not a way to avoid writing the gate — most new
`auto-*`/`*-merge-*` scripts should ship their own config, not an exemption.

## CI enforcement

`scripts/check_auto_script_configs.sh` implements this mechanically:

- Scans `daemon/scripts/auto-*.sh` and `daemon/scripts/*-merge-*.sh`.
- Skips scripts listed in `config/auto_script_config_check_exceptions.json`.
- For every other match, greps the script body for a `config/*.json`
  reference. No reference found → **FAIL**.
- Loads the referenced config as JSON. Any list-typed value in it that is
  non-empty in the committed file → **FAIL**.
- Exits non-zero on any failure (a real CI gate, not a warning — a
  warning-only check on merge/delete/publish policy would be trivially
  ignored, which defeats the point of codifying this after a production
  incident).

Run it locally:

```bash
scripts/check_auto_script_configs.sh
```

It is picked up automatically by CI's `tests/scripts/test_*.sh` sweep via
`tests/scripts/test_check_auto_script_configs.sh`, which exercises both the
real repo state (`auto-merge-guard.sh` passes, `gh-pr-merge-wrapper.sh` is
exempted) and fixture negative cases (a missing config, and a non-empty
default config, are both flagged).

## Adding a new gated script

1. Write `daemon/scripts/auto-<name>.sh` (or `*-merge-*.sh`) as usual.
2. Add `config/<name>_allowlist.json` with an empty default list and a
   `_comment` explaining what enables it, mirroring
   `config/auto_merge_repo_allowlist.json`.
3. Load that file first thing in the script; treat "file missing," "empty
   list," and "target not in list" identically as "do nothing this pass."
4. Run `scripts/check_auto_script_configs.sh` locally before opening the PR.
