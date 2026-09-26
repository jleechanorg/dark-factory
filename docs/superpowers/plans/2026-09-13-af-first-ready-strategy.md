# /af First-Ready Strategy Implementation Plan

**Document**: `/Users/jleechan/projects/worktree_factory_review/docs/superpowers/plans/2026-09-13-af-first-ready-strategy.md`  
**Acceptance target**: Establish the autonomous `/af` lifecycle (`QUEUED -> DISPATCHING -> DISPATCHED -> ATTESTED -> READY`) for target canary `dark-factory-c4zhq` after prerequisite remediation, with runtime evidence for all eight gates while preserving all 745 operator-held identities and avoiding merge conflicts.  
**Dated baselines**: Installed release [`8316b204b9871c70b8e9ecbc9647b8d9579c6d62`](https://github.com/jleechanorg/dark-factory/commit/8316b204b9871c70b8e9ecbc9647b8d9579c6d62), prod +654/-21, non-prod +0/-0 (`ai.dark-factory.daemon.service`, PID 3091590), and upstream `main` [`85617d25597783b898154d8c10bd01ae63f4e011`](https://github.com/jleechanorg/dark-factory/commit/85617d25597783b898154d8c10bd01ae63f4e011), prod +0/-0, non-prod +28/-13. At execution, resolve and pin the exact live release and target head before intake.

---

## Assumptions & Preconditions
1. **Explicit Implementation Authorization**: Execution proceeds only after explicit human authorization.
2. **Autonomous Execution**: Zero hand execution of coding; all implementation is autonomous worker driven.
3. **Engine & Repository Invariants**: Zero edits to AO repository (`project_agento/agent-orchestrator-ts`) or user wrappers; zero global daemon replacement; zero bulk recovery; all 745 `operator_scope_hold_20260907_three_prs` identities remain untouched in `HUMAN_HELD`.
4. **Store Integrity**: Run `ssh jeff-ubuntu 'br --db /home/jleechan/.local/state/dark-factory/.beads/beads.db --no-auto-flush --no-auto-import doctor --quick'`, then separately run `ssh jeff-ubuntu 'br --db /home/jleechan/.local/state/dark-factory/.beads/beads.db --no-auto-flush --no-auto-import sync --status --json'`. Interpret each result; do not invoke an automatic fixer.

---

## Phase 1: Factory Prerequisites Remediation & Validation (Pre-Canary)

### Step 1.1: Verify Live Queue Baseline and Operator Hold Census
- Query `bead_overlay` in `/home/jleechan/.dark-factory/daemon-cxdb.sqlite` via read-only SQLite check.
- Confirm baseline: 748 `HUMAN_HELD` (exactly 745 `operator_scope_hold_20260907_three_prs`), 17 `READY`, 0 `QUEUED`; account for ambient activity while preserving the unchanged 745-identity set.
- Verify `recover_human_held` eligibility matches 0 rows; confirm no bulk recovery is run.

### Step 1.2: Inspect and, if authorized, Repair the Supported Go Dispatch Adapter
- Verify the running state of `ai.dark-factory.ao.service` and inspect the supported Go adapter/configuration used by the factory.
- If insufficient, route a bounded factory-owned adapter implementation through authorized `/af`, using factory source or supported configuration; do not modify AO code or user wrappers.
- If the factory cannot bootstrap the adapter, escalate the exact bootstrap blocker and stop this canary without hand-fixing it.

### Step 1.3: Validate AI Provider Launch Edges and Child Environments
- Use the existing `i92jy` factory path for account repair, then inspect router, coder, reviewer, and fallback edges in `daemon/src/adapters.rs`.
- Before every AI process is created, validate its intended account/provider scope and construct its scoped child environment; fail closed before launch if invalid, and permit fallback only to another already-validated scoped lane. Runtime child inspection corroborates this pre-launch validation and cannot substitute for it. If no compliant lane can bootstrap repairs, report the exact bootstrap blocker without launching an unscoped worker. Record child/process names and boolean validation results only; never record credential values. Configuration variables alone do not prove this.
- The dated `validate_ao_worker_agent_scope` finding covered a definition without caller evidence; require live repair and child-scope proof before dispatch.

---

## Phase 2: Target Canary Intake (`dark-factory-c4zhq`)

### Step 2.1: Verify and Intake the Existing Canary
- Verify the existing open, unlabelled bead `dark-factory-c4zhq` and zero existing branch, session, or overlay owner collisions.
- Ingest it through exact-store two-phase intake and verify transition to `QUEUED`; do not provision a Mac worktree or preimplement tests/code.

---

## Phase 3: Autonomous Dispatch and Worker Execution

### Step 3.1: Observe Autonomous Dispatch and Telemetry Event
- Observe the fast-tier daemon tick for the queued canary bead.
- Acceptance evidence must show `TASK_DISPATCHED` (the actual success event) and the overlay transition to `DISPATCHED`.
- Record runtime evidence for the actual supported factory adapter and active child session; service presence alone is insufficient.

### Step 3.2: Autonomous Worker Code Execution and Review Artifact Creation
- After dispatch, the Linux factory allocates the worker worktree and branch. The worker alone writes the bounded tests and source fix, runs RED then GREEN, and publishes its branch; no manual Mac canary or preimplemented change is allowed.
- Production edits belong in `daemon/src/dispatch.rs` and `daemon/src/tick.rs`; all three newly named end-to-end tests belong in `daemon/tests/tick_integration.rs`. Add:
  - `register_branch_non_transient_collision_emits_parked_human_held_after_durable_save`: exercise a real SQLite collision, durably persist `HUMAN_HELD`, then verify correct `PARKED_HUMAN_HELD` state/reason and no worker spawn.
  - `register_branch_transient_failure_remains_retryable_without_parking`: verify transient registration failure is retryable, leaves no persisted hold, and emits no park telemetry.
  - `register_branch_park_save_failure_suppresses_park_telemetry`: verify failed durable park save is retryable and emits no false park telemetry.
  First read and retain the existing `daemon/src/dispatch.rs` unit tests `register_branch_conflict_on_one_bead_parks_only_that_item_without_refusing_unrelated_work` and `register_branch_conflict_does_not_persist_rejected_branch_or_mislabel_reason`.
- Run these separate commands from the worker target repository, with a nonzero selected-test count and output naming each explicit new test:
  - `cargo test --manifest-path daemon/Cargo.toml --lib register_branch`
  - `cargo test --manifest-path daemon/Cargo.toml --test tick_integration register_branch`
- The source fix must save `OverlayState::HumanHeld` durably before truthful park telemetry and keep transient failures retryable without false park emission.
- Verify zero hand execution of coding was performed and retain runtime evidence for the resulting review artifact.

---

## Phase 4: Autonomous Verifier Evaluation Across All Eight Gates

### Step 4.1: Fast-Tier Verifier Assessment at Exact HEAD
- Run the verifier in `daemon/src/verifier.rs` and retain exact-head evidence for all eight canonical gates:
  1. `ci_green` (`Ci`): Clean compilation and unit tests.
  2. `no_conflicts` (`NoConflicts`): Mergeable into `main`.
  3. `coderabbit` (`CodeRabbitApproved`): Automated reviewer pass.
  4. `bugbot` (`BugbotClean`): Static defect inspection clean.
  5. `comments_resolved` (`CommentsResolved`): Bot threads resolved.
  6. `evidence_review` (`EvidenceFloor`): Receipts complete.
  7. `skeptic` (`Skeptic`): Adversarial behavioral check green.
  8. `vacuous_red_green` (`VacuousRedGreen`): Non-tautological test verification.

### Step 4.2: Terminal Overlay Transition to READY
- Acceptance criteria: runtime evidence shows the canary overlay transition from `ATTESTED` to `READY` after 8/8 green assessment, and the readiness record is present on the resulting review artifact. Do not infer either outcome from configuration or a worker-produced event alone.

---

## Phase 5: Post-Execution Audit, Evidence Manifest & Merge Guard

### Step 5.1: Verify State Store Invariants
- Query SQLite `bead_overlay` and confirm the canary identity reaches `READY`, the unchanged 745 `operator_scope_hold_20260907_three_prs` identities remain in `HUMAN_HELD`, and ambient activity is accounted for. Do not require a global queue total of exactly 18 `READY`.
- If the canary violates its identity, owner, or telemetry invariant, stop only the affected canary and continue read-only diagnosis; an unrelated historical adoption collision is not a global stop.

### Step 5.2: Cryptographic Receipts and SHA-256 Manifest
- Export telemetry receipts for `dark-factory-c4zhq` from `daemon.jsonl`.
- Generate SHA-256 manifest of evidence files and link in review documentation.

### Step 5.3: Enforce Merge Approval Invariant
- Verify the resulting review artifact remains Draft / unmerged. Merging is strictly forbidden absent explicit literal `MERGE APPROVED`.
