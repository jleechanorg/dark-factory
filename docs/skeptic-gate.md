# Skeptic Gate (post-deletion reference, issue #427)

The Skeptic Gate **no longer runs as a GitHub Actions workflow**.
PR #407 (`abfcee8` — "feat(runner): reproduction-receipt gate for parallel
reviewer verdicts + delete failing skeptic-gate GH workflow") removed
`.github/workflows/skeptic-gate.yml` and its bootstrap caller because
the workflow's pre-conditions could not be satisfied inside the dark-factory
PR-merge path: it required a same-target-repo caller pinned by SHA, but no
such caller existed, so the gate was never actually publishing verdicts on
real PRs. The runner-side Python (`runner/skeptic_gate.py`,
`runner/skeptic_gate_cli.py`) was kept for the reproduction-receipt gate
introduced in the same PR.

## Where Skeptic lives now

Skeptic is **gate 7** of the dark-factory 8-Green verifier plane. It is
enforced by the daemon, not by a GH workflow:

- `daemon/src/verifier.rs::GateName::Skeptic` — the canonical enum variant
  in the `GateReport.results` array of fixed length **8**. The Skeptic gate
  consumes the recorded `SkepticVerdict` (`pass | warn | fail`) on the
  `PrEvidence` struct, where `warn` is non-blocking (per spec §4.2.5).
- `daemon/src/skeptic_evidence.rs` — the verdict-collection path the
  verifier reads from. Contract-echo semantics (issue #386) live here:
  the daemon records per-item verdicts against the governing bead's
  acceptance criteria and enforces `PriorFinding` echo so the gate
  cannot silently drop prior findings (PR #412 / bead `jleechan-ijod`).
- `daemon/src/verifier.rs::parse_skeptic_verdict` — the strict no-prose
  marker parser. ZFC note: this is **not** judgment; the LLM emits the
  marker, this function only matches the `verdict:` token.
- `daemon/factory-overlay.sh::REQUIRED_KEYS` — the contract pinning the
  canonical key set:
  `{"ci_green","no_conflicts","coderabbit","bugbot","comments_resolved","evidence_review","skeptic","vacuous_red_green"}`.
  The Skeptic key must be present in every `GATE_ASSESSMENT` payload.
- Runner pipeline node `_gate_skeptic` (`pipelines/factory/level5_feature.dot`
  and the slim variants) — the in-pipeline reviewer lane that shells
  out to the `gate_skeptic` handler. The handler reads the recorded
  verdict from `ctx.state["<node>.outcome"]` and is wired into the
  `fix [max_visits=…]` loop per the standard Level-5 topology.

The 8 named gates, in `GateReport.results` order, are:

1. `ci_green` — CI workflow success
2. `no_conflicts` — PR mergeable_state
3. `coderabbit` — CodeRabbit verdict APPROVED
4. `bugbot` — Bugbot clean (zero error-severity)
5. `comments_resolved` — inline-review threads resolved
6. `evidence_review` — `/er` Evidence review PASS (gate 6, parent of
   `.github/workflows/evidence-gate.yml`)
7. `skeptic` — daemon Skeptic verdict (this gate)
8. `vacuous_red_green` — runtime vacuous-test detector (added by
   PR #387 / bead `jleechan-ijod`)

## What replaced the GH workflow

- The **reproduction-receipt gate** (PR #407) is the
  `parallel_reviewer`-side receipt that proves the reviewer actually
  executed the diff (test runs, lint runs, file cites, HEAD SHA verify).
  This is the gate the GH workflow was supposed to enforce, but failed
  to fire because the bootstrap caller never existed. The runner-side
  receipt is enabled per-lane via `receipt_required` and gated
  pre-aggregation (PR #425, bead `jleechan-9s7u`).
- The **Evidence Gate** (`.github/workflows/evidence-gate.yml`, PR #424)
  is the remaining GHA-side enforcement that binds to **independent
  ground truth** — an `/er` verdict comment OR a canonical
  `**Evidence**: <gist-url>` marker — and fails closed when neither is
  present. Issue #424 closed the regression where the gate could
  self-certify a passing template string.

## Headline invariants (carried over from the deleted workflow)

> **1. Skeptic is a daemon-side gate; no GH workflow fires it.**
> **2. The recorded Skeptic verdict is consumed by `parse_skeptic_verdict` (raw-output parsing of the marker `verdict: pass|warn|fail`). `parse_skeptic_verdict` is NOT judgment — it only matches the marker token and returns the verdict plus the remainder of the body. The `verifier::skeptic_gate` helper then maps `Pass`/`Warn` → `Green` and `Fail` → `Red`. `pass|warn` is green only when `review_degraded` is `false`.**
> **3. `assess` may override `Pass`/`Warn` to `Red` when `review_degraded` is `true` (bead `jleechan-984e` / issue #385, PR #394). The override is enforced at the `assess` site (after `assess` has the full `PrEvidence` and can attribute the reason to the cross-model failure rather than to the gate-7 verdict itself); doing it inside `skeptic_gate` would force that helper to know about `review_degraded` AND about the Stage-1 `mock_llm` exemption (already encoded in `compute_review_degraded`'s empty-family filter). The assess-site wiring keeps `skeptic_gate` verdict-only.**
> **4. The Skeptic key MUST appear in every `GATE_ASSESSMENT` payload (pinned by `daemon/factory-overlay.sh::REQUIRED_KEYS`).**
> **5. The Skeptic gate never executes PR-head Python on the credentialed runner — the reviewer is a daemon-side subprocess with allow-listed env (no `GITHUB_TOKEN`, no `HOME`, no `OPENCLAW_*` / `HERMES_*` / `SLACK_*` secrets).**
> **6. Skeptic verdicts without execution-evidence fields (`TEST_RUN_EVIDENCE`, `LINT_RUN_EVIDENCE`, `GREP_CITES`, `HEAD_COMMIT_VERIFIED`) are rejected as evidence-free (fail-closed, issue #384).**

## Historical content

The pre-deletion GH workflow design — bootstrap dependency, headline
invariants, architecture diagram, trust-posture table, `workflow_dispatch`
semantics, configuration table, and the 79-case test catalogue — is
preserved in git history at `abfcee8^` (the parent of the deletion).
Inspect with:

<<<<<<< HEAD
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
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
> **8. Every verdict MUST include execution-evidence fields
>    (TEST_RUN_EVIDENCE, LINT_RUN_EVIDENCE, GREP_CITES,
>    HEAD_COMMIT_VERIFIED) — pattern-matched PASS verdicts without
>    proof the reviewer ran tests+lint+grep on the PR HEAD are
>    evidence-free and the gate rejects them (issue #384).**
<<<<<<< HEAD
=======
>>>>>>> 22c6eec ([antig] feat(ci): add SHA-bound skeptic gate workflow for 7-green (#281))
=======
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
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
   Resolve caller's commit SHA on the default branch
        │
        ▼
   uses: jleechanorg/dark-factory/.github/workflows/skeptic-gate.yml@<PINNED-SHA>
        │  with inputs pr_number, pr_sha, trusted_code_sha;
        │  secret github_token
        │
        ▼
   .github/workflows/skeptic-gate.yml
        │  (self-hosted runner; private repo selector
        │   via fromJson(vars.SELF_HOSTED_RUNNER_LABELS))
        ▼
0. "Verify pinned reviewer binaries"  ← path + version + sha256
   against repo variables. Drift fails the gate (no mutable install).
   All six SKEPTIC_*_BIN / _VERSION / _SHA256 vars are mandatory —
   absence is fatal.
        │
        ▼
1. "Validate trusted_code_sha is a 40-hex string"  ← derives the
   trusted ref from `inputs.trusted_code_sha` (workflow_call) or
   `github.sha` (workflow_dispatch). Refuses to proceed otherwise.
        │
        ▼
2. "Checkout gate code from IMMUTABLE trusted_code_sha"  ←
   sparse-checkout (runner/skeptic_gate.py + runner/skeptic_gate_cli.py
   from the EXACT pinned SHA, NEVER from the moving default branch
   or PR head).
        │
        ▼
3. "Verify the checked-out HEAD equals trusted_code_sha"  ←
   defense-in-depth: refuses to run if the local HEAD disagrees with
   the input.
        │
        ▼
4. "Verify caller actor"  ← refuses non-github-actions[bot] actors
   for self-PASS (a human-dispatched workflow_dispatch cannot satisfy
   the read-back step's actor equality check).
        │
        ▼
5. "Run skeptic gate" with secrets stripped from the env (HOME,
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
<<<<<<< HEAD
<<<<<<< HEAD
  h. parse_verdict — strict 10-field contract (6 base + 4 execution
     evidence per issue #384), no prose, no code-block smuggling,
     EXACTLY ONCE per field.
  i. bind_reviewer_identity — codex CLI must declare `codex`,
     gemini CLI must declare `gemini`.
  j. verify_provenance — implementation_identity ≠ reviewer_identity.
  k. Execution-evidence consistency: TEST_RUN_EVIDENCE must show
     `failed=0 exit=0`; LINT_RUN_EVIDENCE must show `errors=0`;
     GREP_CITES must contain ≥1 `path:LINE` cite; HEAD_COMMIT_VERIFIED
     must equal HEAD_SHA byte-for-byte.
  l. aggregate_results — ALL reviewers must PASS with non-vacuous
     execution evidence; duplicate reviewers rejected; vacuous
     reviewer (no execution evidence) treated as fail.
  m. Pre-publish API head recheck — abort if changed mid-run.
  n. Post/upsert comment, set commit status. Status failure fails
     closed (not swallowed).
  o. Read back: verify ALL six fields equal what we wrote
=======
  h. parse_verdict — strict 6-field contract, no prose, no
     code-block smuggling, EXACTLY ONCE per field.
=======
  h. parse_verdict — strict 10-field contract (6 base + 4 execution
     evidence per issue #384), no prose, no code-block smuggling,
     EXACTLY ONCE per field.
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
  i. bind_reviewer_identity — codex CLI must declare `codex`,
     gemini CLI must declare `gemini`.
  j. verify_provenance — implementation_identity ≠ reviewer_identity.
  k. Execution-evidence consistency: TEST_RUN_EVIDENCE must show
     `failed=0 exit=0`; LINT_RUN_EVIDENCE must show `errors=0`;
     GREP_CITES must contain ≥1 `path:LINE` cite; HEAD_COMMIT_VERIFIED
     must equal HEAD_SHA byte-for-byte.
  l. aggregate_results — ALL reviewers must PASS with non-vacuous
     execution evidence; duplicate reviewers rejected; vacuous
     reviewer (no execution evidence) treated as fail.
  m. Pre-publish API head recheck — abort if changed mid-run.
  n. Post/upsert comment, set commit status. Status failure fails
     closed (not swallowed).
<<<<<<< HEAD
  n. Read back: verify ALL six fields equal what we wrote
>>>>>>> 22c6eec ([antig] feat(ci): add SHA-bound skeptic gate workflow for 7-green (#281))
=======
  o. Read back: verify ALL six fields equal what we wrote
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
     byte-for-byte (HEAD_SHA full 40-hex, REPO, PR_NUMBER, VERDICT,
     REVIEWER, IMPLEMENTATION_PROVENANCE). Fail closed if any
     disagree. Verify commit-status state matches.
=======
```bash
git show abfcee8^:docs/skeptic-gate.md       # this file pre-deletion
git show abfcee8^:.github/workflows/skeptic-gate.yml
git show abfcee8^:.github/workflows/skeptic-gate-caller.yml
git show abfcee8^:runner/skeptic_gate.py
git show abfcee8^:runner/skeptic_gate_cli.py
git show abfcee8^:tests/test_skeptic_gate.py
>>>>>>> 0c21e07 (claude/antig: docs(runner): gate-count + skeptic-reference consistency sweep (#427))
```

If you need the runner-side helpers (`parse_verdict`,
`aggregate_results`, `--force-pass` / `--force-fail` diagnostic
modes, commit-prefix identity derivation), they were moved into the
parallel-reviewer receipt gate in PR #407. The reproduction-receipt
gate is the documented successor.

## Tests

<<<<<<< HEAD
<<<<<<< HEAD
<<<<<<< HEAD
`tests/test_skeptic_gate.py` covers **109** contract + adversarial cases:

| Case | Test(s) |
|---|---|
| Strict 10-field contract (6 base + 4 execution-evidence), exactly once | `test_parse_verdict_*` |
| Execution-evidence fields required (issue #384) | `test_parse_verdict_rejects_missing_execution_evidence_field`, `test_parse_verdict_rejects_test_run_evidence_when_tests_failed`, `test_parse_verdict_rejects_test_run_evidence_when_exit_nonzero`, `test_parse_verdict_rejects_lint_run_evidence_with_errors`, `test_parse_verdict_rejects_grep_cites_empty`, `test_parse_verdict_rejects_head_commit_verified_mismatch`, `test_parse_verdict_rejects_head_commit_verified_short_sha`, `test_parse_verdict_rejects_duplicate_test_run_evidence` |
| Vacuous regression fixture caught (issue #384 acceptance) | `test_vacuous_regression_fixture_rejected_by_gate`, `test_vacuous_regression_fixture_with_fake_test_counts_still_rejected` |
| Aggregator rejects vacuous reviewer verdicts (issue #384) | `test_aggregate_results_rejects_when_only_one_reviewer_has_evidence` |
| Prompt requires execution-evidence fields | `test_build_prompt_requires_execution_evidence_fields` |
=======
`tests/test_skeptic_gate.py` covers **79** contract + adversarial cases:

| Case | Test(s) |
|---|---|
| Strict 6-field contract, exactly once | `test_parse_verdict_*` |
>>>>>>> 22c6eec ([antig] feat(ci): add SHA-bound skeptic gate workflow for 7-green (#281))
=======
`tests/test_skeptic_gate.py` covers **109** contract + adversarial cases:

| Case | Test(s) |
|---|---|
| Strict 10-field contract (6 base + 4 execution-evidence), exactly once | `test_parse_verdict_*` |
| Execution-evidence fields required (issue #384) | `test_parse_verdict_rejects_missing_execution_evidence_field`, `test_parse_verdict_rejects_test_run_evidence_when_tests_failed`, `test_parse_verdict_rejects_test_run_evidence_when_exit_nonzero`, `test_parse_verdict_rejects_lint_run_evidence_with_errors`, `test_parse_verdict_rejects_grep_cites_empty`, `test_parse_verdict_rejects_head_commit_verified_mismatch`, `test_parse_verdict_rejects_head_commit_verified_short_sha`, `test_parse_verdict_rejects_duplicate_test_run_evidence` |
| Vacuous regression fixture caught (issue #384 acceptance) | `test_vacuous_regression_fixture_rejected_by_gate`, `test_vacuous_regression_fixture_with_fake_test_counts_still_rejected` |
| Aggregator rejects vacuous reviewer verdicts (issue #384) | `test_aggregate_results_rejects_when_only_one_reviewer_has_evidence` |
| Prompt requires execution-evidence fields | `test_build_prompt_requires_execution_evidence_fields` |
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
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

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
### Execution-evidence contract (issue #384)

Pattern-matched PASS verdicts slipped vacuous regression tests past the
gate in PR #382 and fail-open paths in PR #365 r5. To prevent this,
the skeptic gate now requires every reviewer to actually execute the
repo's tests+lint+grep on the PR HEAD and report the result in the
verdict. Verdicts without any of the four execution-evidence fields are
rejected as evidence-free (fail-closed):

- `TEST_RUN_EVIDENCE` — `passed=N failed=N skipped=N exit=N`. The
  reviewer must run the repo's primary test command (e.g. `pytest`,
  `cargo test`) and report the real counts. `failed>0` or `exit!=0`
  → verdict rejected.
- `LINT_RUN_EVIDENCE` — `tool=<name> errors=N warnings=N`. The
  reviewer must run the repo's primary linter (e.g. `ruff`, `clippy`)
  and report real counts. `errors>0` → verdict rejected.
- `GREP_CITES` — `path/to/file.py:LINE;...`. The reviewer must cite
  the production call site AND test for each enforcement claim made
  in the diff. Empty citations or separator-only values (e.g. `;`)
  → verdict rejected.
- `HEAD_COMMIT_VERIFIED` — the full 40-hex SHA of the local HEAD
  the reviewer actually exercised. Must equal `HEAD_SHA` byte-for-byte
  → mismatch → verdict rejected.

The aggregator additionally refuses to produce a PASS unless every
mandatory reviewer submitted non-vacuous execution evidence. A
reviewer whose `ParsedVerdict` has `test_run_evidence is None`,
`lint_run_evidence is None`, empty `grep_cites`, or empty
`head_commit_verified` is treated as if it had failed — the PR is
not promoted.

<<<<<<< HEAD
=======
>>>>>>> 22c6eec ([antig] feat(ci): add SHA-bound skeptic gate workflow for 7-green (#281))
=======
>>>>>>> a6c9078 (claude/fable: fix(daemon): skeptic gate execution-evidence contract (issue #384) (#390))
Run:

```bash
python3 -m pytest tests/test_skeptic_gate.py -v
```

## Configuration

All `vars.*` and `inputs.*` listed below are **required** — there are
no defaults. Absence is fatal at the gate's "Verify mandatory pin
vars are set" step.

| Env / input | Default | Effect |
|---|---|---|
| `SKEPTIC_REVIEWERS_JSON` | `[["codex",""],["gemini","gemini-2.5-pro"]]` | Reviewers that must ALL PASS (must be distinct, must be exactly codex + gemini) |
| `SKEPTIC_STATUS_CONTEXT` | `skeptic` | Commit-status context name (the required-check name) |
| `SKEPTIC_EXPECTED_ACTOR` | `github-actions[bot]` | Bot actor expected on the published comment |
| `vars.SELF_HOSTED_RUNNER_LABELS` | **required** | Private repo runner selector (JSON array; must be wrapped in `fromJson()`) |
| `vars.SKEPTIC_CODEX_BIN` | **required** | Pinned absolute path to codex binary |
| `vars.SKEPTIC_CODEX_VERSION` | **required** | Expected codex version string (e.g. `codex-cli 0.39.0`) |
| `vars.SKEPTIC_CODEX_SHA256` | **required** | Expected SHA256 of the codex binary |
| `vars.SKEPTIC_GEMINI_BIN` | **required** | Pinned absolute path to gemini binary |
| `vars.SKEPTIC_GEMINI_VERSION` | **required** | Expected gemini version string (e.g. `gemini-cli 0.5.4`) |
| `vars.SKEPTIC_GEMINI_SHA256` | **required** | Expected SHA256 of the gemini binary |
| `inputs.trusted_code_sha` | **required** (workflow_call) | 40-hex SHA the caller pinned the workflow ref to. The "Validate trusted_code_sha" step refuses to proceed unless this is a 40-hex string; for dispatch runs it falls back to `github.sha` (the workflow's own commit SHA on the default branch). |
| `inputs.trusted_ref` | **REMOVED** | (Previously allowed caller to override the code ref; removed per post-audit comment 4953064910.) The code ref is now implicitly pinned by the caller's `uses: ...@<SHA>` and the gate's own SHA equality check. |

## Files

- `runner/skeptic_gate.py` — verdict-binding library (strict no-prose
  parser, provenance check, equality read-back, commit-prefix
  identity derivation, mandatory-Codex+Gemini aggregation)
- `runner/skeptic_gate_cli.py` — orchestrator (multi-reviewer,
  sanitized env, head-SHA equality, defense-in-depth diff size,
  stdin-only diff transport, per-reviewer credential isolation,
  --repo binding for diff capture)
- `.github/workflows/skeptic-gate.yml` — workflow (`workflow_call`
  target only; no `pull_request` / `pull_request_target`; pinned
  reviewer binaries; secrets stripped before reviewer invocation;
  no `workflow_dispatch` `trusted_code_sha` input)
- `.github/workflows/skeptic-gate-caller.yml` — bootstrap caller
  (`pull_request_target` + `workflow_dispatch`); resolves the
  caller's commit SHA on the default branch and invokes the gate
  via `uses: ...@<PINNED-SHA>` at the job level (reusable-workflow
  reference must be a static literal)
- `tests/test_skeptic_gate.py` — **79** contract + adversarial tests
- `docs/skeptic-gate.md` — this file
=======
The contract + adversarial tests for the runner-side Skeptic helpers
that survived the deletion live alongside the reproduction-receipt
tests. The daemon-side verdict parsing is pinned by:

- `tests/test_af_gate_contract.py::extract_gate_names_from_rust` —
  asserts the canonical `gate_map` matches `GateName::as_str()` and
  `REQUIRED_KEYS` in `daemon/factory-overlay.sh`.
- `tests/scripts/test_auto_merge_guard_gate_vocabulary.sh` — fixture
  keys must match the same canonical vocabulary.
- The Skeptic-specific verdict contract is exercised by the
  `skeptic_evidence` integration tests under `daemon/tests/`.

If you need to add a Skeptic-related gate or vocabulary change, the
canonical gate vocabulary lives in **three** places that MUST be
updated together (per the canonical-source comment in
`daemon/src/verifier.rs::GateName::as_str`):

1. `daemon/src/verifier.rs::GateName` enum + `as_str()` match arms
2. `daemon/factory-overlay.sh::REQUIRED_KEYS`
3. `tests/test_af_gate_contract.py::extract_gate_names_from_rust`
   `gate_map`
>>>>>>> 0c21e07 (claude/antig: docs(runner): gate-count + skeptic-reference consistency sweep (#427))
