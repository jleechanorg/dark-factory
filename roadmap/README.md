# dark-factory roadmap

## Recent activity (rolling)

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
