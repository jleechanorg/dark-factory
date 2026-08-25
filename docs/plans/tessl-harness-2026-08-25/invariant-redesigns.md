# Tessl-Style Invariant Redesigns

**Date:** 2026-08-25
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD:** `422e86bc5e`
**Method:** For each surviving attack (Phase 2), propose (a) the invariant in plain language, (b) the enforcement mechanism (CI gate / AST-grep / LLM verifier / .dot node), (c) the migration path, and (d) the Tessl risk-tier classification.

The five surviving attacks drive five redesigns, plus a sixth meta-redesign for the analytics gap that emerges when all five are in place.

---

## Redesign R1 — Replace BugBot substring matching with structured verdict (A-4.1, A-4.2)

**Attacks addressed:** A-4.1 (substring 'fail' false-positive churn), A-4.2 (zero comments = vacuous green).

### Invariant

**R1.I1:** "A vendor verdict is a structured field, not free-text." Specifically:
- Every vendor (CodeRabbit, BugBot, Skeptic) publishes its verdict as a typed field (e.g. `bugbot_status: "clean" | "warn" | "error"`).
- The verifier reads ONLY the structured field. Free-text is NEVER used to compute a verdict.
- If the structured field is absent (vendor didn't run), the gate reports `Unknown("vendor_pending")`, NOT green.

### Enforcement mechanism

**Layered enforcement (Tessl P1 + P2):**
1. **CI gate** — `ci/build/structural-verdict.sh`: parses every PR-comment from `cursor[bot]` / `bugbot[bot]` and asserts the comment contains a typed marker (e.g. `<!-- bugbot-verdict: clean -->` HTML comment). Missing marker → CI failure on the coder-side, not on the verifier-side. This forces coders to ensure the vendor ran AND published a structured verdict.
2. **AST-grep rule** — `ast-grep/verifier-rs/bugbot-no-substring.yml`:
   ```yaml
   pattern: 'comment.body.to_lowercase().contains("fail")'
   message: "R1: substring 'fail' is forbidden for vendor verdicts — use the typed $bugbot_status field instead"
   ```
3. **LLM verifier** — `verifiers/bugbot-structured.md`: a narrow verifier that scans the last N BugBot comments and asserts each carries a structured marker.
4. **.dot pipeline node** — `bugbot_structured_review`: a new pipeline node that pre-processes BugBot comments to extract structured verdicts before `gate_4` runs.

### Migration path

- Phase 1 (1 PR): introduce `bugbot_status` field in `PrSnapshot`. Read both substring + structured; if structured present, use structured; fall back to substring with a warning.
- Phase 2 (1 PR): flip default to structured-only; substring as a warning telemetry.
- Phase 3 (1 PR): remove substring code; assert AST-grep rule passes.

### Tessl P3 risk-tier

- **Risk tier: HIGH.** Vendor verdicts are load-bearing for merge authority. Removing substring matching without the structured fallback = every PR with a BugBot substring-noise comment is suddenly unknown.
- **Blast-radius control:** keep substring code as a `legacy_warning` telemetry for 2 release cycles. Block flip on `/er verdict == Pass`.

---

## Redesign R2 — Separate "detector didn't run" from "detector ran and passed" (A-8.1)

**Attacks addressed:** A-8.1 (BaselineFailed → Green = vacuous-pass when detector can't run).

### Invariant

**R2.I1:** "The vacuous-test detector's absence is distinct from its absence-of-finding." Specifically:
- Add `VacuousRedGreenStatus::DetectorSkipped(reason)` — emitted when the detector subprocess couldn't even start (GH_TOKEN missing, cargo not found, no manifest).
- This state maps to `GateResult::Red("vacuous detector did not run — operator must verify infra")` — NOT green, NOT unknown. A Red that explicitly says "infra check needed".
- The aggregator's `all_green` treats this Red the same as Vacuous: bead blocked until operator manually verifies.

### Enforcement mechanism

1. **Type system** — add the new variant to `VacuousRedGreenStatus` enum in `vacuous_red_green.rs:64-90`.
2. **Detective trigger** — in `tick.rs` where `BaselineFailed` is mapped to `Green` (line 965), add a pre-check: if the failure reason is "GH_TOKEN missing" or "gh pr view failed", map to `DetectorSkipped` → Red.
3. **Operator UI** — surface `DetectorSkipped` as a distinct bead state ("INFRA-VERIFICATION-NEEDED") rather than mixed into Attested.
4. **Telemetry** — emit a daily count of DetectorSkipped → a load-bearing signal for the operator dashboard.

### Migration path

- Phase 1 (1 PR): add `DetectorSkipped` variant + Red mapping. Don't change `BaselineFailed` mapping yet (keep it Green for backward compat).
- Phase 2 (1 PR): flip `BaselineFailed` to map to `DetectorSkipped` (operator-visible Red). Add a kill-switch `DARK_FACTORY_DISABLE_DETECTOR_SKIP_RED=1` for emergency rollback.
- Phase 3 (1 PR): remove the kill-switch after 2 weeks of stable operation.

### Tessl P3 risk-tier

- **Risk tier: CRITICAL.** The vacuous detector is THE gate designed to catch fake coverage. If it gates-vacuous-pass when it can't run, the entire purpose of the gate is nullified for that PR. **This is the highest-priority redesign.**
- **Blast-radius control:** the kill-switch is non-negotiable. A bad flip here means every PR with detector-infra-issues wedges in operator-verification, but that's better than vacuous-pass.

---

## Redesign R3 — Surface Skeptic::Warn text in `to_json()` telemetry (A-7.1)

**Attacks addressed:** A-7.1 (Warn text dropped from gate_assessment JSONL).

### Invariant

**R3.I1:** "Every non-passing verdict carries its reason in operator-visible telemetry." Specifically:
- `to_json()` (verifier.rs:156-189) must include the `Warn(_)` text in the `evidence` array when the verdict is Warn.
- Operators MUST be able to grep gate_assessment JSONL for `skeptic_warn` and see what was warned.
- A PR with a Warn verdict is NOT blocked, but the warning text is visible to the operator.

### Enforcement mechanism

1. **Type-level change** — `GateResult::to_json()` serializes Warn text:
   ```rust
   GateResult::Warn(reason) => serde_json::json!({
       "verdict": "warn",
       "evidence": [reason],
   })
   ```
2. **Schema test** — `tests/test_verifier_to_json_warn_carries_reason.rs`: assert that the JSON includes the Warn text and the literal `verdict: "warn"`.
3. **Telemetry grep** — add a daily CXDB query that surfaces all `skeptic_warn` events from the last 24h to a `skeptic_warnings.md` digest.

### Migration path

- 1 PR: change `to_json()` to include Warn text. Schema test. Done.

### Tessl P3 risk-tier

- **Risk tier: HIGH.** This is a 1-PR fix. The risk is zero — Warn text already exists in memory; we just surface it.
- **Blast-radius control:** none needed; this is purely additive telemetry.

---

## Redesign R4 — Tiered LOC floor keyed on file-class (A-6.1)

**Attacks addressed:** A-6.1 (100-LOC floor hardcoded; 99-LOC production bug = no evidence).

### Invariant

**R4.I1:** "The evidence floor is tiered by risk-class, not a single LOC threshold." Specifically:
- Replace `EVIDENCE_FLOOR_LOC = 100` (verifier.rs:21) with a tiered policy:
  - `production/*.rs`, `production/*.py`, schema migrations: floor = 10 LOC. ANY change requires verified evidence gist.
  - Test files (`tests/`, `*_test.rs`, `*_test.py`): floor = 200 LOC.
  - Documentation, examples, fixtures: floor = 500 LOC.
- The tier is computed from `evidence.touched_files` (a new field on `PrEvidence`).
- The tier is operator-visible: gate_assessment JSONL includes `evidence_floor_tier: "production:10"`.

### Enforcement mechanism

1. **Type-level** — add `TouchedFileClass { Production, Test, Docs }` to `PrEvidence` in `verifier.rs`.
2. **Derivation** — populate from `gh pr view --json files` (already part of `PrSnapshot`).
3. **Tier map** — config in `config/daemon.toml`:
   ```toml
   [evidence_floor]
   production_loc = 10
   test_loc = 200
   docs_loc = 500
   ```
4. **Schema test** — for each tier, assert the gate is Red when LOC > floor AND evidence_gist=NotProvided.
5. **Backwards-compat** — if `touched_files` is empty (legacy), default to "production:10" (the strictest tier) — fail-closed.

### Migration path

- Phase 1 (1 PR): add `TouchedFileClass` enum + derivation; keep `EVIDENCE_FLOOR_LOC = 100` as the fallback when classification fails. Telemetry emits `evidence_floor_tier` field but the gate still uses 100.
- Phase 2 (1 PR): flip default to tier-based; keep 100 as fallback for "unclassified" PRs.
- Phase 3 (1 PR): unclassified → strictest tier (production:10); remove 100 entirely.

### Tessl P3 risk-tier

- **Risk tier: HIGH.** Lowering the floor to 10 for production means nearly EVERY production PR needs a verified evidence gist. Operators must be trained to attach gists.
- **Blast-radius control:** Phase 1/2 keep the legacy floor as fallback; Phase 3 (strict) is opt-in via `DARK_FACTORY_STRICT_EVIDENCE_FLOOR=1`.

---

## Redesign R5 — Replace gate-level substring matching with structured vendor identifiers (A-3.2, A-5.1, A-5.2, A-2.1, C-1)

**Attacks addressed:** Multiple — substring vendor names, None pullRequest, UNKNOWN wedges, compound waiver cascade.

### Invariant

**R5.I1:** "Vendor identity is configuration, not code; every reviewer has an availability invariant."

Specifically:
- Move `r.author.login.contains("coderabbit")` to a config table: `[reviewers.coderabbit] aliases = ["coderabbit", "coderabbitai[bot]", "cr-reviewer[bot]"]`. Same for `bugbot`, `cursor[bot]`, `claude-reviewer[bot]`.
- Add `reviewer_health_ledger` parallel to `vendor_health::VendorHealthLedger`. Tracks per-reviewer availability (PRs reviewed in last 1h, last comment timestamp, last response time). A reviewer with 0 reviews in 24h = `Capped`.
- The vendor-waiver contract (`verifier.rs:1118`) requires `reviewer_health_ledger::Skeptic::Pass` AND `/er::Pass` AND `!review_degraded`. If Skeptic or /er are themselves `Capped`, the waiver is INELIGIBLE — the bead wedges with a clear operator message.

### Enforcement mechanism

1. **Config** — `config/reviewers.toml` with alias tables.
2. **Ledger** — extend `vendor_health.rs` with `ReviewerHealth` per reviewer (skeptic, er, cold_reviewer, etc.).
3. **Gate-level** — `gates_compute.rs` reads alias config, not hardcoded substrings.
4. **Test** — `tests/test_gate_alias_config_loads.rs`: assert all configured aliases resolve; missing alias → test fail.
5. **Telemetry** — daily `reviewer_health.jsonl` with per-reviewer status.

### Migration path

- Phase 1 (1 PR): add config table + alias resolution. Hardcoded substring stays as fallback.
- Phase 2 (1 PR): add `ReviewerHealth` ledger + `compute_reviewer_health` reading from CXDB.
- Phase 3 (1 PR): apply ledger to vendor-waiver contract. Waiver requires all three compensating signals green; Capped on any one → waiver ineligible.

### Tessl P3 risk-tier

- **Risk tier: HIGH.** This is the largest redesign — touches config + ledger + gate logic + waiver contract.
- **Blast-radius control:** Phase 3 has a kill-switch `DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1`.

---

## Redesign R6 — Analytics dashboard for invariant adherence (meta-redesign)

**Origin:** Tessl P2 finding — once the above 5 redesigns are in place, the operator needs a live dashboard showing: per-gate pass rate, per-attack surviving count, per-reviewer availability, per-tier evidence floor hit rate. Healer exists but is offline-only.

### Invariant

**R6.I1:** "Every invariant has a measurable adherence signal surfaced daily."

Specifically:
- Daily Healer run produces `docs/heuristic/invariant-adherence-<date>.md` with:
  - Per-gate pass rate (last 24h, last 7d, last 30d)
  - Per-attack surviving count (gates 1-8 × attack matrix)
  - Per-reviewer availability (% reviews completed in 24h)
  - Per-tier evidence floor hit rate (% production PRs with verified gist)
- The dashboard includes a "fires" column: how often does each invariant trigger a block vs pass.

### Enforcement mechanism

1. **Cron** — daily at 04:00 UTC, run Healer in dashboard mode.
2. **Output** — committed to `docs/heuristic/` in the research branch.
3. **Alert** — if any invariant has 0 fires in 7 days, alert ("invariant may be dead-on-arrival").
4. **Trend** — week-over-week deltas.

### Migration path

- 1 PR: add Healer dashboard mode + daily cron + alert.

### Tessl P3 risk-tier

- **Risk tier: LOW.** Pure analytics; no production behavior change.

---

## Summary Table — Redesigns → Attacks → Tier → Migration Cost

| ID | Tessl Pillar | Attacks Addressed | Risk Tier | PRs | Critical-Path |
|----|--------------|-------------------|-----------|-----|---------------|
| R1 | P1+P2 | A-4.1, A-4.2 | HIGH | 3 | BugBot structured verdict |
| R2 | P1 | A-8.1 | **CRITICAL** | 1 (revised) | BaselineFailed → Red (flip existing variant, no new infra) |
| R3 | P2 | A-7.1 | HIGH | 1 | Skeptic::Warn telemetry |
| R4 | P3 | A-6.1 | HIGH | 3 | Tiered evidence floor |
| R5 | P1+P3 | A-2.1, A-3.2, A-5.1, A-5.2, C-1 | HIGH | 3 | Extend vendor_aliases.json + vendor_health.rs |
| R6 | P2 | (meta) | LOW | 1 | Invariant adherence dashboard |

**Total: ~12 PRs across 6 redesigns.** Top 3 priorities by Tessl P1+P3 weight: **R2** (CRITICAL, blocks the entire vacuous-detector purpose), **R1** (HIGH, fixes the most-flaky gate), **R5** (HIGH, foundational for the vendor-waiver contract).

Phase 4 will convert the top 3 (R2, R1, R5) into /af factory-labeled beads.

## Codex cross-model review (rule 12, 2026-08-25)

`codex-pair-verifier` agentId `ac2f46420d36a8511` reviewed the 3 factory beads.

**Verdict per bead:** PLAUSIBLE — real attack vector confirmed at HEAD `422e86bc5e` via grep reproductions.

**Common defect class across all 3 beads:** `propose-new-infrastructure-over-extend-existing-infrastructure`.

| Codex killer | Confirmed? | Revision applied |
|--------------|------------|-------------------|
| Bead 1 misfiled the enum at `vacuous_red_green.rs:64-90`; actual location is `verifier.rs:504-547`. `DetectorSkipped` variant is redundant with existing `CargoNotFound` / `PytestNotFound` / `ManifestMissing` which already map to `Unknown`. Only `BaselineFailed` is the unique bypass. | ✓ | Bead 1 collapsed from 3 PRs to 1: flip the single line `BaselineFailed(_reason) => GateResult::Green` to `Red` at `verifier.rs:965`. No new enum variant. |
| Bead 2 proposed new `bugbot_status: BugbotStatus` field; `bugbot_pending: bool` already exists at `tools.rs:222` (jleechan-8s2p). AST-grep tooling doesn't exist in repo (`grep -rn ast-grep daemon/` returns 0 hits). | ✓ | Bead 2 extends existing `bugbot_status: String` (line 207) into a typed enum; AST-grep replaced with `cargo clippy --workspace -- -D clippy::match_wildcard_for_single_variants` (deterministic coverage test). |
| Bead 3 proposed new `daemon/src/reviewer_health.rs` + `config/reviewers.toml`; existing `daemon/src/vendor_health.rs` (428 LOC, 11 unit tests, `VendorHealthLedger`) and `config/vendor_aliases.json` exist. | ✓ | Bead 3 extends `config/vendor_aliases.json` with parallel `reviewer_aliases` section (mirrors `vendor_aliases` structure); extends `VendorHealthLedger` with `ReviewerHealth` variants. **No new files.** AST-grep replaced with cargo clippy. |

**Post-revision verification:**
- 3 revised beads committed at `f2ebb19b24`.
- Bead body sizes: 2,809 / 3,542 / 3,845 chars (all < 4096).
- Attack vectors remain PLAUSIBLE; fixes are now 1 PR / 3 PRs / 3 PRs (down from 3 / 3 / 3+).