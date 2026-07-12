# Skeptic Gate (issue #278, mandatory redesign)

The Skeptic Gate is the dark-factory 7-green policy's **gate 7**. It
guarantees that every merged PR has an independent, SHA-bound review
verdict from a non-Claude reviewer (Codex AND Gemini must both PASS),
written by code that **never** executes PR-head Python on the
credentialed runner.

## Bootstrap dependency (READ THIS BEFORE MERGING)

This PR cannot self-certify gate-7 from its own workflow. The
`pull_request` trigger is forgeable (PR head controls the YAML), and
`pull_request_target` cannot bootstrap until the workflow file is
already on the default branch — which is what this PR is adding.

**The only legitimate bootstrap is via a separate trusted external
caller.** Concretely:

> Add this workflow to `jleechanorg/dark-factory`'s default branch.
> Then have `jleechanorg/.github` (Hermes reusable workflow, already
> trusted by the wider operator fleet) call this workflow via
> `uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@main`
> from a `pull_request_target`-triggered caller. The caller passes
> `pr_number`, `pr_sha` (optional), and `trusted_ref` (pinned to a
> known-good SHA) as inputs and forwards `secrets.GITHUB_TOKEN`.

Until that bootstrap exists:

- This workflow is NOT triggered by `pull_request` (forgeable).
- This workflow is NOT triggered by `pull_request_target` (cannot
  bootstrap, see above).
- This workflow CAN be invoked via `workflow_call` from a trusted
  caller.
- This workflow CAN be invoked manually via `workflow_dispatch` for
  diagnostic verification.

Gate-7 status for PR #281 is therefore NOT proven by this PR's own
workflow. The dark-factory 7-green policy's gate-7 depends on the
external trusted caller being in place.

## Why this exists (and what the audit demanded)

Before the redesign, the gate's own workflow ran PR-head Python on a
self-hosted runner with full GITHUB_TOKEN. An attacker controlling a
PR's Python code would have read+write to secrets. The redesigned
implementation:

- Drops the `pull_request` and `pull_request_target` triggers
  (`pull_request` is forgeable; `pull_request_target` cannot bootstrap).
- Removes the `npm install -g` reviewer-install step (mutable, un-pinned).
- Becomes a `workflow_call` target invoked by an external trusted
  caller (Hermes reusable workflow in `jleechanorg/.github`).
- Always sparse-checkouts the gate code from `inputs.trusted_ref`
  (default branch by default), never from PR head.
- Validates the structured verdict twice (Codex + Gemini), each from
  a different identity than the implementer.
- Re-reads its own writes (comment + commit-status API surface)
  before declaring success.

## Headline invariants

> **1. A stale-SHA PASS must never satisfy a newer head.**
> **2. PR-head Python is never executed on the credentialed runner.**
> **3. Both reviewers must PASS; one FAIL or one unavailable → gate FAIL.**
> **4. The reviewer identity must differ from the implementer identity.**
> **5. The published comment must be authored by `github-actions[bot]`
>    and re-verified by the gate.**
> **6. The reviewer CLIs must be pre-installed via a pinned/trusted
>    bootstrap; this workflow NEVER installs them on the runner.**

## Architecture

```
Trusted external caller (Hermes reusable workflow)
   pull_request_target on jleechanorg/dark-factory main
        │
        ▼
   uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@main
        │  with inputs pr_number, pr_sha, trusted_ref, secret github_token
        │
        ▼
   .github/workflows/skeptic-gate.yml
        │  (self-hosted runner; private repo selector
        │   via fromJson(vars.SELF_HOSTED_RUNNER_LABELS || '["self-hosted","self-hosted-mikey"]'))
        ▼
1. "Verify reviewer CLIs are pre-installed"  ← observational only,
   no install (mutable installs are forbidden here).
        │
        ▼
2. "Checkout gate code from TRUSTED ref"  ← sparse-checkout
   (runner/skeptic_gate.py + runner/skeptic_gate_cli.py from
    inputs.trusted_ref || default branch; NEVER from PR head)
        │
        ▼
python -m runner.skeptic_gate_cli ...
        │
        ▼
  a. Resolve authoritative API head SHA via `gh pr view`. Refuse if
     input SHA disagrees (defense against stale dispatch).
  b. Fetch PR diff via `gh pr diff`. Refuse if > 1 MiB (no truncation).
  c. Look up commit author identity (claude/codex/gemini/unknown).
  d. Build prompt with implementation_identity inlined.
  e. Run codex (sandbox=read-only, --json, sanitized env)
     AND gemini (-s, default approval mode, sanitized env).
  f. parse_verdict — 6 fields, EXACTLY once each.
     Stale-SHA, duplicate fields, missing fields → reject.
  g. verify_provenance — implementation_identity ≠ reviewer_identity.
  h. aggregate_results — ALL reviewers must PASS.
  i. Pre-publish API head recheck — abort if changed mid-run.
  j. Post/upsert comment, set commit status.
  k. Read back: verify actor, marker, SHA, repo, PR number, verdict
     on both the comment and the commit-status API surface.
```

## Trust posture

| Concern | Mitigation |
|---|---|
| PR head YAML forgeability | No `pull_request` trigger |
| PR-head Python RCE on credentialed runner | `sparse-checkout` from `inputs.trusted_ref`; `pull_request_target` only via external caller |
| Self-review (Claude reviews Claude's diff) | Codex AND Gemini required; provenance check on each |
| Code-block / prompt-injection hiding a second VERDICT | `findall` requires EXACTLY ONE match per field |
| Stale dispatch (input SHA != API SHA) | Resolve API head SHA, refuse on mismatch |
| Mid-run push changing the verdict's SHA | Pre-publish API head recheck, abort on change |
| Reviewer process reading GITHUB_TOKEN | Sanitized env: allowlist-only |
| Reviewer process executing tools | codex `--sandbox=read-only`; gemini `-s` (no `yolo`) |
| Mutable `npm install -g` on credentialed runner | REMOVED; reviewer CLIs must be pre-installed via pinned/trusted bootstrap |
| Partial review of an oversized diff | Hard 1 MiB cap, fail-closed; no truncation |
| Bot identity spoofing after publish | Read-back verifies actor is `github-actions[bot]` |
| Status surface fraud | Read-back verifies `state` matches what we set |

## How to invoke manually

`workflow_dispatch` accepts:

- `pr_number` (required)
- `pr_sha` (optional override; if empty, re-resolved via `gh pr view`)
- `trusted_ref` (optional; defaults to the repo's default branch)

This is intended for **diagnostic verification only** — it does NOT
satisfy gate-7 for a PR because the PR's own invocation is not from
a trusted external caller.

## Tests

`tests/test_skeptic_gate.py` covers the contract end-to-end (46 tests):

| Case | Test(s) |
|---|---|
| Strict 6-field contract, exactly once | `test_parse_verdict_*` |
| Stale SHA PASS rejected | `test_bind_to_pr_rejects_stale_sha` |
| Implementation/reviewer identity | `test_verify_provenance_*` |
| Forced PASS acceptance (no credentials) | `test_forced_pass_acceptance_full_pipeline_binds_to_current_head` |
| Forced FAIL acceptance (no credentials) | `test_forced_fail_acceptance_full_pipeline_propagates_failure` |
| Multi-reviewer aggregation | `test_aggregate_results_*` |
| Read-back verification | `test_verify_published_comment_*` |
| Sandbox-mode flags present | `test_build_reviewer_cmd_*_sandbox` |
| Env sanitizer strips secrets | `test_reviewer_env_strips_secrets` |
| CLI end-to-end paths | `test_cli_forced_pass_*`, `test_cli_forced_fail_*`, `test_cli_provenance_*` |

Run:

```bash
python3 -m pytest tests/test_skeptic_gate.py -v
```

## Configuration

| Env / input | Default | Effect |
|---|---|---|
| `SKEPTIC_REVIEWERS_JSON` | `[["codex",""],["gemini","gemini-2.5-pro"]]` | Reviewers that must ALL PASS |
| `SKEPTIC_STATUS_CONTEXT` | `skeptic` | Commit-status context name (the required-check name) |
| `vars.SELF_HOSTED_RUNNER_LABELS` | unset → `["self-hosted","self-hosted-mikey"]` | Private repo runner selector |
| `inputs.trusted_ref` | empty (→ default branch) | Ref to sparse-checkout gate code from |

## Files

- `runner/skeptic_gate.py` — verdict-binding library (strict parser,
  provenance check, read-back verifier)
- `runner/skeptic_gate_cli.py` — orchestrator (multi-reviewer,
  sanitized env, head-SHA equality, read-back)
- `.github/workflows/skeptic-gate.yml` — workflow (`workflow_call` +
  `workflow_dispatch`; no `pull_request` / `pull_request_target`)
- `tests/test_skeptic_gate.py` — 46 contract tests
- `docs/skeptic-gate.md` — this file
