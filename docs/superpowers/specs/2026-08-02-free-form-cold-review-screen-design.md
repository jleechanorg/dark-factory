# Free-Form Cold-Review Screening Design

Status: awaiting written-spec review  
Date: 2026-08-02  
Tracking bead: `jleechan-gcwh.5`

## Goal

Test whether a reviewer prompt written like a short human request finds more
known actionable defects than `controller-cold-review-v2`. The screen compares
the current v2 control with two free-form prompts. Both short prompts explicitly
cross-check the PR design docs, goals and tenets, description, actual code and
production paths, and executed evidence.

This is a low-cost screening experiment. It cannot authorize rollout or support
a statistically strong claim that one prompt is better. A competitive short
prompt must advance to the complete seven-snapshot sealed benchmark.

## Non-goals

- Do not change production pipelines or their v1 selector.
- Do not replace the controller contract during this screen.
- Do not add keyword scoring, heuristic defect classification, or application
  code that judges finding semantics.
- Do not treat a nine-call result as rollout evidence.

## Arms

### Control: current v2

Use the exact pinned `controller-cold-review-v2` prompt and controller response
contract already present on the experiment branch.

### Free-form traceability

> Review this PR independently. Cross-check its design docs, goals and tenets,
> and PR description against the actual code and production paths and the
> executed evidence. Find every actionable defect and keep reviewing the whole
> change after the first finding. Report each finding as a separate bullet with
> an exact `path/to/file:L123` reference and explain which design goal, tenet,
> description claim, code behavior, or evidence claim it violates.

### Free-form adversarial

> Try to prove this PR is wrong. Cross-check the design docs, goals and tenets,
> PR description, actual production code paths and consumers, and executed
> evidence for contradictions, omissions, false-green tests, and unverified
> claims. Keep attacking independent failure modes after every finding until
> the entire change has been examined. Report each actionable defect as a
> separate bullet with an exact `path/to/file:L123` reference and the
> contradicted claim or evidence.

The two experimental arms have no gates, bindings, verdict line, JSON schema,
or machine response contract in the model-visible prompt. The immutable target
revision and input digests remain controller-owned run metadata outside the
prompt response.

## Screening cases

Use three immutable historical snapshots from the existing public manifest:

| Case | Review surface |
|---|---|
| `wa-8603-r1` | evidence provenance and reproducibility |
| `wa-8612-r2` | runtime integration and consumer behavior |
| `wa-8613-r1` | ordering and fail-open behavior |

The case selection is declared before execution. It must not be changed after
seeing any arm output. Sealed expected findings remain exclusively in the
holdouts repository and never enter reviewer prompts.

## Execution controls

- Model: `gpt-5.6-luna` for all nine review calls.
- Reasoning effort: `high` for every arm.
- Same exact base, head, diff, changed files, task text, evidence manifest,
  tools, read-only workspace, timeout, and input order for all arms of a case.
- Randomize the three-arm order independently per case from one recorded seed.
- Run the three cases concurrently. Run arms serially within each case to avoid
  within-case resource contention.
- Record requested and observed case concurrency; a worker flag alone is not
  proof of parallelism.
- Sanitize holdout paths and variables from every reviewer environment.
- Preserve raw transport output, complete transcript, usage, latency, exit code,
  input digests, prompt digest, and target head for every call.

The screen is nine calls total: three cases times three arms. It measures a
single sample per case and arm, so run-to-run variance remains an explicit
limitation.

## Blinding and semantic scoring

The public runner assigns opaque arm IDs and emits complete transcripts without
the private arm map. A separate evaluator model sees the sealed rubric, bound
case inputs, raw code diff and evidence, and one opaque transcript at a time. It
must assert that it was not shown prompt identity or the private arm map.

The evaluator model, not application code:

1. identifies the distinct actionable findings asserted by the transcript;
2. maps each sealed expected finding to zero or more transcript findings;
3. classifies each transcript finding as supported or unsupported; and
4. determines whether the transcript effectively reported no actionable
   defects, which is an implicit PASS for false-PASS scoring.

No keyword splitter or heuristic classifier may convert prose into findings.
The complete raw transcript remains the scoring source. The judge is instructed
to ignore length, tone, headings, and machine formatting and score only semantic
defect content. Because v2 remains visually distinguishable, format-signaling
bias is a documented limitation of this screen.

## Metrics

Report per case and aggregate, for every arm:

- known P0/P1 recall;
- total actionable recall;
- false-PASS count and rate;
- unsupported-finding count and rate;
- invalid or transport-failure count;
- input, output, and total tokens; and
- total and mean latency.

The result report may reveal arm identities only after all judgments are bound
to transcript and case digests.

## Failure rules

A call is invalid if its transport exits nonzero, its transcript is missing, its
target or input digests do not match, its model or reasoning setting differs, or
its transcript cannot be bound to the run plan. Invalid calls are never silently
retried in place. A replacement run uses a new run ID and records the failed
attempt.

The screen fails closed if any arm receives incomplete judgments, any expected
finding or asserted transcript finding is unclassified, sealed data enters a
reviewer prompt, the private arm map is exposed before judgment binding, or
concurrency and execution conditions differ across arms.

## Advancement rule

A free-form arm is competitive and advances to the full seven-snapshot replay
when all of the following hold:

1. It has no known-P0/P1 false PASS in the three screening cases.
2. Its aggregate known-P0/P1 recall is no worse than current v2.
3. On at least two of three cases, its case-level known-P0/P1 recall is no worse
   than v2 and its unsupported count exceeds v2 by at most one finding.
4. Its aggregate actionable recall is no worse than v2.

Ties advance rather than being declared wins. Multiple free-form arms may
advance. No arm becomes production-eligible from this screen.

## Required artifacts

- immutable run plan and case manifest;
- recorded randomization seed and private arm map;
- per-call prompt digest, input digests, raw JSONL, complete transcript,
  execution receipt, token usage, and latency;
- opaque blinded transcript bundles;
- digest-bound evaluator judgments;
- per-case and aggregate score output; and
- a deblinded comparison that states the screen limitations.

## Implementation boundary after approval

Extend the existing public benchmark driver with an experimental three-arm
mode rather than creating a second runner. Keep free-form transcript collection
separate from the production `run_controller_review` validation path. Reuse the
sealed semantic judge contract where possible, adding a transcript-native
judgment schema only if the current finding-list schema cannot represent
free-form output without heuristic parsing.

Implementation must include unit tests for arm randomization, identical inputs,
environment sanitization, transcript binding, invalid-run handling, semantic
judge completeness, deblinding, and deterministic metric aggregation.
