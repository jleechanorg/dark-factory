# Gate Attacks — Adversarial Lens on the 8-Green Contract

**Date:** 2026-08-25
**Branch:** `research/tessl-harness-applied-to-dark-factory`
**HEAD:** `422e86bc5e`
**Method:** 3-lens refute-by-default per attack (evidence / severity / design). An attack SURVIVES if ≥2/3 lenses confirm it's a real vacuous-pass or bypass. Surviving attacks become invariant-redesign inputs in Phase 3.

---

## Gate 1 — `ci_green` (gates_compute.rs:33-70)

### Attack 1.1 — Empty `gh pr checks` list → vacuous green

**File:line evidence:** `gates_compute.rs:51-52`: `if checks.is_empty() { "unknown" }` then `if any_pending { "unknown" }` then `if any_failed { "red" }` else green. So 0 checks → "unknown" (NOT red), and from `assess`'s `all_green` rule (`verifier.rs:128-129`), Unknown forces all_green=false. **BUT** the gate itself goes to "unknown" not "red" — meaning a PR with NO checks configured (no required CI workflows, all skipped) reports "unknown", which the merge authority treats as red-equivalent via `all_green=false`. The TRIAGE path treats this as a verification gap, not a vacuous pass.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Reading line 51 confirms empty list → "unknown". Line 1194-1199 in `verifier.rs::assess` confirms Unknown forces `all_green=false`. | **CONFIRMED** |
| Severity | Worst case: a PR to a repo with no required CI jobs stays in unverified limbo; triage surfaces it; bead is HUMAN_HELD. No silent merge. | **LOW** — already handled by `all_green` aggregator |
| Design | The empty-list branch is intentional: distinguish "no checks exist" from "checks ran and failed". | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. The empty-list branch is a feature, not a bug.

### Attack 1.2 — Pending checks mask failure

**File:line evidence:** `gates_compute.rs:54-69`: `if any_pending { "unknown" }` is checked BEFORE `if any_failed { "red" }`. A `cargo test` check that's `pending` masks a `lint` check that's `fail`.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed by reading line 63-64: `any_pending` short-circuits before `any_failed`. | **CONFIRMED** |
| Severity | Bead stays in `all_green=false` until pending clears; once all pending either pass or fail, real failure surfaces. No silent merge. | **LOW** — aggregator catches it |
| Design | This is a fail-safe ordering: don't claim red on something that might still resolve to green. | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. The aggregator path catches it; vacuous test fails.

### Attack 1.3 — Slow `gh pr checks` → timeout → empty output

**File:line evidence:** `gates_compute.rs:34-38`: `run_tool("gh", &[...], 30)` — 30-second timeout. If `gh` itself stalls (network), the tool returns empty, deserialization fails at line 47-49.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 38 sets 30s timeout. Empty output → serde_json fails → DaemonError::Parse. The gate never reaches the `is_empty()` branch; the assessment aborts. | **CONFIRMED** |
| Severity | On `Parse` error, `assess` returns `Err(DaemonError)`. The whole assessment aborts; the bead isn't promoted or parked in Attested. The Healer surfaces the parse error. | **MEDIUM** — infra-fragile but not a vacuous pass |
| Design | The 30s timeout is a hard ceiling; longer gh runs are infra issues, not gate decisions. | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. Infra-fragility, not a vacuous pass.

---

## Gate 2 — `no_conflicts` (gates_compute.rs:72-101)

### Attack 2.1 — `mergeable: UNKNOWN` → "unknown" → no red

**File:line evidence:** `gates_compute.rs:97-101`: `match view.mergeable.as_str() { "MERGEABLE" => "green", "CONFLICTING" => "red", _ => "unknown" }`. GitHub returns `UNKNOWN` while it computes; older branches can sit at `UNKNOWN` indefinitely.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed by reading line 100: anything not MERGEABLE/CONFLICTING → unknown. | **CONFIRMED** |
| Severity | `unknown` → `all_green=false` → no merge. Bead stays in flight. If GitHub never resolves UNKNOWN, the bead is wedged indefinitely. | **MEDIUM** — silent wedge, no triage signal |
| Design | This IS a real gap: UNKNOWN ≠ red, but UNKNOWN also ≠ safe. The contract says "can't verify, can't pass", but the operator has no automated retry/exit. | **CONFIRMED** — survives |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (2/3)**. Gate can report UNKNOWN for arbitrary duration; no automated retry; no surfaced reason. Bead silently wedges.

### Attack 2.2 — Stale mergeable on deleted source branch

**File:line evidence:** `gates_compute.rs:73-86` reads `gh pr view` once; no timestamp check. If the source branch was force-deleted, GitHub may still report the cached `mergeable: MERGEABLE`.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Speculative — gh pr view may report cached state, but this isn't tested in the codebase. No source-branch-existence check. | **PLAUSIBLE** |
| Severity | If a cached MERGEABLE is reported on a deleted branch, the merge would fail at GitHub side. The error would surface as a CI-fail or merge-error, not a silent pass. | **LOW** — caught downstream |
| Design | No design intent documented for cached mergeable; this is an edge case rather than a bypass. | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. Edge case caught downstream by GitHub's merge-side.

---

## Gate 3 — `coderabbit` (gates_compute.rs:103-151) — STRONGEST BYPASS

### Attack 3.1 — COMMENTED reviews filtered → "unknown" not red (KNOWN, per MEMORY.md 2026-07-06)

**File:line evidence:** `gates_compute.rs:138`: `rfind(|r| r.author.login.contains("coderabbit") && r.state != "COMMENTED")`. If ALL reviews are COMMENTED (e.g. CodeRabbit only posted nit comments), `last_coderabbit_review` is `None` → "unknown" (line 150).

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 138 explicit filter. MEMORY.md `2026-07-06_coderabbit_commented_review_and_beads_duplicate` records the fix. | **CONFIRMED** |
| Severity | A PR with ONLY COMMENTED CodeRabbit reviews → "unknown" → `all_green=false` → no merge. Already handled by the aggregator. | **LOW** — caught |
| Design | The filter is INTENTIONAL — COMMENTED reviews are nit-level, not blocking. The fix to "ignore COMMENTED" was the right move. | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. Handled by aggregator; correct design.

### Attack 3.2 — NO CodeRabbit review at all (small diffs, private forks)

**File:line evidence:** `gates_compute.rs:137-151`: `last_coderabbit_review` is `None` if no CodeRabbit review exists at all → "unknown" (line 150).

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 150 handles None explicitly. | **CONFIRMED** |
| Severity | "unknown" → `all_green=false` → no merge. Bead wedges until CodeRabbit reviews OR vendor-waiver kicks in. | **MEDIUM** — silent wedge; vendor-waiver path depends on Skeptic + /er being green |
| Design | CodeRabbit doesn't review every PR (e.g. <10 LOC diffs, private forks, no `[coderabbit]` mention in the body). The gate is supposed to surface this. | **NOT A BYPASS** — but a wedge |

**Verdict:** NOT A BYPASS but a wedge. Vendor-waiver path is the legitimate exit; if Skeptic or /er is also unavailable, the bead is stuck. **Combined with Attack 7.1 (skeptic-warn-green), this becomes a real bypass path.** See Compound Attack C1 below.

### Attack 3.3 — CodeRabbit author login mismatch (renamed bot, alt names)

**File:line evidence:** `gates_compute.rs:138`: `r.author.login.contains("coderabbit")`. Substring match. If CodeRabbit is renamed or uses a non-`coderabbit` login (e.g. `coderabbitai[bot]` historically; the substring still matches), it's caught. But if a future bot uses `crabbit` or `reviewer-bot`, it won't match.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Line 138 is a literal `.contains("coderabbit")`. | **CONFIRMED** |
| Severity | Edge case: vendor rename. Easy to detect when it happens (no reviews found). | **LOW** |
| Design | Hardcoded vendor name in gate logic. Tessl P1 invariant would be "vendor identification is configuration, not code". | **PLAUSIBLE** — survives 1/3 |

**Verdict:** NOT A BYPASS. Edge case.

---

## Gate 4 — `bugbot` (gates_compute.rs:153-190) — VACUOUS-PASS CANDIDATE

### Attack 4.1 — Substring `'fail'` matches 'feasibility', 'failure-rate', 'failing test', etc.

**File:line evidence:** `gates_compute.rs:179-181`: `let body = comment.body.to_lowercase(); if body.contains("error") || body.contains("fail")`. The literal substring "fail" matches every word containing "fail" as a substring.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 180 is a bare `.contains("fail")`. | **CONFIRMED** |
| Severity | **HIGH** — a BugBot comment like "No failures detected" → body contains "fail" → bugbot=RED. False-positive RED on routine approval comments. This DOES NOT bypass the gate (it goes red, not green), but it generates churn: every PR with a routine BugBot "no issues" comment must wait for the substring noise to clear or be manually resolved. | **HIGH — false-positive churn** |
| Design | This is a clear anti-pattern: substring matching for semantic judgment. Tessl P1 invariant: "vendor verdicts are STRUCTURED, not free-text". A `bugbot_status: "clean" | "warn" | "error"` field would fix this. | **CONFIRMED** — survives |

**Verdict:** **BYPASS CANDIDATE (inverse) — SURVIVES (3/3)**. Vacuous-pass in reverse: false-positive RED drives bead churn and operator noise. The bead DOES NOT vacuously pass — but the gate vacuously triggers. **Tessl-attack: substring matching for semantic verdict is the canonical anti-invariant.**

### Attack 4.2 — Zero BugBot comments → vacuous green

**File:line evidence:** `gates_compute.rs:175-184`: `bugbot_error_count` counts comments matching cursor|bugbot AND containing error|fail. If BugBot posts zero comments, count = 0 → "green" (line 186).

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 175-184 counts only existing comments; no presence-check. | **CONFIRMED** |
| Severity | A PR where BugBot silently failed (network, no token) → 0 comments → "green". Combined with vendor-waiver path (verifier.rs:1118), this is silent. | **HIGH** |
| Design | The detector ASSUMES BugBot ran. There is no `bugbot_pending` flag in `PrSnapshot` (MEMORY.md 2026-08-09 jleechan-8s2p phase 2 added `bugbot_pending` to `detect_vendor_cap_for`, line 1102 — but this isn't fed into the gate verdict). | **CONFIRMED** — survives |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (3/3)**. Vacuous-pass when BugBot is silently unavailable. **Tessl P1 invariant violation: "absence of negative signal ≠ positive signal"**.

---

## Gate 5 — `comments_resolved` (gates_compute.rs:192-266)

### Attack 5.1 — GraphQL `first:100` pagination cap hides unresolved threads

**File:line evidence:** `gates_compute.rs:198-199`: `reviewThreads(first:100){ nodes{ isResolved } }`. GraphQL pagination caps at 100 results per page; no `after:` cursor.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 199 is a literal `first:100`. | **CONFIRMED** |
| Severity | PRs with 100+ review threads (long-lived PRs, deep back-and-forth) silently get "green" if all 100+ returned are resolved. | **MEDIUM** — known limitation, affects long-lived PRs |
| Design | Tessl P1 invariant: "graph pagination is exhaustive, not capped". | **CONFIRMED** — survives 2/3 |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (2/3)**. Pagination cap is a known tessl invariant violation.

### Attack 5.2 — Missing `pullRequest` in GraphQL response → 0 unresolved → green

**File:line evidence:** `gates_compute.rs:258-260`: `if let Some(pr_data) = gql.data.repository.pull_request { unresolved_thread_count = ... }`. If `pull_request` is `None` (deleted PR, no auth, API error returned 200 with empty data), `unresolved_thread_count = 0` → "green" (line 262).

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 258 unwraps `Option`; None branch leaves count = 0. | **CONFIRMED** |
| Severity | Silent green on a deleted PR. The PR can't be merged anyway (GitHub rejects), so this is caught downstream — but it's still a vacuous-pass that doesn't surface as Unknown. | **LOW** — caught at GitHub merge time |
| Design | `Option` is used because `pull_request` can be null on permission errors; the design SHOULD surface "can't query" as Unknown, not 0. | **CONFIRMED** — survives 2/3 |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (2/3)**. Vacuous-pass when GraphQL returns partial/empty data.

---

## Gate 6 — `evidence_review` (verifier.rs:578-640)

### Attack 6.1 — LOC floor bypass: 99-LOC production bug PR gets unconditional green

**File:line evidence:** `verifier.rs:631-639`: `if evidence.non_test_changed_loc <= EVIDENCE_FLOOR_LOC { GateResult::Green } else { ...Red }`. The floor is hardcoded at line 21: `const EVIDENCE_FLOOR_LOC: u32 = 100`.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 21 constant + line 631 floor check. | **CONFIRMED** |
| Severity | **HIGH** — a 99-LOC production bug PR (e.g. SQL injection fix, hardcoded secret, broken auth) requires NO evidence: no canonical gist, no /er verdict, no skeptic. The /er verdict IS required (line 580-594), but for non-production (line 595) the bar is lower (Pass or Partial). A 99-LOC test-repo PR with /er=Partial passes. | **HIGH** — designed gap, not a bug |
| Design | The floor IS a Tessl P3 risk-ladder decision: small diffs don't need the full evidence stack. The question is whether 100 LOC is the right threshold. Tessl would propose: tiered floor (10/100/500 LOC) keyed on file-class (test vs production). | **DESIGN INTENT** — but undersized for production |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (2/3)**. The 100-LOC floor is a hardcoded risk-ladder decision. Tessl P3 invariant: "risk-ladder thresholds are configurable + tiered, not hardcoded". A 99-LOC SQL injection fix is a real risk; the floor shouldn't blindly trust LOC count.

### Attack 6.2 — `/er` verdict = `Inconclusive` → red (production); `Partial` → green (non-production)

**File:line evidence:** `verifier.rs:591-593` (production): `ErVerdict::Inconclusive => Red`. `verifier.rs:600` (non-production): `Pass | Partial => {}` (continues). So for non-production PRs, /er=Partial is sufficient to pass the /er check.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 600 explicit Partial-OK branch. | **CONFIRMED** |
| Severity | A non-production (test-repo) PR with /er=Partial gets green even if /er explicitly noted concerns. The Partial verdict means "some checks passed, some failed". | **MEDIUM** — designed partial-acceptance |
| Design | Tessl P1 invariant: "Partial is not the same as Pass". The gate treats them identically for non-production. The design choice is reasonable for fast-lane, but the contract isn't surfaced to operators. | **PLAUSIBLE** |

**Verdict:** NOT A BYPASS but a designed gap. Surface the /er=Partial as "evidence concerns flagged" in the telemetry, not just a green.

### Attack 6.3 — `EvidenceGistStatus::Pending` (transient gh outage) → Unknown → all_green=false

**File:line evidence:** `verifier.rs:620-622`: `EvidenceGistStatus::Pending(reason) => Unknown`. This is INTENTIONAL — a transient gh blip must not churn a reroll. But it's also a silent stall: no automated retry, no operator notification.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 620 explicit Pending branch. | **CONFIRMED** |
| Severity | Bead wedges during gh outage. No automated recovery path. | **MEDIUM** — infra-fragile |
| Design | Design intent (r5 finding 3): transient failures stay Unknown, not Red. Tessl would propose: bounded retry with exponential backoff before promoting to Pending. | **NOT A BYPASS** — but a wedge |

**Verdict:** NOT A BYPASS. Designed behavior.

---

## Gate 7 — `skeptic` (verifier.rs:905-921)

### Attack 7.1 — `SkepticVerdict::Warn` → Green (warn is non-blocking)

**File:line evidence:** `verifier.rs:909`: `Some(SkepticVerdict::Warn(_)) => GateResult::Green`. The skeptic can say "I see problems" and the gate passes.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 909 explicit Warn->Green. | **CONFIRMED** |
| Severity | The Skeptic is the LOAD-BEARING input to vendor waivers (verifier.rs:1009). If Skeptic returns Warn, vendor waiver requires Skeptic::Pass (line 1009). So Warn ≠ waiver-path eligibility. **However**, for non-waiver paths, a Warn skeptic doesn't block — the gate is green. The Warn text is dropped silently. | **HIGH** — operator cannot see what was warned |
| Design | The design intent (line 192): "`Warn` is non-blocking — it still counts as `Green` for gate 7 but the caller may choose to surface the warning text elsewhere". The "may" is doing a lot. The Warn text is currently DROPPED — it doesn't appear in `to_json()` output (line 159-160 only emits the verdict enum). | **CONFIRMED** — survives 2/3 |

**Verdict:** **BYPASS CANDIDATE — SURVIVES (2/3)**. The Warn verdict is dropped silently from telemetry. Operator has no way to see what was warned. Combined with Attack 3.2 (no CodeRabbit review) → vendor waiver → no operator visibility.

### Attack 7.2 — Cross-model guarantee downgrade: only fires if `review_degraded == true`

**File:line evidence:** `verifier.rs:1178-1184`: cross-model downgrade is conditional on `evidence.review_degraded == true`. The downgrade fires only when `compute_review_degraded(reviewers)` returns true.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 1178 explicit `match (skeptic_gate, evidence.review_degraded)`. | **CONFIRMED** |
| Severity | `compute_review_degraded` is at line 316; per the docs (line 307), it requires ≥2 DISTINCT non-empty model families. If a reviewer pool has 2 entries from `claude-sonnet-4-6` and `claude-sonnet-4-5`, both same family → degraded=true (correct). But if the pool has 2 entries from the same family with a TYPO, the families list collapses to 1 → degraded=true (correct). If the pool has 1 entry from family A and 1 entry from family A+B (vendor_aliases mishap), families could falsely show 2 → degraded=false → NO downgrade → Skeptic::Pass without cross-model. | **PLAUSIBLE** — needs verification |
| Design | The contract is "two distinct families". Implementation may have edge cases around vendor_aliases resolution. | **PLAUSIBLE** — survives 1/3 |

**Verdict:** PLAUSIBLE BYPASS. Need to verify vendor_aliases.rs.

---

## Gate 8 — `vacuous_red_green` (verifier.rs:955-986) — VACUOUS-PASS HOTSPOT

### Attack 8.1 — `BaselineFailed` → Green (infra failure gates vacuous detection as PASS)

**File:line evidence:** `verifier.rs:965`: `VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green`. The detector couldn't materialize the base worktree (no GH_TOKEN, gh pr view failed) → Green.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 965 explicit BaselineFailed->Green. | **CONFIRMED** |
| Severity | **CRITICAL** — the gate EXISTS to catch vacuous tests. When the detector can't run (the most likely failure mode: GH_TOKEN missing in CI), the gate PASSES. This is the exact pattern from PR #413 (2026-07-21) and is acknowledged in the comment (line 946-954). The intent is "infra failures fall through", but the EFFECT is that any infra failure = vacuous-pass. | **CRITICAL** — design acknowledges but doesn't fix |
| Design | The design intent (line 936-940) is documented: "treating this as Unknown makes all_green=false for every PR on a runner without credentials and deadlocks the bead in Attested." But the side-effect is that a misconfigured runner = universal vacuous-pass. The Healer is supposed to surface this — but Healer is offline. | **CONFIRMED** — survives 3/3 |

**Verdict:** **CRITICAL BYPASS — SURVIVES (3/3)**. The vacuous-test detector's entire purpose is negated by infra failure. Tessl P1 invariant: "infra failure ≠ pass". Need a separate signal that the detector DIDN'T run (vs DID run and detected vacuous).

### Attack 8.2 — `NotProvided` → Green (test-repo PRs and no-test PRs)

**File:line evidence:** `verifier.rs:957`: `VacuousRedGreenStatus::NotProvided => GateResult::Green`. Test-repo PRs (per gate 7 exemption, line 41-42) get unconditional green.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | Confirmed — line 957 explicit NotProvided->Green. | **CONFIRMED** |
| Severity | Test-repo PRs are fast-lane; they don't run the vacuous detector. This is by design (line 41-42). | **NOT A BYPASS** — designed for fast-lane |
| Design | The design intent is correct: test-repo changes can't have vacuous tests. | **NOT A BYPASS** |

**Verdict:** NOT A BYPASS. Design intent.

---

## Compound Attack C1 — Vendor-Waiver Cascade (Attack 3.2 + 7.1 + 8.1)

**Scenario:** A PR lands in a repo where:
- CodeRabbit doesn't review (Attack 3.2) → "unknown"
- Skeptic returns Warn (Attack 7.1) → "green" (warn non-blocking)
- Detector can't run (Attack 8.1 BaselineFailed) → "green"
- BugBot silently failed (Attack 4.2) → "green" (no comments)

**Outcome:** If Skeptic is Warn but a separate review detected vendor outage, the vendor waiver path is ELIGIBLE because:
- CodeRabbit waiver requires Skeptic::Pass (verifier.rs:1009). Skeptic::Warn ≠ Pass → waiver INELIGIBLE.
- BUT the gate-level verdict is Green (line 909). All_green is computed across the 8 gates. If gates 1-5, 7, 8 are green and gate 3 is unknown, all_green = false → no merge. **Safe.**

**However:** if `apply_vendor_waiver` (verifier.rs:1118) flips gate 3 from Unknown to Waived, and `compensating_coverage_green` (line 1008) returns true (Skeptic::Pass, /er::Pass, !review_degraded), then ALL 8 gates are green-equivalent → merge proceeds. The compound attack exploits the waiver path.

| Lens | Refutation attempt | Verdict |
|------|---------------------|---------|
| Evidence | All three sub-attacks are file:line verified. | **CONFIRMED** |
| Severity | The waiver contract is intentional and well-tested (bead jleechan-jsby), but it depends on Skeptic::Pass + /er::Pass + !review_degraded ALL being true. If any of those three is itself unreliable (e.g. Skeptic::Pass without actually running, or /er::Pass without /er actually running), the cascade breaks. | **HIGH** |
| Design | The contract is documented; the risk is in the SLA on the three compensating signals. | **CONFIRMED** — survives 2/3 |

**Verdict:** **COMPOUND BYPASS — SURVIVES (2/3)**. Tessl P3 invariant: "every input to the compensating-coverage contract must itself have an availability invariant". Today, the only availability signal is the vendor_health ledger; Skeptic and /er have no equivalent.

---

## Surviving Attacks — Summary Table

| # | Gate | Attack | Lens Survival | Severity |
|---|------|--------|---------------|----------|
| 2.1 | no_conflicts | UNKNOWN wedges indefinitely | 2/3 | MEDIUM |
| 4.1 | bugbot | Substring 'fail' false-positive | 3/3 | HIGH (false-positive churn) |
| 4.2 | bugbot | Zero comments → vacuous green | 3/3 | HIGH |
| 5.1 | comments_resolved | first:100 pagination cap | 2/3 | MEDIUM |
| 5.2 | comments_resolved | None pullRequest → 0 unresolved | 2/3 | LOW (caught downstream) |
| 6.1 | evidence_review | 100-LOC floor is hardcoded | 2/3 | HIGH |
| 7.1 | skeptic | Warn text dropped from telemetry | 2/3 | HIGH |
| 7.2 | skeptic | Vendor alias resolution edge cases | 1/3 | PLAUSIBLE |
| 8.1 | vacuous_red_green | BaselineFailed → Green | 3/3 | CRITICAL |
| C1 | vendor-waiver | Compound: Skeptic+ER+cross-model SLA | 2/3 | HIGH |

**Top 5 critical attacks (≥2/3, severity HIGH/CRITICAL):** 4.1, 4.2, 6.1, 7.1, 8.1, C1.

These flow into Phase 3 as invariant-redesign inputs.

## Codex cross-model review (rule 12, 2026-08-25)

`codex-pair-verifier` agentId `ac2f46420d36a8511` ran 10 spot-checks against HEAD `422e86bc5e`. Verdict per attack: PLAUSIBLE — all 6 redesigns address real bypasses confirmed via grep reproductions. Codex surfaced a common defect class: the initial redesign proposals favored new infrastructure over extending the existing substrate.

Revisions are documented in `invariant-redesigns.md` § Codex cross-model review and reflected in the 3 factory beads at `beads/factory/tessl-harness-*.md` (commit `f2ebb19b24`).