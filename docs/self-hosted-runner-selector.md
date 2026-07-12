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

### Known platform coverage gap

`tests/security/test_agent_isolation.py` contains `@linux_only` test cases
that test the `LD_PRELOAD` deny-path sandbox shim. These tests are **skipped**
on macOS runners (they emit a `skipif` reason). If the scheduler picks a macOS
runner, sandbox regressions in the Linux backend can merge green.

Mitigation path:
1. Request the org runner admin to add a `self-hosted-linux` label to the
   `ez-runner-c-*` fleet.
2. Add a `test-linux` required job to `ci.yml` with
   `runs-on: [self-hosted, self-hosted-linux, ezgha]`.
3. Until then, the `test` job emits a
   `::warning title=Platform coverage gap::` annotation when it detects
   a non-Linux runner, so the gap is visible in the CI log.

## Fork PR isolation

Both `ci.yml` jobs (`test` and `daemon-tests`) include:

```yaml
if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.fork == false
```

This prevents fork PR code from executing on the org's persistent self-hosted
runners. GitHub's [secure-use docs for public repos with self-hosted runners](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#hardening-for-self-hosted-runners)
warn that persistent runners can be persistently compromised by untrusted PR
code (environment variable poisoning, secret extraction from runner cache,
lateral movement via build artifacts).

**Affected scenarios:**
- Fork PR → CI jobs are **skipped** (not failed, not rerouted to hosted runners).
- Same-org PR → CI jobs run normally on self-hosted runners.
- Push to `main` → CI jobs run normally on self-hosted runners.

**What fork authors see:** the `test` and `daemon-tests` check status will not
appear on their PR (the jobs are skipped at the workflow level). The PR author
should open a PR from a branch in `jleechanorg/dark-factory` to get CI coverage.

## drift-check authentication (`RUNNERS_READ_PAT`)

The drift-check workflow calls `gh api orgs/jleechanorg/actions/runners`.
This endpoint requires the `manage_runners:org` permission.

**Primary auth**: The self-hosted runner images in `jleechanorg` are
pre-authenticated with a token that has `manage_runners:org` scope. When
`RUNNERS_READ_PAT` is **not** configured, the workflow unsets `GH_TOKEN` so
`gh` uses the runner's native keyring auth — this is the steady-state path.

**Fallback auth** (`RUNNERS_READ_PAT`): If the runner fleet is re-imaged
without pre-auth (or on fresh runners), set this secret to ensure the drift
check can still call the org runners endpoint. The secret is a Fine-Grained
PAT with `manage_runners:org` read permission on `jleechanorg`.

To create `RUNNERS_READ_PAT` (only needed if runner native auth is absent):
1. Go to **GitHub → Settings → Developer settings → Fine-grained personal
   access tokens → New token**.
2. Set **Resource owner** to `jleechanorg`.
3. Set **Repository access** to "All repositories" (or limit to dark-factory).
4. Under **Permissions → Organization permissions → Self-hosted runners**,
   grant **Read**.
5. Store the generated token as a repository or org secret named
   `RUNNERS_READ_PAT`.

> [!WARNING]
> Do **not** set `GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}` in the drift-check
> env block. The default Actions token lacks `manage_runners:org` and
> overrides the runner's native auth, causing HTTP 403. The workflow's env
> block passes `RUNNERS_READ_PAT` (not `GH_TOKEN`) and the run script
> conditionally exports `GH_TOKEN` only when the PAT is present.

## Failure modes and runbooks

| Symptom | Likely cause | Runbook |
|---|---|---|
| Drift-check workflow red on a PR | Variable was edited/reset, or runners retired | Run the local drift command; if `match_count: 0`, patch the variable per "Updating the selector" above. |
| Drift-check workflow stays queued | The hardcoded fallback selector itself drifted | Verify via `gh api orgs/jleechanorg/actions/runners?per_page=100 --jq '.runners[].labels[].name' \| sort -u`; patch the workflow's hardcoded list. |
| Drift-check says FLEET_DOWN | All runners offline | See `~/.claude/skills/self-hosted-runner-preflight/SKILL.md` Class B/E triage. |
| CI job queues forever with no log | The variable was edited outside `gh api` and is now a YAML list instead of a JSON string | Re-patch the variable to a JSON-array string; verify via `gh api ... --jq .value` |
| Drift-check exits 2 — HTTP 403 | Runner native auth missing (fresh runner without pre-auth); `RUNNERS_READ_PAT` not configured | Configure `RUNNERS_READ_PAT` secret — see "drift-check authentication" above. |
| CI jobs not running on fork PRs | Expected: fork PRs are skipped by the `if:` guard | See "Fork PR isolation" above. Fork authors must PR from a same-org branch. |
| Platform coverage warning in CI log | Job ran on macOS — Linux-only isolation tests skipped | See "Known platform coverage gap" above. |

## Related

- `scripts/check_runner_selector.py` — the drift check itself
- `tests/test_check_runner_selector.py` — pure-Python + CLI tests
- `.github/workflows/runner-selector-drift.yml` — the workflow that runs it
- `~/.claude/skills/self-hosted-runner-preflight/SKILL.md` — host-level
  runner triage (disk, container, registration)