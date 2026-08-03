---
name: reviewer-calibration
description: "Use when auditing the canonical Codex-only /f controller receipt against one frozen target."
---

# Reviewer Calibration

Use this skill when `/f`, `/factory`, or a PR/work-item review must prove that
the canonical Codex-only controller reviewed one frozen target.

## Rule

Calibration is default-on for real `/f` runs. Treat
`--reviewer-calibration=true` as present unless the user explicitly passes
`--reviewer-calibration=false` and gives a reason.

This is a receipt-integrity audit of one controller-owned Codex review, not a
multi-backend comparison. Do not substitute another backend or author review
instructions outside the controller.

## Frozen Envelope

Create a task file containing only the untrusted PR/work-item text. Determine:

- `target_repo`
- `target_pr` or `work_item`
- `head_sha`
- `base_sha`
- `diff_path` or embedded diff hash
- PR body / task text snapshot
- evidence artifact paths and hashes
- test log paths and hashes
- factory `run_id`
- Codex lane output directory

Do not create reviewer instructions. The controller constructs and binds the
diff, changed files, task snapshot, evidence metadata, static prompt, and exact
response contract.

## Controller command

Run this exact binary-owned interface:

```bash
dark-factory review \
  --workdir <repo> \
  --base-sha <full-40-hex-sha> \
  --head-sha <full-40-hex-sha> \
  --task-file <path> \
  --output-dir <dir> \
  --backend codex
```

Do not add model names, inline prompts, or vendor CLI flags. The ordinary graph shadow
review is separate: non-controller graph nodes may use independent
shadow reviewers, while controller-owned review suppresses ambient shadows.

## Artifacts

The binary, not this skill, writes each lane directory:

```text
evidence/<run-id>/reviewer-calibration/
  codex/
    controller-receipt.json
    envelope.json
    prompt.txt
    transport.jsonl
    reviewer.output.md
    findings.json
  audit.json
```

`prompt.txt` is a binary-emitted audit capture only. It is not prompt
authority. Never author it in a skill/workflow, copy it from the target repo,
or pass it to a vendor CLI. The source-root-pinned controller template and the
receipt digest are authoritative.

Before using a lane, require `controller-receipt.json` to bind:

- controller prompt ID and prompt SHA-256;
- envelope SHA-256 and task/diff snapshot digests;
- exact base SHA and head SHA;
- backend identity (`codex`), `fallback_used=false`, exit status, and response
  SHA-256.

Recompute the emitted `prompt.txt` and `envelope.json` SHA-256 digests and
compare them to the receipt. Record the controller receipt file's own SHA-256
in `audit.json`. A missing receipt or digest, mismatch, nonzero controller
exit, stale SHA, backend other than Codex, or fallback makes the result
`inconclusive`.

## Response contract

The controller appends the exact digest-bound machine lines to the static
catalog prompt. Follow those emitted lines literally; this skill does not
duplicate their changing bound values. The verdict line has the canonical
shape `VERDICT: pass|fail`, followed by the controller-owned C0-C7 and E0-E14
`pass|fail` lines and the narrative sections defined by
`prompts/catalog/controller_cold_review_v1.md`. Do not emit the former JSON
`blockers|no_blockers|inconclusive` schema.

## Final /f Output

Include:

```text
Reviewer calibration: enabled <artifact-path>
Canonical Codex verdict: <...>
Receipt audit: <valid|inconclusive>
```

If disabled:

```text
Reviewer calibration: disabled <explicit reason>
```
