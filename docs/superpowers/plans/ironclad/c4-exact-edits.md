# C4 exact edits — truthful branch-registration collision telemetry

Planning artifact only. The executor may edit exactly `daemon/src/tick.rs`,
`daemon/tests/tick_integration.rs`, and the test-only fixture
`daemon/tests/common/mod.rs` as described below. `daemon/src/dispatch.rs`
already performs the durable park for a non-transient `register_branch` error;
the production edit below only classifies that durable outcome at the tick
boundary. Do not change dispatch, recovery allow-lists, ownership, schema, or
any service configuration.

## Verified source anchors

At the supplied base (`8316b204b9871c70b8e9ecbc9647b8d9579c6d62`):

- `daemon/src/dispatch.rs`, in `dispatch_ready_with_vcs`, the
  `if let Err(err) = store.register_branch(&bead.id, &branch)` block first
  returns `phase = "register_branch"` for transient errors. For a
  non-transient error it sets `overlay.state = OverlayState::HumanHeld`, calls
  `set_human_hold_reason(&mut overlay,
  HumanHoldReason::BranchRegistrationConflict)`, saves the overlay, and then
  returns `phase = "register_branch"`. If that park save is transient it
  returns `phase = "register_branch_park_save"` instead. A non-transient park
  save error returns from dispatch rather than constructing a failure report.
  Therefore a reported non-transient `register_branch` failure itself proves
  the durable park contract; the tick fix does not need a second store read.
- `daemon/src/tick.rs`, in the dispatch-failure loop in `run_slow_tier`, the
  generic `BEAD_DISPATCH_TRANSIENT_ERROR` fallback follows the existing
  `worktree_remote_mismatch` special case. Insert the new special case
  immediately before the exact existing anchor:

  ```rust
              if failure.phase == "worktree_remote_mismatch" {
  ```

- `daemon/tests/tick_integration.rs` already contains
  `run_tick_emits_parked_human_held_for_unmapped_repo_dispatch_failure` at the
  dispatch-failure regression-test cluster. Use its `TickDeps`, telemetry JSON
  parsing, summary assertion, durable overlay assertion, and generic-event
  negative assertion as the shape for the new tests.
- `FakeStateStore` already has `fail_save_for(bead_id, OverlayState)`, which
  injects the existing transient SQLite-shaped save error. Add only one
  test-only `register_branch_error: RefCell<Option<DaemonError>>` field and
  consume it in the existing `register_branch` implementation. The exact
  `StateStore` signatures were checked read-only in `daemon/src/state.rs`; no
  delegate or new method is needed. The no-spawn assertions use the verified
  `FakeSessions::spawn` call-log format `spawn(<bead_id>)` from
  `daemon/tests/common/mod.rs`.
- `SqliteStateStore::open_in_memory_with_schema` is public and accepts
  `include_str!("../contracts/schema.sql")` from `daemon/tests/tick_integration.rs`.
  Use this real store for the collision test so the branch-registry uniqueness
  behavior is exercised rather than simulated.

## Production edit — `daemon/src/tick.rs`

Insert the following block immediately before the existing
`if failure.phase == "worktree_remote_mismatch" {` block identified above.
This is the complete proposed production code for C4; do not modify the
generic fallback or any dispatch/recovery code.

```rust
            if failure.phase == "register_branch" && !failure.transient {
                // dispatch::dispatch_ready durably parked this bead HUMAN_HELD before returning
                // this collision; park-save failures use another phase or return without a report.
                let reason = HumanHoldReason::BranchRegistrationConflict.value();
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": reason,
                        "branch": failure.branch.as_deref(),
                        "error": failure.error.as_str(),
                    }),
                )?;
                continue;
            }
```

The `phase` and `!transient` guard keeps transient registration failures on
the existing retry path. A `register_branch_park_save` failure is deliberately
not matched, so a failed park save cannot produce `PARKED_HUMAN_HELD`
telemetry. The real SQLite test below independently verifies the durable
`HUMAN_HELD`/`branch_registration_conflict` pair.

Production snippet count: 20 Rust lines added, 0 deleted. The import change
below is test-only and is a net +3 Rust lines (4 replacement lines for 1).
There is no dependency or public API change.

## Test-only fixtures — `daemon/tests/common/mod.rs` and `daemon/tests/tick_integration.rs`

### Import anchor

On the existing state import near the top of the file, change:

```rust
use daemon::state::{BeadOverlay, OverlayState, StateStore, CIRCUIT_BREAKER_PARK_REASON};
```

to:

```rust
use daemon::state::{
    BeadOverlay, OverlayState, SqliteStateStore, StateStore,
    CIRCUIT_BREAKER_PARK_REASON,
};
```

In `daemon/tests/common/mod.rs`, add the following field immediately after
the existing `fail_save_for_state` field in `FakeStateStore`:

```rust
pub register_branch_error: RefCell<Option<DaemonError>>,
```

At the existing `register_branch` implementation, immediately after the
existing `calls.push(...)` and before the `branches` mutation, insert:

```rust
        if let Some(error) = self.register_branch_error.borrow_mut().take() {
            return Err(error);
        }
```

`FakeStateStore` already derives `Default`, so the new field requires no
constructor edit. The test-only field consumes one scripted error, preserving
the existing branch/call behavior for all other tests. `fail_save_for` is
already the exact reusable transient-save fixture.

Common fixture snippet count: 4 Rust lines added (1 field + 3-line branch).

In `daemon/tests/tick_integration.rs`, insert these small shared test helpers
immediately before the existing doc comment for
`run_tick_emits_parked_human_held_for_unmapped_repo_dispatch_failure`:

```rust
fn register_branch_test_bead(id: &str) -> Bead {
    Bead {
        id: id.into(),
        title: "register branch failure fixture".into(),
        description: "target_repo: owner/repo".into(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: None,
    }
}

fn register_branch_test_llm() -> FakeLlm {
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"branch failure fixture"}"
            .into(),
    ));
    llm
}

fn register_branch_test_deps<'a>(
    scm: &'a FakeScm,
    tracker: &'a FakeTracker,
    sessions: &'a FakeSessions,
    llm: &'a FakeLlm,
    store: &'a dyn StateStore,
    vcs: &'a FakeVcs,
    cfg: &'a Config,
    telemetry_log: &'a std::path::Path,
) -> TickDeps<'a> {
    TickDeps {
        scm,
        tracker,
        sessions,
        llm,
        store,
        vcs,
        cfg,
        telemetry_log,
        vendor_health: None,
    }
}

fn register_branch_test_events(path: &std::path::Path) -> Vec<serde_json::Value> {
    let body = std::fs::read_to_string(path).expect("tick telemetry must exist");
    body.lines()
        .map(|line| serde_json::from_str(line).expect("telemetry must be JSONL"))
        .collect()
}
```

Helper snippet count: 49 Rust lines added. These helpers only remove repeated
fixture setup; they do not add a new store abstraction.

## Test 1 — real SQLite durable collision

Insert immediately after the fixture block above and before the existing
`run_tick_emits_parked_human_held_for_unmapped_repo_dispatch_failure` test.
This is the only test that uses `SqliteStateStore`; it pre-registers the exact
generated branch to another bead and then runs the real tick/dispatch path.

```rust
#[test]
fn register_branch_non_transient_collision_emits_parked_human_held_after_durable_save() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let bead_id = "register-branch-sqlite-collision";
    tracker
        .candidates
        .borrow_mut()
        .push(register_branch_test_bead(bead_id));
    let sessions = FakeSessions::new();
    let llm = register_branch_test_llm();
    let store = SqliteStateStore::open_in_memory_with_schema(include_str!(
        "../contracts/schema.sql"
    ))
    .expect("in-memory SqliteStateStore must open");
    let branch = format!("factory/{bead_id}-r1");
    store
        .register_branch("existing-branch-owner", &branch)
        .expect("the collision owner must be durable before the tick");
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_register_branch_sqlite_collision_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = register_branch_test_deps(
        &scm,
        &tracker,
        &sessions,
        &llm,
        &store,
        &vcs,
        &cfg,
        &telemetry_log,
    );
    let summary = run_tick(&deps, 0, 0).expect("collision must be isolated to this bead");
    let calls = sessions.calls.borrow().clone();
    assert!(
        !calls.iter().any(|call| call == &format!("spawn({bead_id})")),
        "a rejected branch registration must never spawn a worker; calls={:?}",
        calls
    );
    let events = register_branch_test_events(&telemetry_log);
    let parked = events.iter().find(|event| {
        event["eventType"] == "PARKED_HUMAN_HELD" && event["beadId"] == bead_id
    });
    assert!(
        parked.is_some(),
        "C4_EXPECT_DURABLE_PARK_TELEMETRY: non-transient register_branch collision must emit PARKED_HUMAN_HELD; events={events:?}"
    );

    let overlay = store
        .load(bead_id)
        .expect("durable overlay load must succeed")
        .expect("dispatch must have persisted the candidate overlay");
    assert_eq!(
        store.bead_id_for_branch(&branch).unwrap().as_deref(),
        Some("existing-branch-owner"),
        "the existing branch owner must remain registered after the rejection"
    );
    assert_eq!(
        overlay.branch, None,
        "the rejected candidate overlay must not claim the collided branch"
    );
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "the collision must remain durably HUMAN_HELD"
    );
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("branch_registration_conflict"),
        "the durable reason must be BranchRegistrationConflict"
    );
    assert_eq!(summary.beads_parked_human_held, 1);
    assert_eq!(summary.beads_dispatched, 0);

    let parked = parked.expect("the marker assertion above must retain the event");
    assert_eq!(parked["lifecycleState"], "HUMAN_HELD");
    assert_eq!(parked["context"]["reason"], "branch_registration_conflict");
    assert_eq!(parked["context"]["branch"], branch);
    assert!(
        parked["context"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("already registered")),
        "telemetry must retain the durable collision error: {parked:?}"
    );
    assert!(!events.iter().any(|event| {
        event["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR" && event["beadId"] == bead_id
    }));

    let _ = std::fs::remove_file(&telemetry_log);
}
```

The first telemetry assertion must keep the literal
`C4_EXPECT_DURABLE_PARK_TELEMETRY` in its failure message. Keep the durable
state assertions after that assertion so the pre-fix RED run reports the
intended missing telemetry marker first, while still proving the dispatch layer
already persisted the correct state once the production edit is present.

## Test 2 — transient registration remains retryable

Insert directly after the first test. It must pass before the production fix.
The injected `Tool` error is transient, so the dispatch report uses the same
generic retry event and never parks.

```rust
#[test]
fn register_branch_transient_failure_remains_retryable_without_parking() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let bead_id = "register-branch-transient";
    tracker
        .candidates
        .borrow_mut()
        .push(register_branch_test_bead(bead_id));
    let sessions = FakeSessions::new();
    let llm = register_branch_test_llm();
    let store = FakeStateStore::new();
    *store.register_branch_error.borrow_mut() = Some(DaemonError::Tool {
        tool: "sqlite".into(),
        rc: 1,
        stderr: "scripted transient register_branch failure".into(),
    });
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_register_branch_transient_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = register_branch_test_deps(
        &scm,
        &tracker,
        &sessions,
        &llm,
        &store,
        &vcs,
        &cfg,
        &telemetry_log,
    );
    let summary = run_tick(&deps, 0, 0).expect("transient registration failure is retryable");
    let calls = sessions.calls.borrow().clone();
    assert!(
        !calls.iter().any(|call| call == &format!("spawn({bead_id})")),
        "a transient registration failure must not spawn a worker; calls={:?}",
        calls
    );
    let overlay = store
        .load(bead_id)
        .expect("overlay load must succeed")
        .expect("intake must have persisted the candidate");
    assert_eq!(overlay.state, OverlayState::Queued);
    assert_eq!(overlay.park_reason, None);
    assert_eq!(summary.beads_parked_human_held, 0);
    assert_eq!(summary.beads_dispatched, 0);

    let events = register_branch_test_events(&telemetry_log);
    assert!(events.iter().any(|event| {
        event["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR"
            && event["beadId"] == bead_id
            && event["context"]["phase"] == "register_branch"
            && event["context"]["transient"] == true
    }));
    assert!(!events.iter().any(|event| {
        event["eventType"] == "PARKED_HUMAN_HELD" && event["beadId"] == bead_id
    }));

    let _ = std::fs::remove_file(&telemetry_log);
}
```

## Test 3 — transient park-save failure suppresses park telemetry

Insert directly after the second test. The injected non-transient registration
error enters dispatch's existing park path, but the injected transient save
error causes `phase = "register_branch_park_save"`; the tick special case must
not match it. This also passes before the production fix.

```rust
#[test]
fn register_branch_park_save_failure_suppresses_park_telemetry() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let bead_id = "register-branch-park-save";
    tracker
        .candidates
        .borrow_mut()
        .push(register_branch_test_bead(bead_id));
    let sessions = FakeSessions::new();
    let llm = register_branch_test_llm();
    let store = FakeStateStore::new();
    *store.register_branch_error.borrow_mut() = Some(DaemonError::Config(
        "scripted permanent branch registration collision".into(),
    ));
    store.fail_save_for(bead_id, OverlayState::HumanHeld);
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_register_branch_park_save_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = register_branch_test_deps(
        &scm,
        &tracker,
        &sessions,
        &llm,
        &store,
        &vcs,
        &cfg,
        &telemetry_log,
    );
    let summary = run_tick(&deps, 0, 0).expect("transient park-save failure is retryable");
    let calls = sessions.calls.borrow().clone();
    assert!(
        !calls.iter().any(|call| call == &format!("spawn({bead_id})")),
        "a failed park save must not spawn a worker; calls={:?}",
        calls
    );
    let overlay = store
        .load(bead_id)
        .expect("overlay load must succeed")
        .expect("intake must have persisted the candidate");
    assert_eq!(overlay.state, OverlayState::Queued);
    assert_eq!(overlay.park_reason, None);
    assert_eq!(summary.beads_parked_human_held, 0);

    let events = register_branch_test_events(&telemetry_log);
    assert!(events.iter().any(|event| {
        event["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR"
            && event["beadId"] == bead_id
            && event["context"]["phase"] == "register_branch_park_save"
            && event["context"]["transient"] == true
    }));
    assert!(!events.iter().any(|event| {
        event["eventType"] == "PARKED_HUMAN_HELD" && event["beadId"] == bead_id
    }));

    let _ = std::fs::remove_file(&telemetry_log);
}
```

Test snippet counts: 95 + 64 + 62 = 221 Rust lines added, all in
`daemon/tests/tick_integration.rs`. The complete test change is 279 changed Rust lines
(4 common-fixture lines + 49 helpers + 221 tests + 3 net import lines). This
is 177 lines over the nominal 100-line test target; the shared
same-boundary three-case proof requires the real SQLite fixture, two injected
failure cases, durable-state assertions, telemetry assertions, and explicit
no-spawn assertions. Reusing `FakeStateStore` removes the former 137-line
delegate; no artificial split is introduced. The three and only three new test
names are:

1. `register_branch_non_transient_collision_emits_parked_human_held_after_durable_save`
2. `register_branch_transient_failure_remains_retryable_without_parking`
3. `register_branch_park_save_failure_suppresses_park_telemetry`

## Focused execution and expected RED/GREEN

Run from the repository root after adding the tests. This focused command uses
the existing fakes and the in-memory SQLite store; it does not make live LLM or
service calls:

```bash
cargo test --manifest-path daemon/Cargo.toml --test tick_integration register_branch -- --nocapture
```

Expected RED before `daemon/src/tick.rs` is changed:

- The command exits non-zero because the first test cannot find
  `PARKED_HUMAN_HELD` and its failure includes
  `C4_EXPECT_DURABLE_PARK_TELEMETRY`.
- `register_branch_transient_failure_remains_retryable_without_parking` is
  green.
- `register_branch_park_save_failure_suppresses_park_telemetry` is green.
- A compile failure, a zero-test run, or a failure in either negative-space
  test is not the intended RED result; stop and report the exact output to the
  parent rather than repairing or redesigning the fixture.

Expected GREEN after the one production insertion:

```bash
cargo test --manifest-path daemon/Cargo.toml --test tick_integration register_branch -- --nocapture
```

All three named tests pass. The final real-canary/E2E validation remains a
separate parent-owned gate; this focused test command does not replace it.

## Executor stop rules and residual guesses

- Stop and report to the parent if the exact dispatch-failure anchor is absent
  or if `BranchRegistrationConflict::value()` no longer returns
  `branch_registration_conflict`; do not rederive an insertion or change
  dispatch/schema behavior.
- Stop and report to the parent if
  `SqliteStateStore::open_in_memory_with_schema`, the verified import, or the
  `branch_registry` schema anchor is absent; do not invent a constructor or
  substitute a different store.
- Stop and report to the parent if the verified `DaemonError::is_transient`
  classification changes; do not invent alternate error constructors or
  reinterpret the test lanes.
- Stop and report to the parent if the verified `StateStore` required-method
  signatures differ; do not invent delegate methods, add shared-fixture
  changes, or silently omit a method.
- Do not add a defensive durable reread or a backend fallback in the tick
  branch. The dispatch report contract already separates successful permanent
  parks from transient park-save failures; the missing behavior is only the
  tick-side telemetry classification.
