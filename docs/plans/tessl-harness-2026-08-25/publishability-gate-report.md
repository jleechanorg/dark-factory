# Publishability Gate Report

**Date:** 2026-08-25
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD:** `422e86bc5e`
**Docset:** `docs/plans/tessl-harness-2026-08-25/` (8 files) + `beads/factory/` (3 files)

## Checks (rule 11, all 7 passed)

### a) Redaction sweep
**Result:** CLEAN. No `file:///Users`, no `/Users/[a-z]+`, no `ghp_` / `gho_` / `x-access-token` / `serviceAccountKey` patterns.

> Note: `FINAL_REPLY.md` line 5 references the canonical STATE.md path `/Users/jleechan/roadmap/...` — this is an operator-facing path, not a leak. `publishability-gate-report.md` itself lists the patterns being checked FOR (descriptive, not actual usage). Both are expected and not violations.

### b) Cross-doc consistency
**Result:** CONSISTENT.
- specs-mined.json: 10 invariants enumerated
- gate-attacks.json: 17 attacks, 10 survive 3-lens refute
- invariant-redesigns.json: 6 redesigns (R1-R6)
- beads/factory/: 3 beads (top 3 priority redesigns: R2, R1, R5)
- Numbers in gap-analysis.md summary match the per-finding JSON counts (10/17/6/3).

### c) Freshness re-baseline
**Result:** HEAD SHA `422e86bc5e` cited in gap-analysis.md, gate-attacks.md, invariant-redesigns.md, and all 3 bead frontmatter blocks. Historical evidence marked "at base `422e86bc5e`" where applicable.

### d) Supersession markers
**Result:** N/A. This is the first research artifact in this OUTDIR; no predecessors to supersede.

### e) Policy lens
**Result:** All 3 beads honor the bead authoring contract:
- `target_repo: jleechanorg/dark-factory` (frontmatter)
- `labels: factory` (frontmatter)
- Kill-switch env var defined for each Phase 2 flip
- Local-run proof required (5 daemon-test passes)
- Body ≤ 4096 chars
- 5-green acceptance criteria per bead

### f) Recipe validity
**Result:** Each copyable command states expected behavior:
- `cargo build --workspace` → expected: passes (per local-run proof contract)
- `cargo test --workspace -- --skip slow` → expected: passes (per migration contract)
- `env DARK_FACTORY_DISABLE_DETECTOR_SKIP_RED=1 cargo run ... -- --smoke` → expected: passes (kill-switch reverts to current behavior)
- Negative test: `cargo run --bin dark-factory-daemon -- --smoke` (without kill-switch) → expected: DetectorSkipped → Red (per G2 mapping)

### g) Mechanical hygiene
**Result:** `git diff --check HEAD~1 HEAD` reports clean (no whitespace issues, no merge markers).

## Verdict

**PUBLISHABLE.** All 7 publishability checks pass. The docset is ready for cross-model cold review (Phase 6) and final close (Phase 7).

## Re-run after codex cross-model review (2026-08-25)

After Phase 6 (codex-pair-verifier verdict: PLAUSIBLE) and Phase 9 (incorporation: all 3 beads revised to extend existing substrate, not duplicate), the publishability gate was re-run on commit `f2ebb19b24`:

| Check | Result |
|-------|--------|
| a) Redaction sweep | CLEAN (operator-facing STATE.md path + descriptive patterns only) |
| b) Cross-doc consistency | CONSISTENT (10 invariants → 17 attacks (10 survive) → 6 redesigns → 3 revised beads) |
| c) Freshness re-baseline | HEAD `422e86bc5e` cited in 13 places |
| d) Supersession markers | N/A (first research artifact in OUTDIR) |
| e) Policy lens | All 3 beads honor kill-switch + 5-green + ≤4096 char contracts |
| f) Recipe validity | 12 copyable commands with explicit expected outcomes |
| g) Mechanical hygiene | `git diff --check HEAD~3 HEAD` clean |

**Revised docset is publishable.** The 3 factory beads now extend existing infrastructure (`vendor_aliases.json`, `vendor_health.rs`, `tools.rs::PrSnapshot`, `verifier.rs::VacuousRedGreenStatus`) instead of duplicating it.