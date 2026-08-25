# Final Reply (revised after codex cross-model review)

**STATE.md:** `file:///Users/jleechan/roadmap/dark-factory/sidekick/research-tessl-harness-applied-to-dark-factory/STATE.md`
**Branch:** `research/tessl-harness-applied-to-dark-factory` (5 commits ahead of `origin/main` HEAD `422e86bc5e`)
**Mission HEAD:** `f2ebb19b24`
**Constraint honored:** Research-only — never pushed to `origin/main`; never opened a PR.

## Beads filed (3, all factory-labeled, codex-revised)

1. **`beads/factory/tessl-harness-1-detector-skipped-red.md`** — R2 redesign, **CRITICAL**. Fix `verifier.rs:965` mapping `VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green` → `Red(reason)`. **Single-line gate flip**, no new enum variant. Body 2,809 chars.
2. **`beads/factory/tessl-harness-2-bugbot-structured-verdict.md`** — R1 redesign, **HIGH**. Extend existing `bugbot_status: String` (tools.rs:207) to typed `BugbotStatus` enum (extending the `bugbot_pending: bool` pattern at tools.rs:222, jleechan-8s2p). Replace substring matching (gates_compute.rs:180) with enum dispatch. Body 3,542 chars.
3. **`beads/factory/tessl-harness-3-reviewer-availability-ledger.md`** — R5 redesign, **HIGH**. Extend existing `config/vendor_aliases.json` with parallel `reviewer_aliases` section (mirrors the canonical `include_str! + OnceLock + serde::Deserialize` pattern at `daemon/src/vendor_aliases.rs`). Extend existing `VendorHealthLedger` (vendor_health.rs, 428 LOC, 11 unit tests) with `ReviewerHealth` variants. Body 3,845 chars.

Each bead carries `target_repo: jleechanorg/dark-factory`, `labels: factory`, 5-green acceptance, kill-switch env var for Phase 2 flip (`DARK_FACTORY_LEGACY_BASELINE_FAILED_GREEN=1`, `DARK_FACTORY_LEGACY_BUGBOT_SUBSTRING=1`, `DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1`), and local-run proof (5 daemon-test passes).

## Codex Cross-Model Review (rule 12, completed)

`codex-pair-verifier` agentId `ac2f46420d36a8511` ran 10 spot-checks against HEAD `422e86bc5e`.

**Verdict per bead:** PLAUSIBLE — all 6 redesigns address real bypasses confirmed via grep reproductions.

**Common defect class across all 3 initial drafts:** `propose-new-infrastructure-over-extend-existing-infrastructure`.

| Codex killer | Confirmed? | Revision applied |
|--------------|------------|------------------|
| Bead 1 misfiled the enum at `vacuous_red_green.rs:64-90`; actual location is `verifier.rs:504-547`. `DetectorSkipped` variant would be redundant with existing `CargoNotFound` / `PytestNotFound` / `ManifestMissing` which already map to `Unknown`. Only `BaselineFailed` is the unique bypass. | ✓ | Collapsed 3 PRs → 1 PR: flip single line at `verifier.rs:965`. No new variant. |
| Bead 2 proposed new `bugbot_status: BugbotStatus` field; `bugbot_pending: bool` already exists at `tools.rs:222` (jleechan-8s2p). AST-grep tooling doesn't exist in repo. | ✓ | Extend existing `bugbot_status: String` (line 207) into typed enum. AST-grep replaced with `cargo clippy --workspace -- -D clippy::match_wildcard_for_single_variants`. |
| Bead 3 proposed new `daemon/src/reviewer_health.rs` + `config/reviewers.toml`; existing `daemon/src/vendor_health.rs` (428 LOC, 11 unit tests, `VendorHealthLedger`) and `config/vendor_aliases.json` exist. | ✓ | Extend `vendor_aliases.json` with parallel `reviewer_aliases` section; extend `VendorHealthLedger` with `ReviewerHealth` variants. **No new files.** |

## Key Gap Findings (5, all file:line verified at HEAD `422e86bc5e`)

1. **CRITICAL** — `verifier.rs:965`: `VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green`. The vacuous-test detector's entire purpose is negated by infra failure. Bead 1 fixes this.
2. **HIGH** — `gates_compute.rs:180`: `if body.contains("error") || body.contains("fail")`. Substring matches `feasibility`, `failure-rate`, etc. Bead 2 fixes this.
3. **HIGH** — `verifier.rs:909` + `159-160`: `SkepticVerdict::Warn` → Green AND Warn text dropped from `to_json()`. Operator cannot see what Skeptic warned.
4. **HIGH** — Compound waiver cascade at `verifier.rs:1011`: vendor waiver requires Skeptic::Pass + /er::Pass + cross-model, but NONE have availability invariants. Per MEMORY.md 2026-08-17 ironclad audit, this caused real damage (7/7 false-PASS). Bead 3 fixes this.
5. **HIGH** — `verifier.rs:21`: `const EVIDENCE_FLOOR_LOC: u32 = 100` is hardcoded. 99-LOC production bug bypasses evidence requirements.

## Operational Constraints — All Honored

- ✅ Research-only mission; never pushed to `origin/main`.
- ✅ 5 research commits on `research/tessl-harness-applied-to-dark-factory`.
- ✅ Each bead has kill-switch env var for Phase 2 flip.
- ✅ Local-run proof required (5 daemon-test passes per bead).
- ✅ All 7 publishability checks pass (rule 11) — see `docs/plans/tessl-harness-2026-08-25/publishability-gate-report.md`.
- ✅ Cross-model cold review completed (rule 12 — codex-pair-verifier verdict: PLAUSIBLE).
- ✅ 4 file:line claims independently spot-checked at HEAD before codex dispatch.
- ✅ Codex findings incorporated: 3 beads revised, all using existing repo substrate.
- ✅ Re-run publishability gate on revised docset (commit `f2ebb19b24`): all 7 checks pass.
- ✅ 5-min checkpoint cadence: STATE.md updated at every phase boundary.
- ✅ 3-hour autonomy time-box honored (mission completed well under).

## Files Produced (13)

```
docs/plans/tessl-harness-2026-08-25/
  gap-analysis.md                          (Tessl pillar × state mapping)
  gate-attacks.md + .json                  (17 attacks, 10 survive 3-lens)
  invariant-redesigns.md + .json           (6 redesigns R1-R6 + codex-review section)
  specs-mined.json                         (10 invariants, file:line)
  gate-failures.json                       (30-day CXDB cluster)
  history-mined.json                       (14 session cross-refs)
  publishability-gate-report.md            (rule 11, all 7 pass + post-revision re-run)
  close-out-summary.md                     (Phase 8 prep)
  FINAL_REPLY.md                           (this file)

beads/factory/
  tessl-harness-1-detector-skipped-red.md           (CRITICAL, R2 — 1 PR)
  tessl-harness-2-bugbot-structured-verdict.md      (HIGH, R1 — 3 PRs)
  tessl-harness-3-reviewer-availability-ledger.md   (HIGH, R5 — 3 PRs)
```

## Token Spend (estimate)

~150K tokens across Phase 0-9. **Single subagent dispatch** (codex-pair-verifier). Per-phase breakdown in `close-out-summary.md`.

## Agent Counts

- Phase 0 (Setup): 0 subagent (in-session STATE.md + task list)
- Phase 1-5 (Mining through Publishability): 0 subagent
- Phase 6 (Cross-model review): **1 subagent (codex-pair-verifier, PLAUSIBLE verdict)**
- Phase 7 (Close): 0 subagent
- Phase 9 (Incorporation): 0 subagent (in-session revision)

**Total subagent dispatches: 1** (the rule-12 mandatory cross-model pass).

## Recommended Next Steps (for operator)

1. **Push** `research/tessl-harness-applied-to-dark-factory` to `origin` (no PR).
2. **File** the 3 factory beads via `br` (target_repo: jleechanorg/dark-factory, labels: factory).
3. **/af** picks them up on the next iteration.
4. **/advice** can run as a follow-up if operator wants multi-reviewer sign-off.

---

*Mission complete. Codex cross-model review completed and incorporated. All 5 research commits on the research branch; never pushed to `main`.*