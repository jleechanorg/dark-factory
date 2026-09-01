# PR #812 final-head pre-merge evidence

This evidence is intentionally bound to the frozen controller target that
contains this file. All commands below were executed after this evidence file
was committed, with `HEAD` equal to the controller's exact `head_sha` and a
clean worktree. The controller receipt supplies the immutable SHA/tree/diff
identity; this file does not guess its own commit hash.

The parent code commit is
`55b799763853c20148d46e0e7374606c68dd1a32`. The target differs from that
code commit only by adding this evidence file. This is pre-merge proof only;
terminal post-merge `/af` C1-C7 evidence remains required.

## Exact-target executable results

- `cargo test --lib -j 1 -- --test-threads=1`: PASS, 638 passed, 0 failed.
- `cargo test --test reroll_integration test_reroll_adopted_success_spawns_remediation_session_leaves_pr_open -j 1 -- --exact --test-threads=1`: PASS, 1 passed.
- `cargo test --test reroll_integration test_reroll_adopted_skips_duplicate_spawn_when_session_already_active -j 1 -- --exact --test-threads=1`: PASS, 1 passed.
- `cargo test --lib open_migrates_ -j 1 -- --test-threads=1`: PASS, 3 passed.
- `cargo test --lib save_failure_after_spawn -j 1 -- --test-threads=1`: PASS, 2 passed.
- `cargo clippy --lib --bins -- -D warnings`: PASS.
- `cargo check --bins --tests -j 1`: PASS.
- `git diff --check`: PASS.
- `git status --short`: empty after the commands.

## Independent semantic review

The final read-only Terra review reported no remaining Blocker, High, or
Medium findings and APPROVED:

- exact AO-project persistence before and after normal/adopted spawn;
- restart restoration for session-only and branch-only identities;
- fail-closed multi-repo unknown/conflicting identities;
- idempotent legacy SQLite migration;
- injective, root-contained controller namespaces including empty, dot, and
  dot-dot project strings; and
- exact manifest-project validation before any process-group signal or
  manifest removal.

## Anti-gaming boundary

This evidence does not claim merge, deployment, native AO reroll/READY,
funnel thresholds, or the 48-hour sustain criterion.
