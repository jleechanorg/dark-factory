# PR #812 complete final-head pre-merge evidence

This evidence is bound to the frozen controller target that contains this
file. Every command below was executed after this file was committed, with
`HEAD` equal to the controller's exact `head_sha` and a clean worktree. The
controller receipt supplies the immutable SHA/tree/diff identity.

The parent code commit is
`52ac0bf8a4893c8fc6b14abcf0d0076a46c1ecea`. The target differs from that
code commit only by adding this evidence file.

## Exact-target executable results

- `cargo test --lib -j 1 -- --test-threads=1`: PASS, 640 passed, 0 failed.
- `cargo test --test gh_circuit_breaker_integration -- --test-threads=1`: PASS.
- `cargo test --test adapters_integration -- --test-threads=1`: PASS.
- `cargo test --test tick_integration -- --test-threads=1`: PASS.
- `cargo test --test tick_integration_sqlite -- --test-threads=1`: PASS.
- `PYTHONPATH=. .venv/bin/python -m pytest -q tests/test_install_immutable_linux.py -x`: PASS.
- `cargo clippy --lib --bins -- -D warnings`: PASS.
- `cargo check --bins --tests -j 1`: PASS.
- `git diff --check`: PASS.
- `git status --short`: empty after verification.

## Findings closed at this target

- File-backed GraphQL query fields in separated and attached `-f`/`-F` forms
  fail closed as mutations when the document cannot be inspected.
- AO routing is durable before and after normal/adopted spawn boundaries and
  survives daemon restart without mutable-config retargeting.
- Controller namespaces are injective and root-contained; manifest identity
  is validated before signals or removals.
- Immutable release reuse verifies the complete manifest with an external
  interpreter before executing release-owned code.

Independent Terra review reported no remaining Blocker, High, or Medium
findings in the restart-routing and controller-namespace changes.

## Anti-gaming boundary

This remains pre-merge evidence. It does not claim deployment, native AO
failed-gate/reroll/READY proof, funnel thresholds, or 48-hour sustain.
