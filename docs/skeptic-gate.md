# Skeptic Gate (issue #278)

The Skeptic Gate is the dark-factory 7-green policy's **gate 7**. It
guarantees that every merged PR has an independent, SHA-bound review
verdict from a model that did **not** write the diff.

## Why it exists

Before issue #278, the 7-green policy required a `github-actions[bot]`
comment containing `VERDICT: PASS` at the PR head SHA — but no
workflow produced one. The Skeptic Gate adds that workflow and binds
the verdict to the exact commit it reviewed.

## Headline invariant

> **A stale-SHA PASS must never satisfy a newer head.**

The verdict is bound to repository, PR number, and exact head SHA. If
the PR head changes, the gate re-runs and the new head gets a fresh
verdict. A previous PASS for an older head is visually marked
`STALE` in the bot comment and is rejected by `bind_to_pr`.

## Architecture

```
PR open / synchronize / reopened / edited
        │
        ▼
.github/workflows/skeptic-gate.yml
        │  (self-hosted runner; private repo selector)
        ▼
runner/skeptic_gate_cli.py
        │  (orchestrator — gathers PR context via `gh`, builds prompt)
        ▼
non-Claude reviewer CLI  ←  gemini (default) or codex
        │  (emits structured VERDICT: PASS|FAIL + HEAD_SHA)
        ▼
runner/skeptic_gate.evaluate
        │  (ZFC: deterministic binding only — no semantic judgment)
        ▼
┌──────────────────────────────────────────────────────────┐
│  1. parse_verdict  →  struct { verdict, sha, repo, pr }   │
│  2. bind_to_pr     →  reject if SHA/REPO/PR mismatch     │
│  3. format_comment →  render idempotent bot comment       │
│  4. gh api upsert  →  find prior by MARKER, PATCH or POST │
│  5. gh api status  →  set "skeptic" commit status         │
└──────────────────────────────────────────────────────────┘
```

## ZFC compliance

The reviewer (gemini or codex — never the implementing model) is the
only place that judges the diff. `runner/skeptic_gate.py` is **only**
allowed to validate the structured verdict (`VERDICT:` / `HEAD_SHA:`
lines) and bind the SHA. There is no keyword routing, no scoring, no
semantic classification of the diff. Failures flow from missing or
malformed structured fields, never from this code's opinion of the
diff.

## Required status check

The job name is `skeptic` and the commit-status context is `skeptic`
(overridable via `SKEPTIC_STATUS_CONTEXT`). Repo admins can require
`skeptic` in the merge-protection "Required status checks" list. Until
the workflow runs successfully on the current head SHA, the check is
not `success` and merge is blocked.

## Manual rerun

`workflow_dispatch` accepts:

- `pr_number` (required) — the PR to (re-)run the gate on.
- `pr_sha` (optional) — head SHA override. If empty, the workflow
  re-resolves via `gh pr view` so stale dispatch inputs are corrected.

## Tests

`tests/test_skeptic_gate.py` covers the contract end-to-end:

| Case | Test |
|---|---|
| PASS | `test_evaluate_pass_path_yields_success_state` |
| FAIL | `test_evaluate_fail_path_yields_failure_state` |
| Stale SHA | `test_evaluate_stale_sha_pass_yields_failure_not_success` |
| Missing reviewer | `test_evaluate_missing_reviewer_yields_error_state` |
| Malformed output | `test_evaluate_malformed_output_yields_failure_state` |
| Duplicate reruns | `test_evaluate_duplicate_rerun_same_sha_produces_same_marker` |

Run:

```bash
python3 -m pytest tests/test_skeptic_gate.py -v
```

## Configuration

| Env var | Default | Effect |
|---|---|---|
| `SKEPTIC_REVIEWER` | `gemini` | Reviewer CLI to invoke (`gemini` or `codex`) |
| `SKEPTIC_REVIEWER_MODEL` | `gemini-2.5-pro` | Model name passed to the CLI |
| `SKEPTIC_STATUS_CONTEXT` | `skeptic` | Commit-status context name (the required-check name) |

## Files

- `runner/skeptic_gate.py` — pure verdict-binding library
- `runner/skeptic_gate_cli.py` — workflow-facing orchestrator
- `.github/workflows/skeptic-gate.yml` — the gate itself
- `tests/test_skeptic_gate.py` — 34 contract tests
- `docs/skeptic-gate.md` — this file
