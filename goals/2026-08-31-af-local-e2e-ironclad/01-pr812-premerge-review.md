# PR #812 pre-merge review contract

Review the exact PR head as a factory-maintenance proof unit. This review does
not claim or replace the post-merge `/af` C1–C7 terminal proof in
`00-goal-definition.md`.

Pass only if the frozen diff and supplied evidence support all of these:

1. AO recovery is bounded, project-scoped, process-identity bound, isolated
   from shared operator state, and fails closed without burning dispatch
   attempts.
2. Cross-repository sessions retain their owning AO project for status,
   activity, branch, quiescence, cleanup, and reroll operations.
3. Dependency-blocked beads do not dispatch or consume retries, while transient
   base-revision failures requeue and later resume the same dispatch phase.
4. Adopted-PR ownership is atomic and coalesces only an identical live owner;
   transaction commit failures roll back all partial ownership state.
5. GraphQL and REST breakers remain independent for reads, while every GitHub
   mutation form—including attached field flags—requires mutation admission.
6. An immutable Linux release is built from exact committed Git bytes, records
   a deterministic full-runtime manifest, validates it with tooling outside the
   release before reuse, and rejects restored-read-only tampering.
7. Targeted and full tests exercise these contracts without weakening gates,
   substituting mocks for the eventual native E2E proof, or dispatching bare
   `claude`/`claude-sonnet` by default.

Do not fail this pre-merge review merely because the unmerged revision has not
been deployed or sustained for 48 hours; those are deliberately retained as
post-merge terminal criteria in `00-goal-definition.md`. Do fail for any code,
test, provenance, security, concurrency, or regression defect in this PR.
