# Skeptic Gate (issue #278, mandatory redesign)

The Skeptic Gate is the dark-factory 7-green policy's **gate 7**. It
guarantees that every merged PR has an independent, SHA-bound review
verdict from non-Claude reviewer CLIs (Codex AND Gemini must both
PASS), published by code that **never** executes PR-head Python on
the credentialed runner.

## Bootstrap dependency (READ THIS BEFORE MERGING)

**This PR cannot self-bootstrap gate-7.** A `pull_request` workflow
is forgeable — PR head controls its own YAML and could hardcode a
PASS marker — and `pull_request_target` cannot bootstrap until this
workflow file already lives on the default branch, which is exactly
what this PR is adding. The catch-22 is structural, not
implementation-specific.

The only legitimate bootstrap is via a separate trusted external
caller workflow that lives on the default branch and invokes this
file via `uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@<PINNED-SHA>`.

Concretely:

1. Merge this PR so the workflow + Python files exist on `main`.
2. Add a trusted caller workflow (e.g. `.github/workflows/skeptic-caller.yml`)
   that lives on `main` and uses `pull_request_target` to call this
   file via `workflow_call`. The caller pins the ref via
   `uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@<PINNED-SHA>`
   and forwards `secrets.GITHUB_TOKEN`.
3. The caller must be in the same target repo (`jleechanorg/dark-factory`)
   so the GITHUB_TOKEN's repo scope matches the comment/status target.
   Cross-repo callers (e.g. `jleechanorg/.github`) cannot post comments
   into a private repo with a caller-repo-scoped token — they fail
   closed at the first API call.

Until the caller exists:

- This workflow has NO `pull_request` trigger (forgeable).
- This workflow has NO `pull_request_target` trigger (cannot bootstrap).
- This workflow accepts `workflow_call` only from a trusted caller in
  the same repo.
- This workflow accepts `workflow_dispatch` for diagnostic re-runs only.
  A `workflow_dispatch` run posts as `github-actions[bot]`, but the
  read-back step requires a same-target-repo bot token; a human-
  dispatched run therefore cannot produce a satisfiable PASS.

**Self-PASS is impossible by design.** A diagnostic `workflow_dispatch`
run that tries to publish a PASS comment will fail the read-back step
because the comment actor's token scope won't match the target repo.
No "skip readback" flag exists; no `--force-pass` flag exists.

## What this PR does and does not do

This PR adds:

- A SHA-bound verdict parser (`parse_verdict`) with a strict no-prose
  contract (no Markdown code blocks, no extra prose, no second
  VERDICT line, exactly one occurrence per field).
- Multi-reviewer aggregation (`aggregate_results`) with duplicate-
  reviewer rejection (a PR may not be reviewed twice by the same
  model).
- A pinned-reviewer-binary workflow step that asserts the absolute
  path, version string, and SHA256 of each reviewer binary against
  repo variables before any reviewer is invoked.
- A sanitized reviewer subprocess env (no GITHUB_TOKEN, no HOME,
  no SSH agent socket, no OPENCLAW_*/HERMES_*/SLACK_* secrets, no
  cloud-credential env vars).
- stdin-only diff transport (no argv) to avoid E2BIG on the gemini
  CLI's ~140KB argv cap.
- An equality read-back step that fails closed when ANY of the six
  comment fields (HEAD_SHA, REPO, PR_NUMBER, VERDICT, REVIEWER,
  IMPLEMENTATION_PROVENANCE) disagrees with what we wrote.
- 67 contract + adversarial integration tests (commit-prefix
  provenance, code-block injection, duplicate reviewer, E2BIG,
  identity-mismatch, env leakage, status-failure-fail-closed, etc.).

This PR does NOT add:

- A `pull_request` or `pull_request_target` trigger. See "Bootstrap
  dependency" above.
- A `--force-pass` or `--skip-readback` flag. No escape hatch.
- A mutable reviewer install (`npm install -g …`) on the credentialed
  runner. Reviewer binaries must be pre-installed via the trusted
  runner image bootstrap.

## Headline invariants

> **1. A stale-SHA PASS must never satisfy a newer head.**
> **2. PR-head Python is never executed on the credentialed runner.**
> **3. Both reviewers must PASS; one FAIL or one unavailable → gate FAIL.**
> **4. The reviewer identity must differ from the implementer identity.**
> **5. The reviewer's declared IDENTITY must match its CLI
>    (codex ↔ codex, gemini ↔ gemini).**
> **6. Reviewer binaries are pinned to path + version + sha256 via
>    the trusted runner image; this workflow NEVER installs them.**
> **7. The reviewer subprocess env is allow-list only — no
>    GITHUB_TOKEN, no HOME, no SSH agent socket, no cloud creds.**
> **8. The published comment is read back with full equality on
>    all 6 fields; mismatched values fail closed.**
> **9. A `workflow_dispatch` self-run cannot produce a satisfiable
>    PASS — the read-back step refuses non-target-repo actor tokens.**

## Architecture

```
Trusted same-target-repo caller workflow
   pull_request_target on jleechanorg/dark-factory main
        │
        ▼
   uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@<PINNED-SHA>
        │  with inputs pr_number, pr_sha; secret github_token
        │
        ▼
   .github/workflows/skeptic-gate.yml
        │  (self-hosted runner; private repo selector
        │   via fromJson(vars.SELF_HOSTED_RUNNER_LABELS || '["self-hosted","self-hosted-mikey"]'))
        ▼
0. "Verify pinned reviewer binaries"  ← path + version + sha256
   against repo variables. Drift fails the gate (no mutable install).
        │
        ▼
1. "Checkout gate code from TRUSTED default branch"  ← sparse-checkout
   (runner/skeptic_gate.py + runner/skeptic_gate_cli.py from
   github.event.repository.default_branch; NEVER from PR head)
        │
        ▼
2. "Verify caller actor"  ← refuses non-github-actions[bot] actors
   for self-PASS (a human-dispatched workflow_dispatch cannot satisfy
   the read-back step's actor equality check).
        │
        ▼
3. "Run skeptic gate" with secrets stripped from the env (HOME,
   GITHUB_TOKEN, OPENCLAW_*/HERMES_*/SLACK_*) and PATH reduced to a
   minimal list containing only the pinned reviewer bin dirs.
        │
        ▼
python -m runner.skeptic_gate_cli ...
        │
        ▼
  a. Resolve authoritative API head SHA via `gh pr view`. Refuse if
     input SHA disagrees (defense against stale dispatch).
  b. Fetch PR diff via `gh pr diff`. Refuse if > MAX_DIFF_BYTES
     (no truncation).
  c. Defense-in-depth: re-check the diff size at the CLI level
     (cannot be bypassed by mocking `get_pr_diff`).
  d. Look up commit-subject prefix; map deterministically to
     implementation_identity (no ZFC keyword routing on free-form
     author/email text).
  e. Reject duplicate reviewer identities in `--reviewers-json`.
  f. Build prompt with implementation_identity inlined.
  g. Run codex (sandbox=read-only, --json, sanitized env,
     stdin-delivered prompt) AND gemini (-s, default approval,
     sanitized env, stdin-delivered prompt). 90s default timeout.
  h. parse_verdict — strict 6-field contract, no prose, no
     code-block smuggling, EXACTLY ONCE per field.
  i. bind_reviewer_identity — codex CLI must declare `codex`,
     gemini CLI must declare `gemini`.
  j. verify_provenance — implementation_identity ≠ reviewer_identity.
  k. aggregate_results — ALL reviewers must PASS; duplicate reviewers
     rejected.
  l. Pre-publish API head recheck — abort if changed mid-run.
  m. Post/upsert comment, set commit status. Status failure fails
     closed (not swallowed).
  n. Read back: verify ALL six fields equal what we wrote
     byte-for-byte (HEAD_SHA full 40-hex, REPO, PR_NUMBER, VERDICT,
     REVIEWER, IMPLEMENTATION_PROVENANCE). Fail closed if any
     disagree. Verify commit-status state matches.
```

## Trust posture

| Concern | Mitigation |
|---|---|
| PR head YAML forgeability | No `pull_request` trigger |
| PR-head Python RCE on credentialed runner | `sparse-checkout` from `github.event.repository.default_branch`; PR head is never checked out |
| Cross-repo caller cannot post to target repo | Same-target-repo caller enforced operationally (caller must live in `jleechanorg/dark-factory`); GITHUB_TOKEN scope mismatch fails at the first API call |
| `pull_request_target` cannot bootstrap | Trigger is absent; bootstrap requires the workflow file to already exist on `main` (which is what this PR adds) |
| Self-PASS via `workflow_dispatch` | Read-back requires `github-actions[bot]` actor; a human-dispatched run's token scope won't match the target repo |
| Mutable PATH binaries | Pinned absolute paths + version + SHA256 verified before invocation; workflow refuses to run on drift |
| Mutable `npm install -g` on credentialed runner | REMOVED; reviewer CLIs are pre-installed via the trusted runner image |
| Code-block / prompt-injection hiding a second VERDICT | `findall` requires EXACTLY ONE match per field + no-prose check rejects Markdown fences |
| Prose-around-verdict injection | No-prose check rejects any non-leading-comment line outside the contract fields |
| Identity impersonation (codex declares gemini) | `bind_reviewer_identity` rejects mismatched CLI/identity pairs |
| Duplicate reviewer (`codex`/`codex`) | `aggregate_results` + `_parse_reviewers` reject duplicates |
| Self-review (Claude reviews Claude's diff) | Codex AND Gemini required; deterministic commit-prefix provenance check on each |
| Stale dispatch (input SHA != API SHA) | Resolve API head SHA, refuse on mismatch |
| Mid-run push changing the verdict's SHA | Pre-publish API head recheck, abort on change |
| Reviewer process reading GITHUB_TOKEN | Sanitized env: allow-list only, secrets stripped at both workflow + CLI levels |
| Reviewer process reading HOME/shell rc | HOME/USER/SSH_AUTH_SOCK stripped from reviewer env |
| Reviewer process executing tools | codex `--sandbox=read-only`; gemini `-s` `--approval-mode=default` (no `yolo`) |
| E2BIG on argv from large diff | Diff is passed via stdin (`-` / `-p -`); never via argv |
| Partial review of an oversized diff | Hard 1 MiB cap, fail-closed; no truncation; defense-in-depth check at both `get_pr_diff` and CLI level |
| Bot identity spoofing after publish | Read-back verifies actor is `github-actions[bot]` |
| Status surface fraud | Read-back verifies `state` matches what we set; status-write failure fails closed |
| Comment body fraud | Read-back does FULL EQUALITY on all six fields (HEAD_SHA, REPO, PR_NUMBER, VERDICT, REVIEWER, IMPLEMENTATION_PROVENANCE), not just non-empty |
| Caller-supplied `trusted_ref` re-pinning to PR head | REMOVED — the workflow has NO `trusted_ref` input; the ref is implicitly pinned by the caller's `uses: ...@<SHA>` |
| Implementer identity derived from free-form author/email (ZFC keyword routing) | Replaced with deterministic commit-subject-prefix match; no keyword classification on free-form text |

## How to invoke manually

`workflow_dispatch` accepts:

- `pr_number` (required)
- `pr_sha` (optional override; if empty, re-resolved via `gh pr view`)

This is intended for **diagnostic verification only**. A diagnostic
run cannot satisfy gate-7 for a PR because the read-back step
requires a same-target-repo bot actor; a human-dispatched run's
token scope won't match the target repo, so the read-back fails
closed. **There is no self-PASS path.**

## Tests

`tests/test_skeptic_gate.py` covers 67 contract + adversarial cases:

| Case | Test(s) |
|---|---|
| Strict 6-field contract, exactly once | `test_parse_verdict_*` |
| No-prose / no-code-block contract | `test_adversarial_parse_rejects_*` |
| Stale SHA PASS rejected | `test_bind_to_pr_rejects_stale_sha` |
| Full equality read-back | `test_verify_published_comment_*`, `test_adversarial_readback_rejects_*` |
| Implementation/reviewer identity | `test_verify_provenance_*`, `test_adversarial_bind_reviewer_identity_*` |
| Commit-prefix provenance (deterministic) | `test_adversarial_commit_prefix_*` |
| Duplicate reviewer rejection | `test_adversarial_aggregate_rejects_duplicate_*`, `test_adversarial_cli_rejects_duplicate_reviewer_json` |
| Forced PASS acceptance (no credentials) | `test_forced_pass_acceptance_full_pipeline_binds_to_current_head` |
| Forced FAIL acceptance (no credentials) | `test_forced_fail_acceptance_full_pipeline_propagates_failure` |
| Multi-reviewer aggregation | `test_aggregate_results_*` |
| Read-back verification | `test_verify_published_comment_*` |
| Sandbox-mode flags present | `test_build_reviewer_cmd_*_sandbox` |
| Env sanitizer strips secrets (incl. HOME) | `test_reviewer_env_strips_secrets` |
| Status-failure fail-closed | `test_adversarial_status_failure_is_fail_closed` |
| Oversize diff fail-closed | `test_adversarial_diff_oversize_fails_closed` |
| Workflow has no `trusted_ref` input | `test_adversarial_workflow_has_no_trusted_ref_input` |
| Workflow pins reviewer binaries | `test_adversarial_workflow_pins_reviewer_binaries` |
| Workflow strips secrets before reviewer invocation | `test_adversarial_workflow_strips_secrets_before_reviewer_invocation` |
| CLI end-to-end paths | `test_cli_forced_pass_*`, `test_cli_forced_fail_*`, `test_cli_provenance_fails_self_review` |

Run:

```bash
python3 -m pytest tests/test_skeptic_gate.py -v
```

## Configuration

| Env / input | Default | Effect |
|---|---|---|
| `SKEPTIC_REVIEWERS_JSON` | `[["codex",""],["gemini","gemini-2.5-pro"]]` | Reviewers that must ALL PASS (must be distinct) |
| `SKEPTIC_STATUS_CONTEXT` | `skeptic` | Commit-status context name (the required-check name) |
| `SKEPTIC_EXPECTED_ACTOR` | `github-actions[bot]` | Bot actor expected on the published comment |
| `vars.SELF_HOSTED_RUNNER_LABELS` | unset → `["self-hosted","self-hosted-mikey"]` | Private repo runner selector |
| `vars.SKEPTIC_CODEX_BIN` | unset → `/opt/reviewers/codex/codex` | Pinned absolute path to codex binary |
| `vars.SKEPTIC_CODEX_VERSION` | unset → `` | Expected codex version string (e.g. `codex-cli 0.39.0`) |
| `vars.SKEPTIC_CODEX_SHA256` | unset → `` | Expected SHA256 of the codex binary |
| `vars.SKEPTIC_GEMINI_BIN` | unset → `/opt/reviewers/gemini/gemini` | Pinned absolute path to gemini binary |
| `vars.SKEPTIC_GEMINI_VERSION` | unset → `` | Expected gemini version string (e.g. `gemini-cli 0.5.4`) |
| `vars.SKEPTIC_GEMINI_SHA256` | unset → `` | Expected SHA256 of the gemini binary |
| `inputs.trusted_ref` | **REMOVED** | (Previously allowed caller to override the code ref; removed per post-audit comment 4953064910.) The code ref is now implicitly pinned by the caller's `uses: ...@<SHA>`. |

## Files

- `runner/skeptic_gate.py` — verdict-binding library (strict no-prose
  parser, provenance check, equality read-back, commit-prefix
  identity derivation)
- `runner/skeptic_gate_cli.py` — orchestrator (multi-reviewer,
  sanitized env, head-SHA equality, defense-in-depth diff size,
  stdin-only diff transport)
- `.github/workflows/skeptic-gate.yml` — workflow (`workflow_call`
  target only; no `pull_request` / `pull_request_target`; pinned
  reviewer binaries; secrets stripped before reviewer invocation)
- `tests/test_skeptic_gate.py` — 67 contract + adversarial tests
- `docs/skeptic-gate.md` — this file