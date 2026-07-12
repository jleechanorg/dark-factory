# Severity-lens adversarial verification

## Lane A findings

### /thermo findings

- **F1** (`dict[str, str]` contract enforced at three sites instead of one): strong → strong
  - **Reasoning**: Real defect. Three defensive coercion sites defending an unenforced type contract is dead-weight code that obscures the bug. It's not data-corrupting (a defensive coercion won't crash) and not blocking (it works today), so it stays at strong. The judo simplification (delete two sites, assert at the third) is the right shape. Does not warrant promotion to blocker because no production path currently produces wrong output — only maintenance burden.

- **F2** (`_check_stale_artifacts` is observability noise on the only path that matters): strong → strong (lean toward nit)
  - **Reasoning**: The "5-minute mtime window is the opposite of stale" claim is a real logic bug — anything modified within 5 minutes is fresh, so a `> 5 * 60` predicate filters out recent edits and accepts only older ones, exactly inverted from the function name. Confirmed. The "never-consumed state key" claim is also confirmed (grep across `runner/`, `tests/`, `prompts/` finds zero readers). However: the function *emits an observability event* even when no one consumes the state key, so the dead-write is observable rather than truly invisible. Production impact is currently zero (the state key is never read; the emitted event logs but doesn't gate anything). Strict reading: this is structural debt with a logic bug inside it, not a defect that corrupts behavior today. Strong is correct; not blocker because nothing downstream misbehaves.

- **F3** (`_enforce_outcome_verdict_consistency` performs a backend semantic override): blocker → blocker
  - **Reasoning**: Confirmed blocker. The override silently rewrites a model-owned semantic field, hides the disagreement from consumers (the `original_verdict` / `verdict_adjusted_for_consistency` discriminators are written but never read), and stamp-binds future audit trail. If a reviewer emits `verdict=pass` because they read a stale spec and the runner coerces it to `verdict=fail`, downstream gate logic uses the coerced value — exactly the wrong gate verdict the reviewer was supposed to prevent. This is a correctness regression in adversarial conditions. The right judo move (collapse to outcome-only contract, derive verdict server-side) is the strongest fix and is well-named. Stays blocker.

- **F4** (file-size / decomposition in `_parallel_reviewer`): nit → nit
  - **Reasoning**: File is at 363 lines, threshold is 1000. The single function does read as multiple responsibilities stitched together, but no production impact today. Nit is correct.

- **F5** (`dict[str, str]` typed but never enforced): nit → nit
  - **Reasoning**: Already counted under F1. Nit is correct — the *fact* of unenforcement is real, but the impact is captured by F1's judo recommendation. Demoting further to "remove from list, F1 owns it" would be tidier, but keeping it as nit acknowledges the existence of the gap.

- **F6** (sequential orchestration — no parallelism regression): nit → nit
  - **Reasoning**: Confirmed nit. No regression. Lane A correctly flagged as non-finding.

### /code-standards findings

- **F7** (ponytail rung 7 violated — three-layer defense for one violation): strong → strong
  - **Reasoning**: Same defect as F1, second lens. Stays strong. Not blocker because defensive coercion does not corrupt data.

- **F8** (ZFC violation — backend silently overrides a model-owned semantic field): blocker → blocker
  - **Reasoning**: Confirmed blocker. The `_VERDICT_NORMALIZE` table in `handler_verdict.py` is the canonical verdict tokenizer; `_enforce_outcome_verdict_consistency` introduces a *second* outcome→verdict mapping (`{"success":"pass","failure":"fail","error":"fail","partial":"fail"}` at `handler_parallel_reviewer.py:230-235`) that disagrees with the canonical one on edge cases (`"warn"` maps to `pass` canonically but to `fail` in the new override). This is two competing sources of truth for the same field — exactly the ZFC violation pattern the skill calls out. Stays blocker.

- **F9** (ZFC leveling — dual-field contract gives the model multiple ways to contradict itself): strong → strong
  - **Reasoning**: Real and well-named. `outcome` and `verdict` are two semantic expressions of the same fact; the model can and does disagree with itself. Stays strong. Not blocker on its own because the dual-field model is upstream of pr228's diff (the model contract was pre-existing); F3/F8 capture the corrective-layer impact.

- **F10** (root-cause-first — Bug 1 patched at the symptom, not the invariant): strong → strong
  - **Reasoning**: Confirmed. The "we may encounter old ctx.state values from before this fix" justification is structurally invalid (ctx.state is per-process, not persistent), so the read-site legacy-dict branch and the substitution-time coercion defend against an impossible failure mode. Stays strong. Not blocker because the write-site fix (the only real fix) is present and correct.

- **F11** (root-cause-first — Bug 2 patched at the symptom, not the upstream instruction): blocker → blocker
  - **Reasoning**: Confirmed blocker. F3's override doesn't address either root cause (dual-field contract or spec staleness), it papers over the symptom. The "tests prove the override works" but no raw evidence that prompt/schema can't handle the dual-field contract is the unproven-fallback pattern from root-cause-first. Stays blocker. (Same finding as F3 from a different lens; convergence is the signal.)

- **F12** (root-cause-first — missing canonical-layer fix for `dict[str, str]` contract): strong → strong
  - **Reasoning**: Confirmed. The `StringState(dict[str, str])` subclass proposal is the canonical enforcement home; lacking it, every writer is responsible individually. Stays strong. Not blocker because no current writer actually violates the contract in practice (only one site writes, and the patch fixes it).

## Lane B findings

### /thermo findings

- **F-B1** (test-judo opportunity: 11 verdict-consistency tests duplicate the truth table): strong → strong
  - **Reasoning**: Real test-architecture concern. 11 tests with parametric structure collapsing to ~4 parametrized tests is the standard pytest pattern. Stays strong because it is real code-judo and improves the test surface linearly with the truth table rather than with consumer-call shape. Not blocker because tests pass either way; the maintenance story is the cost.

- **F-B2** (test architecture — ignores canonical conftest infrastructure): nit → nit
  - **Reasoning**: Two repeating patterns (`sys.path.insert` boilerplate and `Context(...)` construction). The `sys.path.insert` is a conftest concern, out of scope for this PR. The `make_context` fixture suggestion is genuinely useful but each instance saves ~1 line and the file is 215 lines total. Nit is correct.

- **F-B3** (test judo via delete — `test_cross_visit_tolerates_legacy_dict` / `test_cross_visit_tolerates_malformed_value` pin transient compat shims): strong → nit
  - **Reasoning**: **Downgrade.** The two tests pin behavior that genuinely exists in production today (the read-back shim at `handler_dispatch.py:723-732`) — they are regression tests for current code, not aspirational pinning. They will become obsolete only when the read-back shim is deleted, which itself is part of F1's judo move. Calling them "permanent debt that prevents contract tightening" overstates the issue: the tests do not prevent the F1 judo; they merely outlive it. If/when the shim is removed, the tests should be deleted in the same commit — that is a normal lifecycle, not a blocker. Demote to nit with a TODO marker as the only concrete ask.

- **F-B4** (sequential orchestration in test setup — two-call "read-back" tests duplicate setup): nit → nit
  - **Reasoning**: Confirmed nit. A parameterized fixture would deduplicate but the file is small and the duplication is contained. Lane B correctly kept it at nit.

- **F-B5** (spaghetti in tests — `test_cross_visit_tolerates_malformed_value` chains two assertions): nit → nit
  - **Reasoning**: Confirmed nit. Splitting into two named tests is style, not correctness. The two paths do chain (None seed → fallthrough, malformed string → fallthrough) so the assertion chain is at least narratively coherent. Nit is correct.

- **F-B6** (no /thermo "tests push file past 1k lines" issue): nit → nit
  - **Reasoning**: Confirmed nit / non-finding.

### /code-standards findings

- **F-S1** (ponytail rung 2 — missed-reuse of `_priority_node`): strong → strong
  - **Reasoning**: Real reuse miss. `_priority_node` exists three cells away in `test_gate_priority_queue.py`; the new tests hand-roll the same construction four times. This is a textbook rung-2 violation (use the helper that exists). Stays strong. Not blocker because tests pass either way; the cost is drift when the queue order changes.

- **F-S2** (ZFC framing risk — the override pins a heuristic that should be a model decision): strong → strong
  - **Reasoning**: Confirmed. The new test exhaustively pins `_enforce_outcome_verdict_consistency`'s `outcome_to_verdict` mapping, which is itself the ZFC violation F3/F8 identify. The tests make the future removal harder because they encode the override as expected behavior. Stays strong. Not blocker because the recommendation (open a follow-up bead for `original_verdict` auditability) is the right framing, not a hard requirement.

- **F-S3** (ZFC `Banned Patterns` — `_enforce_outcome_verdict_consistency` reads as a hidden heuristic): strong → strong
  - **Reasoning**: Confirmed. The function's location (in `handler_parallel_reviewer.py` rather than `handler_verdict.py` next to `_VERDICT_NORMALIZE`) is the smell — it lives next to the caller, not next to the canonical vocabulary. The Move-recommendation is correct. Stays strong. Not blocker because the function still works correctly; it's a code-organization issue.

- **F-S4** (root-cause-first — `dict[str, str]` contract should be enforced once, not discovered twice): strong → strong
  - **Reasoning**: Confirmed. The new tests seed `_substitute_placeholders` with non-string values to prove coercion works — correct regression-test behavior — but no test asserts the upstream contract at the write site. The TODO-marked "freezes the loose contract" test is the right ask. Stays strong.

- **F-S5** (root-cause-first — `_check_stale_artifacts` untested): strong → strong
  - **Reasoning**: Real coverage gap. Lane A's F2 establishes the function has a logic bug (inverted mtime predicate) and is dead-write (no consumer). Lane B has zero tests for it. The asymmetry (11 tests for the override, 0 tests for the detection layer) is the surface a second-opinion reviewer would flag. Stays strong. Not blocker because the override layer works in isolation; the gap is in upstream coverage, not in functional correctness.

## Severity calibration summary

### Downgrades

- **F-B3** (`legacy_dict` / `malformed_value` tests pin transient compat shim): strong → nit
  - **Impact assessment**: The tests pin *currently active* production behavior (`handler_dispatch.py:723-732` legacy read-back). They are regression tests, not aspirational pinning. They become obsolete only when F1's judo lands — at which point they should be deleted in the same commit. Calling them "permanent debt that prevents contract tightening" overstates the maintenance burden: tests deleted in the same PR as the code they pin is a normal lifecycle, not an architectural impediment. The right framing is "TODO marker + delete in the judo commit," not "structural objection to the test surface today."

### Promotions

None. No findings were under-claimed. Lane A's blocker label on F3/F8/F11 is correct (silent model-output override with downstream gate-impact). Lane A's strong on F1/F2/F7/F9/F10/F12 is correct. Lane B's strong on F-B1/F-S1/F-S2/F-S3/F-S4/F-S5 is correct.

### Confirmed blockers (Lane A only)

The single blocker cluster — F3 ≡ F8 ≡ F11 — is real and severe:
- Silent override of a model-owned semantic field.
- Competing canonical vocabularies (`_VERDICT_NORMALIZE` vs. the new `outcome_to_verdict`).
- Audit trail lost (discriminators written but never read).
- Wrong gate verdicts in adversarial conditions (reviewer emits `pass` on stale spec → runner coerces to `fail` → downstream gates see `fail`).

The right move — collapse `verdict` to a deterministic derivation from `outcome`, delete the override — is the single highest-value change in the lane. Together with F1's `StringState(dict[str, str])` judo, the two refactors remove ~130 net lines while preserving the actual bug fixes.

### Confirmed non-blockers (Lane B)

Lane B findings are all test-architecture / coverage-shape concerns. None corrupt production behavior. F-B3 was the most over-claimed and is downgraded to nit; the rest stay at their lane-assigned severity.

### Convergence notes

- Lane A's F1/F7/F10/F12 cluster (triple-defended type contract) and Lane B's F-S4 (no test pins the write-site contract) reinforce each other: the production code defends in three places and the test suite defends in zero places at the canonical boundary. Both should land in the same refactor.
- Lane A's F3/F8/F11 cluster (verdict override) and Lane B's F-S2 (test exhaustively pins the override) reinforce each other: the production code overrides silently and the test suite makes the override permanent. Both collapse together when the override function is deleted.
- Lane A's F2 (stale-artifact detection is noise) and Lane B's F-S5 (detection layer untested) reinforce each other: a logic bug exists in code no one tests. The right move is deletion, not adding tests for broken behavior.