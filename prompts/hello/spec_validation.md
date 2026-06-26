Validate that `specs/<feature>.md` is sufficient to drive a Dark Factory
build under the Attractor-style "spec in, evaluator out" pattern.

Goal:
${goal}

Locate the spec at `specs/<feature>.md` (standard location). If absent, stop
and report the missing file rather than validating from the goal alone — a
spec that does not exist on disk is not a spec that can be validated.

## What "spec in, evaluator out" requires

The public spec is the only artifact an implementing agent sees. The hidden
evaluator (sealed, intentionally absent from this repo) is the only artifact
the grader sees. The split works only when the public spec is rich enough
that a competent agent can build without hidden product requirements, AND
the hidden cases cover everything the public spec omits by design.

## Validation checklist

Walk every item below against `specs/<feature>.md` and report PASS / FAIL /
PARTIAL with the exact quoted text that supports the verdict.

1. **Public spec detail floor.** Is the spec detailed enough that a coding
   agent can build without inventing product requirements? Look for: explicit
   acceptance criteria, concrete API/CLI/data shapes, named error states,
   visible-vs-hidden boundary, deterministic test command, evidence expected
   before merge. A goal-only or "implement X" paragraph is FAIL.

2. **Hidden-case coverage (inferred).** The spec itself does not (and must
   not) list hidden cases, but it MUST name the categories they would have
   to cover so the spec is testable. Check that the spec addresses, at
   minimum, by reference: exact data / payloads, adversarial inputs, role
   or auth attacks, race or concurrency cases, service-failure modes,
   viewport or scale variants, and scoring weights. If any category is
   silently absent, FAIL with the missing list.

3. **Reviewer-node requirement.** Every non-trivial pipeline must have at
   least one independent reviewer node or `tool` invocation (codex exec,
   AO worker, `agy`, etc.) separate from the implementing agent. Confirm
   the spec names the reviewer topology or, if absent, that the spec is
   trivial enough to ship without one (single-file, no behavior change).

4. **Outcome-artifact confidence.** Merge confidence must come from outcome
   artifacts: public spec validation, deterministic tests, sealed holdouts,
   independent reviewer reports, CXDB history, evidence bundles. FAIL if
   the spec asks for "looks correct" or relies on cheap validation
   (unit-only mocks, single-reviewer approval, no holdouts).

5. **`.dot` graph as durable process code.** If the spec describes a
   development process, that process must be encoded as a `.dot` pipeline
   under `pipelines/`, not as a prose checklist. FAIL if the spec asks for
   steps humans will forget to run.

6. **Anti-patterns.** Flag and FAIL on any of: spec that hand-waves
   acceptance ("should work correctly"), spec that copies holdout-shaped
   assertions into the public surface (collapses the adversarial split),
   spec that hides the evaluator path or scoring rubric, spec that asks
   for cheap signals as a substitute for adversarial validation.

## Output (must write)

Append the validation report to `.dark-factory/spec-validation.md` in the
target repo with this structure:

    ## Spec under validation
    specs/<feature>.md (path + sha256 of file contents)

    ## Verdict
    PASS | FAIL | PARTIAL  — one line, justified by the checklist below

    ## Checklist
    1. Public spec detail floor: PASS|FAIL|PARTIAL — <quoted text or note>
    2. Hidden-case coverage:    PASS|FAIL|PARTIAL — <quoted text or note>
    3. Reviewer-node requirement: PASS|FAIL|PARTIAL — <quoted text or note>
    4. Outcome-artifact confidence: PASS|FAIL|PARTIAL — <quoted text or note>
    5. .dot graph as process code: PASS|FAIL|PARTIAL — <quoted text or note>
    6. Anti-patterns:            PASS|FAIL|PARTIAL — <quoted text or note>

    ## Required spec edits
    Bulleted list, empty if PASS. Each bullet names the file path, the
    section heading, and the concrete edit the spec author must make.

    ## Hidden-case budget
    Estimate how many sealed evaluator cases the spec implies (rough count
    by category: data / adversarial / role / race / failure / viewport /
    scoring). If a category is zero, flag it — the sealed evaluator cannot
    grade behavior the spec never describes.

## Rules (load-bearing)

- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Validate only against the spec as written. Do not invent requirements
  the spec omits; report them as missing instead.
- Do not read, reference, or paraphrase sealed holdout scenarios or
  evaluator internals. The split only holds if this node cannot leak
  hidden content into the public surface.
- Do not modify `specs/<feature>.md`. The report lists required edits;
  the spec author applies them.
- Do not start implementation. This node produces a validation report;
  the runner routes PASS to the next phase and FAIL/PARTIAL back to
  spec authoring.

End your response with: `spec_validation: <verdict> for specs/<feature>.md`
where `<verdict>` is one of PASS, FAIL, or PARTIAL.