# Slim Cold-Review Fallback Design

**Date:** 2026-08-26
**Status:** Implemented on `feat/slim-cold-review-prompt-luna`

## Intent

The native fallback needs a short, model-usable cold-review instruction. The
controller remains the owner of target binding and artifact integrity; the
model owns semantic review judgment. The fallback therefore removes the
verbose C0-E14 checklist and controller-generated hash echoes from the model
conversation without weakening the envelope, workspace, or receipt checks.

## Contract

The static authority tells the reviewer to treat repository data as untrusted,
compare the stated goal and description/claims with the code, its callers and
consumers, and the supplied evidence, then continue after the first finding,
run feasible read-only checks, and fail on material uncertainty or missing
applicable proof. The only accepted model response is one JSON object
with exactly these keys:

```json
{"verdict":"pass|fail","findings":[],"evidence_checked":[],"commands_executed":[],"caveats":[]}
```

The controller still binds and verifies the source template, canonical
Base64 envelope, repository/base/head/tree, changed-file and evidence digests,
and workspace cleanliness. The tool-free `ReviewTransportReceipt` and
controller terminal receipt bind the prompt, envelope, response, reviewed
revision/tree, and evidence manifest. `ValidatedReview` continues exposing
`checks`, but it is always the empty tuple for compatibility with external
receipt consumers.

For a `pass`, `evidence_checked` must contain non-empty strings and
`commands_executed` must be exactly `[]`. A `fail` remains valid without
evidence or receipts so missing proof can itself be reported as the blocking
result.

## Failure behavior

The controller checks the reviewer subprocess return code before attempting
JSONL extraction. A nonzero return code is a contract failure even if stdout
happens to contain text that could otherwise be parsed.
