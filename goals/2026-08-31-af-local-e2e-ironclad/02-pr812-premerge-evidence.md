# PR #812 pre-merge evidence

Evidence scope: the pre-merge contract in `01-pr812-premerge-review.md`.
This is not terminal C1-C7 proof; deployment and native post-merge `/af`
evidence remain required by `00-goal-definition.md`.

## Immutable code identity

- Base SHA: `ec63e090c6466be60fde7b92b8bc4cdb61751e5d`
- Reviewed code SHA: `3cfcc9f7e8e4d9cf1b9dee9e88f7cc7637082f9f`
- Reviewed tree SHA: `53016501110c28cdddf0070de9b78c951062c59e`
- Canonical binary diff SHA-256 (`git diff --binary <base>...<code-sha>`):
  `741263cafe0545a2f197b5d8f89850081874f30c90bbadaa49033a842cc87038`

The evidence commit that adds this file is a documentation-only descendant of
the reviewed code SHA. The controller receipt binds its own final head and
canonical diff independently.

## Fresh executable verification at the reviewed code SHA

- `cargo test --lib -j 1 -- --test-threads=1`: PASS, 637 passed, 0 failed.
- `cargo test --lib session_routing_restores_sessions_branches_and_exact_spawn_project -j 1`: PASS, 1 passed.
- `cargo test --lib restored_session -j 1`: PASS, 2 passed.
- `cargo test --lib open_migrates_ -j 1 -- --test-threads=1`: PASS, 3 passed.
- `cargo test --lib save_failure_after_spawn -j 1 -- --test-threads=1`: PASS, 2 passed.
- `cargo test --test reroll_integration test_reroll_adopted_success_spawns_remediation_session_leaves_pr_open -j 1 -- --exact --test-threads=1`: PASS, 1 passed.
- `cargo test --test reroll_integration test_reroll_adopted_skips_duplicate_spawn_when_session_already_active -j 1 -- --exact --test-threads=1`: PASS, 1 passed.
- `cargo clippy --lib --bins -- -D warnings`: PASS.
- `cargo check --bins --tests -j 1`: PASS.
- `git diff --check`: PASS.

## Independent review

A read-only Terra semantic review initially rejected the in-memory-only owner
map. After iteration, its final review reported no remaining Blocker, High, or
Medium findings and APPROVED these properties:

- the `ao_project` migration is idempotent and runs for file-backed and
  in-memory startup;
- exact AO routing is persisted before and after both normal and adopted-PR
  spawn boundaries;
- restart reconstruction covers session-only and branch-only identities;
- multi-repo unknown identities fail closed instead of falling back to the
  default project; and
- conflicting session/branch identities fail reconstruction.

A separate read-only release-manifest review APPROVED complete-tree manifest
verification before release-owned code executes. GitHub REST and GraphQL audit
found all six prior CodeRabbit threads resolved; product tests were green at
the preceding head, while the Evidence Gate awaited this evidence and the bot
re-review was externally rate-limited.

## Anti-gaming boundary

This evidence does not claim a merged release, deployed process provenance,
native AO failed-gate/reroll/READY lifecycle, funnel threshold, or 48-hour
sustain. Those remain terminal post-merge criteria.
