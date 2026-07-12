# Self-hosted runner selector & drift check

> **Audience:** anyone touching `runs-on:` in a `.github/workflows/*.yml` file
> in `jleechanorg/dark-factory`, or anyone responsible for the org's runner
> fleet.
>
> **Refs:** bead `jleechan-z284`, issue
> [#286](https://github.com/jleechanorg/dark-factory/issues/286),
> workflow `runner-selector-drift.yml`.

## TL;DR

- The repo variable `SELF_HOSTED_RUNNER_LABELS` is the source of truth for
  which self-hosted runner pool CI jobs target.
- All workflows use `runs-on: ${{ fromJSON(vars.SELF_HOSTED_RUNNER_LABELS) }}`.
  **Never** put `runs-on: ubuntu-latest` (or any other GitHub-hosted label)
  in a workflow file. That is policy drift and will be rejected.
- `.github/workflows/runner-selector-drift.yml` runs the drift check on every
  push/PR + weekly + on demand, and uses a **hardcoded** selector so it can
  detect drift in the variable itself. A failing drift check fails the PR.
- Drift detected = at least one online runner no longer carries every label in
  the conjunction. The fix is to update the variable to a selector that
  matches ≥1 online runner.

## Why this exists

A selector that matches **zero** online runners causes every targeted job to
queue forever with no visible failure — silent. This happened in the wild when
the variable drifted to `["self-hosted","Linux","ARM64","agent-orchestrator"]`
on 2026-07-06 (bead `jleechan-z284`); zero of the 19 online runners exposed
all four labels, so every CI job sat at "in_progress / 0 steps" until the
drift was noticed by hand.

## Current truthful selector

As of 2026-07-12:

```json
["self-hosted","self-hosted-mikey","ezgha"]
```

This conjunction matches all 19 online org runners (`ez-mac-runner-b-1..3`
plus `ez-runner-c-1..16`). All 19 runners expose `self-hosted`,
`self-hosted-mikey`, and `ezgha`; the macOS runners additionally carry
`self-hosted-macos`, but adding it to the conjunction would restrict jobs to
the 3 mac runners only — we want the broadest possible pool.

To verify the selector still matches ≥1 runner, run locally:

```bash
SELF_HOSTED_RUNNER_LABELS="$(gh api repos/jleechanorg/dark-factory/actions/variables/SELF_HOSTED_RUNNER_LABELS --jq .value)" \
  python3 scripts/check_runner_selector.py --org jleechanorg --json
```

A `verdict: PASS` is the steady state.

## Updating the selector

When the runner fleet changes (retire, replace, re-label), update the
variable via `gh api`. **Never** edit it from the GitHub UI without also
updating this doc and running the drift check.

```bash
echo '{"value":"[\"self-hosted\",\"new-label\"]"}' > /tmp/var-patch.json
gh api repos/jleechanorg/dark-factory/actions/variables/SELF_HOSTED_RUNNER_LABELS \
  -X PATCH --input /tmp/var-patch.json
```

After patching:

1. Run the drift check locally (command above).
2. Trigger the drift-check workflow manually:
   `gh workflow run runner-selector-drift.yml`
3. Open a PR that touches one workflow file so CI exercises the new selector.

## How workflows use the selector

```yaml
runs-on: ${{ fromJSON(vars.SELF_HOSTED_RUNNER_LABELS) }}
```

`fromJSON()` converts the JSON-array string stored in the variable into a
YAML list. GitHub Actions interprets a list under `runs-on` as a conjunctive
selector (the runner must carry **every** label). With the current variable
this resolves to `runs-on: [self-hosted, self-hosted-mikey, ezgha]`.

**Forbidden** patterns — adding any of these is policy drift:

- `runs-on: ubuntu-latest`
- `runs-on: [ubuntu-latest, self-hosted]` (mixing hosted and self-hosted)
- `runs-on: ${{ fromJSON(vars.SELF_HOSTED_RUNNER_LABELS || '[some-fallback]') }}`
  (the fallback masks drift in the variable — the whole point of the gate is
  to *fail loud* when the variable breaks, not silently narrow)

## Drift detection

`.github/workflows/runner-selector-drift.yml` runs the drift check from
`scripts/check_runner_selector.py`. Triggers:

| Trigger | Purpose |
|---|---|
| `push` to `main` | catch drift the moment it ships |
| `pull_request` to `main` | catch drift before it ships |
| `schedule` (weekly Mon 09:00 UTC) | catch drift from fleet changes (retire/replace) |
| `workflow_dispatch` | manual run on demand |

The drift check job itself uses a **hardcoded** selector
(`[self-hosted, self-hosted-mikey, ezgha]`) on purpose: it must remain
reachable even when the variable is broken, because a broken selector is the
very failure mode the gate exists to detect. Routing the gate through
`vars.SELF_HOSTED_RUNNER_LABELS` would defeat the purpose — if the variable
drifts to a no-match selector, the gate itself queues forever and the drift
goes silent.

Exit codes of `scripts/check_runner_selector.py`:

| rc | Meaning |
|---|---|
| 0 | PASS — ≥ `--min-matches` (default 1) runners satisfy the selector |
| 1 | DRIFT — selector matches fewer than required runners |
| 2 | invocation error (bad args, malformed selector, `gh` failed) |
| 3 | FLEET_DOWN — zero online runners (distinct from drift) |

Any non-zero rc fails the workflow.

## Why we don't fall back to GitHub-hosted runners

The acceptance criterion for issue #286 explicitly forbids it. Two reasons:

1. **The whole point is to use the private runner pool.** A fallback to
   `ubuntu-latest` would silently swallow the very drift we are guarding
   against.
2. **Cost / data residency.** Some CI steps assume on-prem secrets, on-prem
   caches, and on-prem source mirrors. A GitHub-hosted fallback would route
   data through GitHub's hosted environment and would also count against the
   org's Actions minute budget.

If a job genuinely needs hosted-only tooling (e.g. an Actions marketplace
feature unavailable on the self-hosted image), the answer is to add the
tooling to the runner image — not to widen the fallback.

## Adding a platform-specific job

The current selector matches both Linux (`ez-runner-c-*`) and macOS
(`ez-mac-runner-b-*`) runners. If a job needs to be platform-restricted:

- **macOS-only:** add `self-hosted-macos` to the conjunction. Currently 3
  runners match.
- **Linux-only:** the fleet has no explicit `self-hosted-linux` label; the
  Linux runners are distinguished by the *absence* of `self-hosted-macos`. If
  a future change adds `self-hosted-linux`, use it.

If you add a platform-specific job, document the selector and the rationale
in this file and update the `select_matching` test in
`tests/test_check_runner_selector.py` so the conjunction's coverage is
exercised.

## Failure modes and runbooks

| Symptom | Likely cause | Runbook |
|---|---|---|
| Drift-check workflow red on a PR | Variable was edited/reset, or runners retired | Run the local drift command; if `match_count: 0`, patch the variable per "Updating the selector" above. |
| Drift-check workflow stays queued | The hardcoded fallback selector itself drifted | Verify via `gh api orgs/jleechanorg/actions/runners?per_page=100 --jq '.runners[].labels[].name' \| sort -u`; patch the workflow's hardcoded list. |
| Drift-check says FLEET_DOWN | All runners offline | See `~/.claude/skills/self-hosted-runner-preflight/SKILL.md` Class B/E triage. |
| CI job queues forever with no log | The variable was edited outside `gh api` and is now a YAML list instead of a JSON string | Re-patch the variable to a JSON-array string; verify via `gh api ... --jq .value` |

## Related

- `scripts/check_runner_selector.py` — the drift check itself
- `tests/test_check_runner_selector.py` — pure-Python + CLI tests
- `.github/workflows/runner-selector-drift.yml` — the workflow that runs it
- `~/.claude/skills/self-hosted-runner-preflight/SKILL.md` — host-level
  runner triage (disk, container, registration)