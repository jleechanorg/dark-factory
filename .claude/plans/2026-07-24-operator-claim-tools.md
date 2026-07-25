# Plan — operator-side bin/claim + bin/release tla wrappers

Repo: dark-factory daemon. Snapshot = origin/main @ 5928a0d (PR #475 merge).
The orphan already has 2 real commits on top:
- add bin/claim + bin/release shell wrappers
- add docs/operator-claim-tools.md

## Task 1 — Verify locally
1. bash -n bin/claim (syntax check)
2. bash -n bin/release
3. Create a fresh sqlite3 db, exercise claim/release on a test bead:
   - sqlite3 /tmp/test.db "CREATE TABLE claim_audit ..."
   - bin/claim test-bead (write to /tmp/test.db) — should return 0
   - bin/claim test-bead again — should return 1 (self already holds)
   - bin/release test-bead — should return 0
   - bin/release test-bead again — should return 1 (not held)
4. Publish the 2 commits to origin (already pushed in the dispatch prep).

## Task 2 — Evidence + open PR
1. `gh api -X POST repos/jleechanorg/dark-factory/pulls --input /tmp/pr-claim-tools.json`
   with title `[antig] feat(daemon): operator-side bin/claim + bin/release tla wrappers`
   base main, head factory/operator-claim-tools (the branch I just pushed to).
2. Body includes `**Evidence**: <gist-url> (head af29e4e6c1b2119235d280c39f9973315e69508d)`.
3. Create evidence gist ≥256 bytes naming jleechanorg/dark-factory + PR number + head SHA.
4. `gh run rerun <latest-evidence-gate-run-id>` for the branch.
5. Confirm Evidence Gate green.

## Task 3 — Stop. Do NOT merge or deploy. Operator /hermes lane handles merge + deploy from handoff.
