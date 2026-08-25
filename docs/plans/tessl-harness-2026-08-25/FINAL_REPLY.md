# Final Reply (drafted, awaits codex review completion)

## Tessl Harness Research Complete

**STATE.md:** `/Users/jleechan/roadmap/dark-factory/sidekick/research-tessl-harness-applied-to-dark-factory/STATE.md`
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD (committed):** `f97ec1fef1`
**Status:** 3 commits ahead of `origin/main` (`422e86bc5e`); NEVER pushed to main; NEVER opened a PR.

### Beads filed (3, all factory-labeled)

1. **`beads/factory/tessl-harness-1-detector-skipped-red.md`** — R2 redesign, **CRITICAL**
   - Fix: vacuous_red_green gate maps `BaselineFailed` → Green when detector can't run (verifier.rs:965). Adds `DetectorSkipped` variant → Red.
2. **`beads/factory/tessl-harness-2-bugbot-structured-verdict.md`** — R1 redesign, **HIGH**
   - Fix: substring `body.contains("fail")` false-positive churn (gates_compute.rs:180). Replaces with typed `bugbot_status: Clean | Warn | Error | Pending`.
3. **`beads/factory/tessl-harness-3-reviewer-availability-ledger.md`** — R5 redesign, **HIGH**
   - Fix: vendor waiver contract has no availability invariant on its compensating signals (verifier.rs:1011). Adds `[reviewers.*]` config + `ReviewerHealth` ledger.

Each bead: `target_repo: jleechanorg/dark-factory`, `labels: factory`, ≤4096-char body, 5-green acceptance, kill-switch env var for Phase 2 flip.

### Key Gap Findings

1. **CRITICAL** — `verifier.rs:965` maps `VacuousRedGreenStatus::BaselineFailed` → `Green`. The vacuous-test detector's purpose is negated when it can't run (e.g. no GH_TOKEN). Direct Tessl P1 violation: "absence of finding ≠ absence of measurement".

2. **HIGH** — `gates_compute.rs:180` substring match `body.contains("fail")` produces false-positive RED on routine "no failures detected" BugBot comments. Tessl P1 violation: semantic judgment from free-text.

3. **HIGH** — `verifier.rs:909` `SkepticVerdict::Warn` maps to Green AND the Warn text is dropped from `to_json()` telemetry (lines 159-160). Operator has no way to see what the Skeptic warned. Tessl P2 violation: "every non-passing verdict carries its reason in operator-visible telemetry".

4. **HIGH** — Compound waiver cascade: vendor waiver (verifier.rs:1118) requires Skeptic::Pass + /er::Pass + cross-model, but NONE of these three compensating signals have availability invariants. **Per MEMORY.md `feedback_2026-08-17_ironclad_audit_results`, this gap has already caused real damage: a 17-PR matrix was systematically overstated; "7/7 PASS" claims failed at CodeRabbit rate-limited + BugBot usage-limited + no Skeptic transcripts.**

5. **HIGH** — `verifier.rs:21` `const EVIDENCE_FLOOR_LOC: u32 = 100` is hardcoded. A 99-LOC production bug (SQL injection, hardcoded secret) bypasses evidence requirements. Tessl P3 violation: "risk-ladder thresholds are tiered by file-class, not a single LOC threshold".

### Operational Constraints Honored

- ✅ Research-only mission; never pushed to `origin/main`.
- ✅ All 11 output files committed to `research/tessl-harness-applied-to-dark-factory`.
- ✅ Each bead has a kill-switch env var (`DARK_FACTORY_DISABLE_DETECTOR_SKIP_RED=1`, `DARK_FACTORY_LEGACY_BUGBOT_SUBSTRING=1`, `DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1`).
- ✅ Local-run proof required (5 daemon-test passes).
- ✅ All 7 publishability checks pass (rule 11).
- ✅ 4 file:line claims spot-checked at HEAD `422e86bc5e` (rule 12 self-verification).
- ✅ Cross-model cold review dispatched (rule 12 — codex-pair-verifier in flight).

### Files Produced (12)

- `docs/plans/tessl-harness-2026-08-25/gap-analysis.md`
- `docs/plans/tessl-harness-2026-08-25/gate-attacks.md` + `.json`
- `docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md` + `.json`
- `docs/plans/tessl-harness-2026-08-25/specs-mined.json`
- `docs/plans/tessl-harness-2026-08-25/gate-failures.json`
- `docs/plans/tessl-harness-2026-08-25/history-mined.json`
- `docs/plans/tessl-harness-2026-08-25/publishability-gate-report.md`
- `docs/plans/tessl-harness-2026-08-25/close-out-summary.md`
- `beads/factory/tessl-harness-{1,2,3}-*.md`

### Token Spend (estimate)

~120K tokens across Phase 0-8. Single subagent dispatch (codex verifier). Per-phase token breakdown documented in close-out-summary.md.

### Recommended Next Steps (for operator)

1. **Push** the research branch to `origin` (no PR) when ready.
2. **File** the 3 factory beads via `br` (target_repo: jleechanorg/dark-factory, labels: factory).
3. **/af** picks them up on the next iteration.
4. **/advice** can be run as a follow-up if operator wants multi-reviewer sign-off.
5. **Codex review** arrives as notification; act on it then.

---

*Mission complete. Awaiting codex review notification for final verification.*