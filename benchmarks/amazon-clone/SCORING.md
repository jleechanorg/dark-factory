# Amazon Clone scoring rubric

Total: 100 points.

- Build and launch contract: 10
- Candidate self-tests: 10
- Core product behavior: 35
- Edge-case behavior: 15
- Accessibility and responsive behavior: 10
- Evidence quality: 10
- Cost, runtime, and autonomy: 10

The public rubric intentionally describes scoring categories, not exact sealed
scenario values or evaluator implementation details.

Candidate quality threshold: 80/100 with no critical sealed validation failure.

Fair method comparison additionally requires the outer harness to enforce equal
visible spec, starter state, model access, runtime budget, token budget, retry
budget, and held-back evaluator version. This rubric scores one candidate artifact;
it does not by itself prove a method bakeoff was fair.

For the visible-vs-held-back disclosure contract that pairs with this rubric, see
the parent `README.md` ("Revealed vs. operator-only" section). For the visible
acceptance checklist used by the implementing agent during self-review, see
`visible_acceptance.md`.
