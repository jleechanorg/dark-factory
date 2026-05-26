# Fix Task

The local gates, independent reviewer, or sealed evaluator found a failure in
the Amazon full-stack commerce implementation.

## Reference Spec

Use `benchmarks/amazon-clone/spec.md` as the visible product contract. Do not
read sealed holdout repositories, hidden evaluator paths, hidden scenarios, or
hidden test source.

## Task

1. Read the visible failure summary carefully.
2. Map the failure to the relevant public user story, route, collection, or
   validation requirement.
3. Root-cause the implementation defect.
4. Make the smallest durable fix that satisfies the public spec.
5. Run the relevant local command, usually `make test` or `make validate-size`.
6. If the public acceptance gate failed, run
   `bash benchmarks/amazon-clone/scripts/public_acceptance.sh .` locally and
   repair the first concrete failure without weakening the spec.

## Visible Failure Context

- Last node: `${state._last_node}`
- Last outcome: `${state._last_outcome}`
- Last output:

```text
${state._last_output}
```

## Constraints

- Do not game hidden tests.
- Do not remove user stories, routes, collections, or validation checks to make
  failures disappear.
- Do not weaken Firestore rules or backend validation.
- Do not log sensitive checkout fields.
- Do not replace full-stack behavior with browser-only constants.

## Success Criteria

The fix is complete when the failing gate is addressed, local validation passes,
and no product-scope requirement from the public spec has been weakened.

## Parallelization

If the active backend supports subagents or parallel workers, use them for
independent failure analysis, implementation repair, and validation lanes, then
integrate the result into the single candidate workspace before exiting.
