# Tessl Harness Research — Close-Out Summary

**Date:** 2026-08-25
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD:** `f7513b779d` (research branch only; never pushed to `main`)
**Mission:** `/swarm ultracode` — Tessl "Rise of the Harness Engineer" thesis applied to dark-factory.

## Deliverables (all in `docs/plans/tessl-harness-2026-08-25/` + `beads/factory/`)

### Plan docs

1. **gap-analysis.md** — Tessl pillar × dark-factory state mapping with file:line evidence.
2. **gate-attacks.md** + **.json** — 17 adversarial attacks on the 8-Green contract; 10 survive 3-lens refute.
3. **invariant-redesigns.md** + **.json** — 6 Tessl-style redesigns (R1-R6) covering all surviving attacks.
4. **publishability-gate-report.md** — Rule 11 sweep, all 7 checks pass.
5. **specs-mined.json** — 10 currently-enforced invariants with file:line citations.
6. **gate-failures.json** — 30-day CXDB cluster analysis (gate_red=24 top failure).
7. **history-mined.json** — 14 prior session cross-references to MEMORY.md entries.
8. **close-out-summary.md** — this file.

### Factory-labeled beads (top 3 priority redesigns)

1. **`tessl-harness-1-detector-skipped-red.md`** — R2 redesign, **CRITICAL**. Add `VacuousRedGreenStatus::DetectorSkipped(reason) => GateResult::Red`. Fixes the vacuous-detector-gates-as-pass-on-infra-failure bypass.
2. **`tessl-harness-2-bugbot-structured-verdict.md`** — R1 redesign, **HIGH**. Replace `comment.body.to_lowercase().contains("fail")` with typed `bugbot_status: Clean | Warn | Error | Pending`. Fixes false-positive churn + zero-comments vacuous-pass.
3. **`tessl-harness-3-reviewer-availability-ledger.md`** — R5 redesign, **HIGH**. Add `[reviewers.*]` config + `ReviewerHealth` ledger. Vendor waiver contract becomes fail-closed when compensating signals are Capped.

Each bead:
- `target_repo: jleechanorg/dark-factory`
- `labels: factory`
- Body ≤ 4096 chars (3,678 / 3,451 / 4,051)
- 5-green acceptance criteria
- Kill-switch env var for Phase 2 flip
- Local-run proof required

## Key Tessl Findings

### P1 — Systems thinking (invariants)

**What dark-factory has:** 8-gate contract, vendor-waiver contract, vacuous-detector. Most invariants exist but are scattered across `verifier.rs`, `gates_compute.rs`, `vacuous_red_green.rs`, `pipelines/factory/*.dot`. **No single registry.**

**What's missing:**
- Substring-based vendor verdicts (gates_compute.rs:180) — should be typed fields.
- Vacuous-detector infra failure → Green (verifier.rs:965) — should be Red.
- Hardcoded vendor identifiers (gates_compute.rs:138) — should be config.

**Per MEMORY.md `feedback_2026-08-17_ironclad_audit_results`, the repo has been bitten by exactly this gap: a 17-PR matrix was systematically overstated; "7/7 PASS" claims failed at CodeRabbit rate-limited + BugBot usage-limited + no Skeptic transcripts.**

### P2 — Analytics (data over assertion)

**What dark-factory has:** CXDB (2,495 steps over 152 runs all-time; gate_red=24 is top 30-day failure), Healer (offline-only).

**What's missing:**
- Healer doesn't distinguish infra-error from verdict-failure (CXDB shows gate_es:error=36, failure=22 all-time).
- No daily cron for Healer; no live dashboard.
- No alert when an invariant goes dark (0 fires in 7 days).

### P3 — Risk & operations (blast-radius ladder)

**What dark-factory has:** Vendor-waiver contract — explicit compensating-coverage rule for unavailable external reviewers. **Best in class for a harness repo.**

**What's missing:**
- The three compensating signals (Skeptic, /er, cross-model) themselves have no availability invariant.
- A misconfigured runner = universal vacuous-pass (verifier.rs:965).
- 99-LOC SQL injection fix bypasses evidence requirements (verifier.rs:21).

## Tessl Thesis Applied — Summary

The Tessl article argues: agents follow ~70% of skill instructions even when they complete the task. Skills alone aren't enough; they need checks.

The dark-factory repo IS a Tessl-grade harness **70% of the way**. The 8 gates, the vendor-waiver contract, the sealed holdouts, the CXDB+Healer feedback loop — these are the "invariants capture" infrastructure.

What's missing is the **discipline of explicitly registering the invariants**. Per MEMORY.md 2026-08-17 ironclad audit, this has already caused real damage (false-PASS at 7/7 PRs).

The 3 filed beads fix the top-3 survival-critical gaps. The 3 unfiled redesigns (R3/R4/R6) are smaller wins that the factory can pick up after the foundation is in place.

## Operational Constraints Honored

- ✅ Research-only mission; never pushed to `origin/main`.
- ✅ Branch `research/tessl-harness-applied-to-dark-factory` only.
- ✅ All output to `docs/plans/tessl-harness-2026-08-25/` + `beads/factory/`.
- ✅ Factory-labeled beads carry `target_repo: jleechanorg/dark-factory` + `labels: factory`.
- ✅ Bead bodies under 4096 chars.
- ✅ Each bead has a kill-switch env var for Phase 2 flip.
- ✅ Each bead has 5-green acceptance criteria.
- ✅ 4 file:line citations spot-checked at HEAD `422e86bc5e`.
- ✅ Publishability gate (rule 11) — all 7 checks pass.
- ✅ Cross-model cold review (rule 12) — codex dispatched.
- ✅ Memory + git committed (Phase 5 commit + STATE.md).

## What's NOT done (out of scope per brief)

- Do NOT implement the proposed beads — factory picks them up.
- Do NOT push to `origin/main`.
- Do NOT open a PR unless user explicitly asks.
- Do NOT rely on same-model verify alone — codex cross-model pass is the rule-12 mechanism.

## Agent Counts

- Phase 0: 1 session-local task list (8 tasks)
- Phase 1-5: 0 subagent dispatches (mining done inline; source materials in context)
- Phase 6: 1 codex-pair-verifier dispatch (cross-model cold review)
- Phase 7: 0 subagent dispatches (final close in-session)

**Total subagent dispatches: 1** (codex verifier, in flight at close)

## Token Spend (estimate)

- Phase 0-5: ~80K tokens (in-session work; multiple file reads + writes)
- Phase 6: codex verifier — ~30-50K tokens (model-dependent)
- Total: ~120K tokens

## Recommended Next Steps (for operator)

1. **Push** the research branch to `origin` (no PR).
2. **File** the 3 factory beads with `br` (target_repo: jleechanorg/dark-factory, labels: factory).
3. **/af** can now pick them up on the next iteration.
4. **/advice** on the docset can be run as a follow-up if operator wants a multi-reviewer sign-off.
5. **Codex review** result will arrive as a notification — act on it then.

---

*Mission complete. Awaiting codex review notification for final close.*