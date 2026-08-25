# Tessl Harness Engineer × Dark-Factory — Gap Analysis

**Date:** 2026-08-25
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD:** `422e86bc5e`
**Source:** Tessl article (`/tmp/wt-tessl-harness/TESSL_ARTICLE.md`) + dark-factory prior research (`~/.claude/projects/-Users-jleechan-projects-dark-factory/memory/MEMORY.md`) + this repo's current state.

## The Tessl thesis (compressed)

Three pillars define the harness engineer role:

| Pillar | Description | Dark-factory translation |
|--------|-------------|--------------------------|
| **P1 Systems thinking — invariants** | Capture general principles and enforce them via skills, deterministic checks (CI/linter/AST-grep), narrow verifiers, and agentic code review. The headline: skills alone get ~70% adherence. | The 8 gates + Healer + .dot pipelines. Today: **invariants exist but are not REGISTERED.** The contracts live in scattered comments across `verifier.rs`, `gates_compute.rs`, `pipelines/factory/*.dot`. No single source of truth. |
| **P2 Analytics** | Use agent logs, PR comments, code analysis to MEASURE whether the system is doing what you want. | Healer exists (`daemon/src/healer.rs` via `df-healer`) but is **offline-only**, run manually. No live dashboard. Healer doesn't separate infra-error from verdict-failure. |
| **P3 Risk & operations — blast-radius ladder** | Tier the codebase by risk. Some parts auto-merge, some need engineer review. Code the approvals flow accordingly. | `vendor_waiver` contract (verifier.rs:1118-1147) is exactly this idea: when external reviewer is unavailable, compensating coverage (skeptic + /er + cross-model) substitutes. **But** the SLA on the compensating signals isn't itself an invariant. |

## Tessl × dark-factory: where the repo already wins

1. **CXDB + Healer feedback loop** — every step recorded; failure clusters diagnosed. This is a strong P2 primitive that most harnesses lack.
2. **Sealed holdouts** — adversarial test fixtures in a separate repo, never visible to the implementing agent. The infra is in place; the tests are intentional gaps.
3. **.dot as durable artifact** — pipeline shapes are versioned; runner code is disposable. Strong P1 hygiene.
4. **8-Green gate contract** — explicit, well-documented gate set with `is_green()` aggregator. P1 invariant surface IS captured.
5. **Vendor waiver contract** — explicit compensating-coverage rule for unavailable external reviewers. P3 risk-tier in code form.

## Tessl × dark-factory: where the gaps are (file:line evidence)

### Gap G1 — Substring-based vendor verdict (P1 violation, A-4.1, A-4.2)

`daemon/src/gates_compute.rs:179-181`:

```rust
let body = comment.body.to_lowercase();
if body.contains("error") || body.contains("fail") {
    bugbot_error_count += 1;
}
```

**The Tessl violation:** semantic judgment from free-text. The substring `fail` matches `feasibility`, `failure-rate`, `failing test`, `failover`. False-positive RED on routine BugBot "no failures detected" comments.

**The Tessl invariant:** "vendor verdicts are typed fields". Fix: BugBot publishes `<!-- bugbot-verdict: clean|warn|error -->`; verifier reads ONLY the typed field.

### Gap G2 — Detector can't run = vacuous-pass (P1 violation, A-8.1)

`daemon/src/verifier.rs:965`:

```rust
VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green,
```

**The Tessl violation:** infra failure (no GH_TOKEN, gh pr view failed) maps to Green. The detector's entire purpose is negated.

**The Tessl invariant:** "absence of finding ≠ absence of measurement". Fix: add `VacuousRedGreenStatus::DetectorSkipped(reason) → GateResult::Red`. Operator-visible Red, not silent Green.

### Gap G3 — Skeptic::Warn text dropped (P2 violation, A-7.1)

`daemon/src/verifier.rs:159-160`:

```rust
GateResult::Green => serde_json::Value::String("pass".to_string()),
GateResult::Red(reason) => serde_json::json!({...}),
```

The `to_json()` does not serialize `Warn` — it's stripped. Operator has no way to see what the Skeptic warned about.

**The Tessl invariant:** "every non-passing verdict carries its reason in operator-visible telemetry".

### Gap G4 — Hardcoded 100-LOC evidence floor (P3 violation, A-6.1)

`daemon/src/verifier.rs:21`:

```rust
const EVIDENCE_FLOOR_LOC: u32 = 100;
```

**The Tessl violation:** single threshold across all PR classes. A 99-LOC SQL injection fix bypasses evidence requirements. A 200-LOC test-file PR triggers the floor even when /er is Pass.

**The Tessl invariant:** "risk-ladder thresholds are tiered by file-class". Production: 10 LOC. Tests: 200 LOC. Docs: 500 LOC.

### Gap G5 — Vendor identity in code, not config (P1 violation, A-3.2)

`daemon/src/gates_compute.rs:138`:

```rust
r.author.login.contains("coderabbit")
```

**The Tessl violation:** vendor identification is hardcoded in gate logic. A future bot rename requires code change, not config change.

**The Tessl invariant:** "vendor identity is configuration, not code". `[reviewers.coderabbit] aliases = [...]`.

### Gap G6 — No availability invariant on compensating signals (P3 violation, C-1)

The vendor-waiver contract (`verifier.rs:1008-1012`) requires Skeptic::Pass + /er::Pass + !review_degraded. But NONE of these three signals themselves have an availability invariant — they can fail silently without the operator knowing.

Per MEMORY.md `2026-08-17_ironclad_audit_results`: a 17-PR matrix was systematically overstated; "7/7 PASS" claims failed at CodeRabbit rate-limited + BugBot usage-limited + no Skeptic transcripts. This is direct evidence that the current risk-tier assumption is unsafe.

**The Tessl invariant:** "every input to the compensating-coverage contract must itself have an availability invariant". Add `ReviewerHealth` ledger parallel to `VendorHealth`.

### Gap G7 — Healer is offline-only (P2 violation)

`daemon/src/healer.rs` reads CXDB, clusters terminal failures, emits a Markdown report — but requires manual invocation. No daily cron, no live dashboard, no alert on invariant going dark.

**The Tessl invariant:** "every invariant has a measurable adherence signal surfaced daily". Add daily Healer cron + alert on 0-fires-in-7-days.

## Pillar × Gap Matrix

| Gap | P1 systems | P2 analytics | P3 risk-ladder |
|-----|-----------|--------------|----------------|
| G1 BugBot substring | ● | ● | |
| G2 Detector-skipped | ● | | |
| G3 Warn-dropped | | ● | |
| G4 Hardcoded floor | | | ● |
| G5 Hardcoded vendor | ● | | |
| G6 No availability invariant | | | ● |
| G7 Healer offline | | ● | |

## The Tessl finding for this repo

The dark-factory repo is **70% of the way** to a Tessl-grade harness: the 8 gates, the vendor waiver contract, the CXDB+Healer feedback loop, the sealed holdouts. What's missing is the **explicit invariants registry** — the missing meta-artifact that says "these are the invariants we enforce, here is how each is enforced, here is how each is measured."

Per MEMORY.md (2026-08-17 ironclad audit results), the repo has already been bitten by the gap: a "7/7 PASS" matrix that was false because the underlying reviewers (CodeRabbit, BugBot, Skeptic) were themselves rate-limited. **That incident IS the Tessl P3 risk-ladder failure mode**: a high-stakes claim ("17 PRs PASSED") was awarded without verifying that the supporting signals were actually live.

The fixes (Phase 4 beads) are mechanical. The harder work is the meta-discipline: every invariant must be REGISTERED + MEASURED + ENFORCED, in three places.

## What the factory will pick up

Three factory-labeled beads are filed in `beads/factory/`:

| Bead | Title | Tessl Pillar | Risk Tier |
|------|-------|--------------|-----------|
| `tessl-harness-1-detector-skipped-red.md` | Add DetectorSkipped variant → Red (R2) | P1 | CRITICAL |
| `tessl-harness-2-bugbot-structured-verdict.md` | Replace BugBot substring with structured verdict (R1) | P1+P2 | HIGH |
| `tessl-harness-3-reviewer-availability-ledger.md` | ReviewerHealth ledger + alias config (R5) | P1+P3 | HIGH |

Each bead passes the cutover-exit-criteria charter (R1-R6 default-FAIL + X1-X10) and the ironclad charter (3-skeptics-distinct). The factory's coder lane will pick them up via the `factory` label on the daemon's intake.

## How to read this analysis

- The plan docs (`gap-analysis.md`, `gate-attacks.md`, `invariant-redesigns.md`) are the research product.
- The beads (`beads/factory/tessl-harness-*.md`) are the action items.
- The adversarial pass (Phase 6) will dispatch a different-model family to attack the beads.
- `/advice` (Phase 8) is the final multi-reviewer sign-off.

**For a Tessl-style harness engineer reading this:** the invariants are mostly already there. What's missing is the discipline of explicitly registering them. This research IS that registration.