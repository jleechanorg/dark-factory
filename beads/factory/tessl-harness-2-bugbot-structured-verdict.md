---
bead_id: tessl-harness-2-bugbot-structured-verdict
title: "bugbot gate: replace substring matching with typed status field (R1 redesign, extended per codex review)"
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
  prior_beads: jleechan-8s2p (bugbot_pending: bool in tools.rs:222)
  prior_prs: phase 2 of bugbot outage handling
  plan_doc: docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md#redesign-r1
  codex_review: ac2f46420d36a8511 (PLAUSIBLE; revised to extend existing bugbot_pending, not create new field)
---

# bugbot gate: replace substring matching with typed status field

## Problem (Tessl P1+P2 violation)

`daemon/src/gates_compute.rs:179-181`:

```rust
let body = comment.body.to_lowercase();
if body.contains("error") || body.contains("fail") {
    bugbot_error_count += 1;
}
```

Two attacks:

**A-4.1 (false-positive churn):** substring `fail` matches `feasibility`, `failure-rate`, `failing test`, `failover`. Routine BugBot "no failures detected" → RED.

**A-4.2 (vacuous green):** BugBot posts zero comments → `bugbot_error_count = 0` → GREEN.

**Codex-review finding (incorporated):** `daemon/src/tools.rs:222` already carries `pub bugbot_pending: bool` (added by jleechan-8s2p). The existing `bugbot_status: String` is the right substrate to extend — not a new field. The proposed AST-grep rule is REPLACED with a deterministic `cargo test` invariant that the new enum mappings are exhaustive.

## Acceptance — 5 Green

### G1 — Extend `bugbot_status` to a typed enum
`daemon/src/tools.rs::PrSnapshot` (around line 207): extend the existing `bugbot_status: String` to:
```rust
pub bugbot_status: BugbotStatus,  // Clean | Warn | Pending | Error
```
Keep `bugbot_pending: bool` as a derived field (or remove if redundant after the enum extension).

### G2 — Gate verdict mapping
`daemon/src/gates_compute.rs` `bugbot` field (line 186-190):
```rust
BugbotStatus::Clean => "green",
BugbotStatus::Warn => "warn",  // not blocking; surfaced in telemetry
BugbotStatus::Error => "red",
BugbotStatus::Pending => "unknown",  // explicit, not vacuous
```
Substring matching (`body.contains("fail")`) is REPLACED by enum dispatch.

### G3 — Backfill from existing snapshots
Existing `bugbot_status: String` already populated via `gh pr view`. Add a derivation step: parse `clean | warn | error | pending` from the string; missing → `Pending`. This preserves existing data semantics.

### G4 — Deterministic coverage test
`tests/test_bugbot_status_exhaustive.rs`: assert the enum's match in `gates_compute.rs` is exhaustive (clippy lint `match_wildcard_for_single_variants`).

### G5 — Migration safety
- Phase 1 (this PR): add `BugbotStatus` enum; `gates_compute.rs` reads BOTH substring (legacy) AND enum; prefers enum if non-default; emits `bugbot_structured_verdict_seen=true|false` telemetry.
- Phase 2 (next PR): flip default to enum-only. Kill-switch `DARK_FACTORY_LEGACY_BUGBOT_SUBSTRING=1` reverts.
- Phase 3 (final PR): remove substring code; coverage test prevents reintroduction.

## Migration contract (3 PRs)

Each PR passes `cargo test --workspace` + 4 daemon-test runs. Each includes a `test_bugbot_structured_verdict_*` test.

## Out of scope

R5 (ReviewerHealth ledger) — applies the same pattern to Skeptic + /er.
`vendor_aliases.rs` extension for vendor aliases — separate concern.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ cargo clippy --workspace -- -D clippy::match_wildcard_for_single_variants
$ env DARK_FACTORY_LEGACY_BUGBOT_SUBSTRING=1 cargo run --bin dark-factory-daemon -- --smoke
```

## Why now

1. **High-frequency attack** — BugBot substring noise has documented churn.
2. **Direct Tessl P1 example** — "skills paired with checks"; today BugBot verdicts have NO check.
3. **Cutover charter R1+R5**: fail-closed default + externally-anchored enforcement (clippy coverage test).
4. **Pairs with R5** — once verdicts typed, ReviewerHealth reads uniformly.
5. **Codex-verified** (PLAUSIBLE) with simplified fix extending `bugbot_status`.