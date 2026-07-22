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

```bash
git show abfcee8^:docs/skeptic-gate.md       # this file pre-deletion
git show abfcee8^:.github/workflows/skeptic-gate.yml
git show abfcee8^:.github/workflows/skeptic-gate-caller.yml
git show abfcee8^:runner/skeptic_gate.py
git show abfcee8^:runner/skeptic_gate_cli.py
git show abfcee8^:tests/test_skeptic_gate.py
```

If you need the runner-side helpers (`parse_verdict`,
`aggregate_results`, `--force-pass` / `--force-fail` diagnostic
modes, commit-prefix identity derivation), they were moved into the
parallel-reviewer receipt gate in PR #407. The reproduction-receipt
gate is the documented successor.

## Tests

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