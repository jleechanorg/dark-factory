---
bead_id: tessl-harness-2-bugbot-structured-verdict
title: "bugbot gate: replace substring matching with structured verdict (R1 redesign)"
target_repo: jleechanorg/dark-factory
labels: factory
priority: P1
risk_tier: HIGH
tessl_pillar: P1_systems_invariants+P2_analytics
created: 2026-08-25
branch: research/tessl-harness-applied-to-dark-factory
head_sha: 422e86bc5e
refs:
  attacks: A-4.1, A-4.2
  prior_beads: jleechan-jsby, jleechan-8s2p
  plan_doc: docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md#redesign-r1
  memory: feedback_2026-07-06_coderabbit_commented_review_and_beads_duplicate
---

# bugbot gate: replace substring matching with structured verdict

## Problem (Tessl P1+P2 violation)

`daemon/src/gates_compute.rs:179-181`:

```rust
let body = comment.body.to_lowercase();
if body.contains("error") || body.contains("fail") {
    bugbot_error_count += 1;
}
```

Two attacks:

**A-4.1 (false-positive churn):** substring `fail` matches `feasibility`, `failure-rate`, `failing test`, `failover`. A routine BugBot "no failures detected" comment → body contains "fail" → bugbot=RED. Every PR with substantive BugBot feedback must wait for substring noise to clear or be manually resolved.

**A-4.2 (vacuous green):** BugBot posts zero comments (network failure, no token, vendor outage) → `bugbot_error_count = 0` → GREEN. The gate ASSUMES BugBot ran. There is no presence-check.

The Tessl invariant: "vendor verdicts are typed fields, not free-text".

## Acceptance — 5 Green

### G1 — Structured verdict field
BugBot publishes its verdict as an HTML marker in the comment:
```
<!-- bugbot-verdict: clean -->
<!-- bugbot-verdict: warn -->
<!-- bugbot-verdict: error -->
```
The verifier reads ONLY the marker. Free-text is never used.

### G2 — `bugbot_status` in PrSnapshot
`daemon/src/tools.rs` `PrSnapshot` gains:
```rust
pub bugbot_status: BugbotStatus,  // Clean | Warn | Error | Pending
```
Where `Pending = "no bugbot comments with structured marker in last 24h"`. BugBot `Pending` is distinct from `Clean` (had comments, all clean) — solves A-4.2.

### G3 — Gate verdict mapping
`daemon/src/gates_compute.rs` `bugbot` field (line 186-190) maps:
```rust
BugbotStatus::Clean => "green",
BugbotStatus::Warn => "warn",  // not blocking, but surfaced
BugbotStatus::Error => "red",
BugbotStatus::Pending => "unknown",  // explicit, not vacuous
```

### G4 — AST-grep + LLM verifier
- AST-grep rule `ast-grep/verifier-rs/bugbot-no-substring.yml` blocks any new code that uses `comment.body.contains("fail")` for vendor verdicts.
- LLM verifier `verifiers/bugbot-structured.md` scans the last 10 BugBot comments on every PR and asserts each carries a structured marker. Missing marker → coder-side CI failure (PR can't merge until vendor publishes a typed verdict).

### G5 — Migration safety
- Phase 1 (this PR): add `bugbot_status` enum; verifier reads BOTH substring (legacy) AND structured; prefers structured if present; emits telemetry `bugbot_structured_verdict_seen=true|false`.
- Phase 2 (next PR): flip default to structured-only; substring emits `legacy_warning` telemetry.
- Phase 3 (final PR): remove substring code; AST-grep rule prevents reintroduction.

## Migration contract (3 PRs)

This bead covers G1+G2+G3+G5. Each PR passes `cargo test --workspace` + 4 daemon-test runs; each includes `test_bugbot_structured_verdict_*`.

## Out of scope

R5 (ReviewerHealth ledger), vendor alias config — separate beads.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ env DARK_FACTORY_LEGACY_BUGBOT_SUBSTRING=1 cargo run --bin dark-factory-daemon -- --smoke
```

## Why now

1. **High-frequency attack** — BugBot substring noise has documented churn (MEMORY.md 2026-07-06).
2. **Direct Tessl P1 example** — "skills paired with checks"; today BugBot verdicts have NO check.
3. **Cutover charter R1+R5**: fail-closed default + externally-anchored enforcement (AST-grep rule).
4. **Pairs with R5** — once verdicts typed, ReviewerHealth can read uniformly.