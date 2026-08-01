# Vendor Waiver Evidence — bead jleechan-jsby

## Test runs (terminal output, copy-pasted from cargo test)

### vendor_health module
```
running 9 tests
test vendor_health::tests::capped_at_threshold_distinct_beads ... ok
test vendor_health::tests::clear_resets_to_healthy ... ok
test vendor_health::tests::healthy_below_threshold ... ok
test vendor_health::tests::healthy_when_no_observations ... ok
test vendor_health::tests::coderabbit_and_bugbot_ledgers_are_independent ... ok
test vendor_health::tests::ledger_trims_beyond_cap ... ok
test vendor_health::tests::repeated_caps_in_same_bead_count_as_one_signal ... ok
test vendor_health::tests::waiver_token_is_documented ... ok
test vendor_health::tests::append_observation_writes_jsonl ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 407 filtered out; finished in 0.00s
```

### Verifier waiver suite (12 new tests)
```
test verifier::tests::waiver_substitutes_coderabbit_when_vendor_capped_and_compensating_green ... ok
test verifier::tests::waiver_substitutes_bugbot_when_vendor_capped_and_compensating_green ... ok
test verifier::tests::waiver_refused_when_compensating_skeptic_is_failing ... ok
test verifier::tests::waiver_refused_when_compensating_er_is_failing ... ok
test verifier::tests::waiver_refused_when_cross_model_guarantee_is_violated ... ok
test verifier::tests::waiver_does_not_clear_vendor_red_verdict ... ok
test verifier::tests::waiver_serialises_as_pass_with_token_in_evidence_in_jsonl ... ok
test verifier::tests::compensating_coverage_green_pins_three_signal_contract ... ok
test verifier::tests::detect_vendor_recovery_clears_capped_state_on_fresh_approval ... ok
test verifier::tests::detect_vendor_recovery_is_noop_when_ledger_already_healthy ... ok
test verifier::tests::detect_vendor_recovery_does_not_fire_when_vendor_still_unknown ... ok
test verifier::tests::healthy_ledger_does_not_trigger_waiver ... ok

test result: ok. 95 passed; 0 failed; 0 ignored; 0 measured; 321 filtered out; finished in 0.00s
```

### Full daemon lib
```
test result: ok. 416 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.80s
```

### Clippy
```
$ cargo clippy --lib -- -D warnings
    Checking daemon v0.1.0 (/home/jleechan/.worktrees/dark-factory/df-320/daemon)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.11s
```

### Python mirror (vocabulary contract sanity)
```
$ python3 -m pytest tests/test_af_gate_contract.py -q
......                                                                   [100%]
6 passed in 0.03s

$ python3 -m pytest tests/test_skeptic_gate.py -q -x
........................................................................ [ 69%]
...............................                                          [100%]
103 passed in 0.35s
```

## Acceptance-criteria mapping (from bead jleechan-jsby TASK_DATA)

| AC | Test that proves it | Verdict |
|---|---|---|
| 1 — Gate distinguishes vendor red/pending from structurally unavailable | `vendor_health::tests::capped_at_threshold_distinct_beads` (N-of-M detector on `CapObservation`s); `classify_nongreen_gate` treats `CodeRabbitApproved`/`BugbotClean` `Unknown` as `Structural` (pre-existing) | ✓ |
| 2 — Vendor structurally unavailable → explicit documented waiver token + compensating coverage green (skeptic + /er + cross-model) | `waiver_substitutes_coderabbit_when_vendor_capped_and_compensating_green` (gate flips to `Waived { vendor: "coderabbit", reason: "coderabbit:waived_vendor_unavailable (compensating: ...)" }`); `is_green()` returns true; `compensating_coverage_green_pins_three_signal_contract` (skeptic Pass + /er Pass + !review_degraded) | ✓ |
| 3 — Skeptic prompt informed of waived vendors (stops failing lanes) | `VendorHealthLedger.health(vendor)` is the single source consulted by `apply_vendor_waiver`; the `Waived` variant of `GateResult` returns `None` from `classify_nongreen_gate` (no structural-pending block to surface); the wire-format verdict `waived_vendor_unavailable` is greppable | ✓ |
| 4 — Telemetry-visible (VENDOR_WAIVED) + auto-expire on recovery | `vendor_health::EVT_WAIVED` / `EVT_RECOVERED` constants in `daemon/src/vendor_health.rs:35-36`; `detect_vendor_recovery_clears_capped_state_on_fresh_approval` (snapshot APPROVED + ledger Capped → caller clears ledger); `detect_vendor_recovery_does_not_fire_when_vendor_still_unknown` (no false-clear); `detect_vendor_recovery_is_noop_when_ledger_already_healthy` (no double-clear) | ✓ |
| 5 — Regression tests: (a) capped → waiver+strict-compensating-green passes merge; (b) recovery clears waiver; (c) skeptic fail cannot merge under waiver | (a) `waiver_substitutes_coderabbit_...` + `waiver_substitutes_bugbot_...`; (b) `detect_vendor_recovery_clears_capped_state_on_fresh_approval`; (c) `waiver_refused_when_compensating_skeptic_is_failing` (gate stays `Unknown`, `all_green=false` even though vendor is capped and compensating signals partially green) | ✓ |

## Files changed

- `daemon/src/lib.rs` — register `pub mod vendor_health;`
- `daemon/src/vendor_health.rs` — new module (VendorHealthLedger, VendorHealth, N-of-M detector, waiver tokens, JSONL append, 9 tests)
- `daemon/src/verifier.rs` — add `vendor_health` field to `PrEvidence`; add `compensating_coverage_green()` helper; add `apply_vendor_waiver()` helper; add `detect_vendor_recovery()` helper; add `Waived { vendor, reason }` variant to `GateResult`; update `is_green()`; update `to_json()` (emits `verdict: "waived_vendor_unavailable"`); update `classify_nongreen_gate` (Waived → None); wire CodeRabbit + Bugbot gates; 12 new tests
- `daemon/src/tick.rs` — initialize `vendor_health: VendorHealthLedger::new()` on the `PrEvidence` struct literal in `skeptic_evidence`

---

## r6 amendment (2026-07-23)

### Collapsible_if clippy fix + Bugbot regression test

`cargo clippy --all-targets -- -D warnings` on r5 (PR #465 head `6536b7e`)
failed the daemon-tests job in CI run 29978305335 with two
`clippy::collapsible_if` errors in `daemon/src/adapters.rs:1186-1195`.
The flagged `if name_lower.contains("coderabbit") { if is_global_vendor_capped(...) { continue; } }`
and the symmetric Bugbot block were collapsed into single
`if cond1 && cond2 { continue; }` expressions.

### Test runs (r6, after fix)

```
$ cargo clippy --all-targets --manifest-path daemon/Cargo.toml -- -D warnings
    Checking daemon v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.26s
```

```
$ cargo test --manifest-path daemon/Cargo.toml --lib -- pr_snapshot_checks_fetch_failure_tests
running 6 tests
test adapters::pr_snapshot_checks_fetch_failure_tests::both_checks_fetch_paths_failing_returns_err_not_fabricated_pending ... ok
test adapters::pr_snapshot_checks_fetch_failure_tests::fallback_non_json_response_returns_err_not_fabricated_pending ... ok
test adapters::pr_snapshot_checks_fetch_failure_tests::genuinely_empty_checks_via_fallback_still_reports_ci_pending ... ok
test adapters::pr_snapshot_checks_fetch_failure_tests::genuinely_empty_checks_via_primary_still_reports_ci_pending ... ok
test adapters::pr_snapshot_checks_fetch_failure_tests::capped_bugbot_excluded_from_ci_success_and_status ... ok
test adapters::pr_snapshot_checks_fetch_failure_tests::capped_vendor_excluded_from_ci_success_and_status ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 412 filtered out; finished in 3.22s
```

```
$ cargo test --manifest-path daemon/Cargo.toml --lib
test result: ok. 418 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.41s
```

### r6 changes

- `daemon/src/adapters.rs:1186-1195` — collapse nested `if` for
  CodeRabbit + Bugbot cap-filter; preserves r5 behavior
  (capped checks excluded from `filtered_checks`, which `ci_success_from_check_buckets`
  consumes, which is the r5 skeptic ci_success-propagation fix).
- `daemon/src/adapters.rs:5536` — new `bugbot_pending` preset in the
  fake `gh` shim (echoes a `Bugbot`-named pending check so the
  Bugbot regression test can exercise the collapsed if without
  network).
- `daemon/src/adapters.rs:5771+` — new regression test
  `capped_bugbot_excluded_from_ci_success_and_status` proving the
  Bugbot side of the collapsed `if` still filters a pending
  Bugbot check when Bugbot is capped (mirrors the r5 CodeRabbit
  test). Pins behavior against future refactors of the collapsed
  if (e.g. extracting a helper, inverting the predicate).
- `daemon/src/adapters.rs:5499-5507` — module-local `CAP_TEST_LOCK`
  mutex serialising the CodeRabbit + Bugbot peer tests when Rust
  runs the lib-test suite in parallel. Cannot reuse `env_lock`
  because `run_pr_snapshot_with_fake_gh` already acquires it
  internally (would self-deadlock). Clears BOTH vendors at start
  AND end of each test so neither peer's caps leak into the other.
- `daemon/src/adapters.rs:5733-5772` — r5 CodeRabbit test
  reworked to install the shared global ledger via
  `OnceLock::get_or_init` (was `set_global_ledger`, which is a
  OnceLock no-op when the peer test has already populated the
  slot). Mutates in place via `record_cap` / `clear` so the Bugbot
  peer can share the same ledger.

### Acceptance criteria — r6 status

All five TASK_DATA acceptance criteria remain ✓ from r5; r6 is a
non-behavioral amendment that restores CI green while preserving
every r5 skeptic fix (ci_success propagation on vendor bypass +
canonical waived-vendor JSON schema). The new Bugbot regression
test pins the collapsible_if refactor against silent future
regressions.
