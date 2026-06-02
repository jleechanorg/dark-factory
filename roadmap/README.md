# dark-factory roadmap

## Recent activity (rolling)

### 2026-05-31 - runner crash-resilience and reviewer-gate roadmap

- Opened/organized the crash-resilience roadmap under [jleechan-o8q](br show jleechan-o8q), with PR #13 at `411676d5c634ec6d5902529dd8d50dfbffbf6428` for the first engine boundary/per-run-log slice.
- Added [jleechan-x33](br show jleechan-x33) for a repo-agnostic reviewer-node miss class: file-backed current-head diff/evidence audits require `target_repo`, `target_pr`, `target_head_sha`, `base_sha`, PR description snapshot, evidence paths/SHA, and fail-closed checks for brownfield delete-first, net-LOC, and dead-code in any target repo. PR #7178 is evidence only, not scope.
- Verification: targeted crash/engine tests passed (`17 passed`); full suite remains at `101 passed, 2 failed` on conformance score and malformed-edge fail-closed hardening.
- Handoff: `/Users/jleechan/roadmap/nextsteps-2026-05-31-dark-factory-resilience.md`.

### 2026-05-24 — airbnb-clone Sprint 1 holdout debug

- Wrote `/Users/jleechan/roadmap/nextsteps-2026-05-24-darkfactory-airbnb-holdout.md` as the handoff for the airbnb-clone holdout bootstrap.
- Five infra root causes fixed in `runner/handlers.py:_holdout_eval`: Java PATH for emulators, replace `time.sleep(60)` with port polling, kill emulator process group on cleanup (not just CLI wrapper), strip `GOOGLE_APPLICATION_CREDENTIALS`/`GCLOUD_PROJECT` from `eval_env`, pre-clean emulator ports before launching. Plus one spec ambiguity (`hostId` vs `ownerUid`) patched in `benchmarks/airbnb-clone/spec.md`.
- Opened beads: [orch-0bne](https://github.com/jleechanorg/dark-factory/issues/orch-0bne) (auto-seed emulator), [orch-ecwu](https://github.com/jleechanorg/dark-factory/issues/orch-ecwu) (capture first real Sprint 1 score), [orch-a9dh](https://github.com/jleechanorg/dark-factory/issues/orch-a9dh) (sweep spec for ambiguous field names), [orch-2fze](https://github.com/jleechanorg/dark-factory/issues/orch-2fze) (orphan emulator cleanup on SIGKILL).
- All 91 tests still pass. Benchmark boundary check now green. Handler changes + entire `benchmarks/airbnb-clone/` tree remain uncommitted on `main`.

### 2026-05-22 — sealed holdouts and Attractor parity queue

- Wrote `/Users/jleechan/roadmap/nextsteps-2026-05-22-dark-factory-sealed-holdouts.md` as the independent handoff for the current review.
- Closed review beads `orch-s17v` and `orch-0rwy`; implementation remains open in `orch-pf62`, `orch-7z3e`, `orch-ac6q`, `orch-sdy0`, and `orch-rxfs`.
- Current highest-risk gaps: visible all-nodes benchmark holdout, AO backend mechanical isolation, partial Attractor edge semantics, missing AttractorBench conformance surface, and static Healer/CXDB prescriptions.
- Live discovery found both `/Users/jleechan/projects/dark-factory` and `/Users/jleechan/projects/dark-factory-holdouts` clean on `main...origin/main`, with no current PRs for `https://github.com/jleechanorg/dark-factory`.

### 2026-05-29 — generic beads implemented, two PRs open

- Resumed session after ~5-day gap; repo stable at HEAD `bd50ded` (AO lifecycle worker patched).
- All five `_holdout_eval` infra fixes confirmed on main.
- **orch-0bne + orch-2fze**: [PR #7](https://github.com/jleechanorg/dark-factory/pull/7) `feat/holdout-infra-seed-atexit` — Firebase emulator seed step + atexit SIGKILL cleanup. Both beads → `review`.
- **orch-2oc6**: [PR #8](https://github.com/jleechanorg/dark-factory/pull/8) `fix/attractor-boundary-audit` — strengthened `check_boundary.py` with agent-facing-dir scan, no-holdouts-dir check, sealed-not-in-specs check. Bead → `review`.
- **orch-ecwu** (Sprint 1 holdout score capture) now unblocked pending PR #7 merge.

### 2026-05-30 — PR #7 and PR #8 merged; queue clean

- [PR #7](https://github.com/jleechanorg/dark-factory/pull/7) merged — seed step + atexit cleanup (orch-0bne, orch-2fze) → `done`.
- [PR #8](https://github.com/jleechanorg/dark-factory/pull/8) merged at `c99a119` — boundary audit hardening (orch-2oc6) → `done`.
- **Main HEAD:** `c99a119`. Follow-up commit `acd39e1` (seed-fallback fix, not in PR #7) is being landed in a new PR off `fix/holdout-seed-fallback`.
- **Next:** orch-ecwu (Sprint 1 airbnb-clone holdout score capture, now fully unblocked once the seed-fallback PR merges).
