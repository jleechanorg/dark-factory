# /af First-Ready Strategy Design Specification

**Document**: `/Users/jleechan/projects/worktree_factory_review/docs/superpowers/specs/2026-09-13-af-first-ready-strategy-design.md`  
**Target Repository**: `jleechanorg/dark-factory`  
**Dated baselines (resolve exact head at execution)**:
- Installed release baseline on Linux: [`8316b204b9871c70b8e9ecbc9647b8d9579c6d62`](https://github.com/jleechanorg/dark-factory/commit/8316b204b9871c70b8e9ecbc9647b8d9579c6d62), prod +654/-21, non-prod +0/-0 (`ai.dark-factory.daemon.service`, PID 3091590)
- Upstream `main` baseline: [`85617d25597783b898154d8c10bd01ae63f4e011`](https://github.com/jleechanorg/dark-factory/commit/85617d25597783b898154d8c10bd01ae63f4e011), prod +0/-0, non-prod +28/-13
- These are dated baselines. At execution, resolve and pin the exact live release and target head before intake.

---

## 1. Executive Summary & Verified Facts
The Dark Factory transitions tasks across canonical lifecycle states (`QUEUED -> DISPATCHING -> DISPATCHED -> ATTESTED -> READY`). Routing verdicts (such as `StandardPath` or `AdoptExistingPr`) select execution logic but are distinct from database lifecycle states. The observed baseline was 748 `HUMAN_HELD` (745 operator holds under `operator_scope_hold_20260907_three_prs`, 3 historical pilots: `uee0`, `2dus`, `72hp`), 17 `READY`, and 0 `QUEUED`; execution must account for ambient activity.
1. **Preserve All 745 Held Identities**: In `daemon/src/state.rs:2310`, `recover_human_held` enforces an immutable 7-reason allowlist (`transient_spawn_retry_cap_exceeded`, `transient_processing_retry_cap_exceeded`, `adopted_pre_session_sha_capture_failed`, `session_stalled`, `stage1_gate_not_green`, `spec_validation_failed`, `target_checkout_unconfigured`) plus router prefixes. Current eligible rows equal 0. Running global recovery touches 0 rows; all 745 operator-held identities must remain strictly preserved without bulk recovery.
2. **Historical Pilots Closed**: Closed historical pilots must not be revived. Existing adoption-collision guards remain scoped to the affected identity.
3. **Telemetry Fidelity Confirmed**: The original full binary log (`daemon.jsonl`) preserves full error context (`coder_silent` at exact park timestamp, spawn failure strings, branch collisions). Its first NUL is at byte 215238827, a possible mechanism for grep false negatives; the method used for any earlier search is unknown.
4. **Remediation vs. Canary Separation**: Factory prerequisites (AO Go dispatch alignment and launch-edge account scope validation) must be remediated and verified independently before the target canary is dispatched.

---

## 2. Exploration of Architectural Approaches
- **Approach 1: Revive historical pilot work**: Conflates pipeline verification with stale branch and merge work.
- **Approach 2: Heavy Telemetry Outbox & Schema Overhaul**: Blocks dispatch on an unneeded database outbox rewrite. Error strings already persist to disk. Violates minimal blast radius.
- **Approach 3: Narrow Permanent-Park Canary (`c4zhq`) Following Prerequisite Validation (SELECTED)**: Independently verifies AO Go dispatch and launch-edge account scoping, then runs existing open bead `dark-factory-c4zhq` to fix a narrow branch-registration park defect in `dispatch.rs`/`tick.rs` on an isolated branch.

---

## 3. Assumptions and Recommended Defaults
| # | Question Considered | Options Considered | Auto-Picked Choice | Rationale & Empirical Evidence |
|---|---|---|---|---|
| 1 | **Canary Selection** | A. Revive historical pilot work<br>B. Duplicate documentation routing<br>C. Existing `c4zhq` narrow park defect | **Option C: `c4zhq` narrow defect** | Real, bounded bug in the installed baseline and main baseline. Avoids stale branch work. |
| 2 | **Account Scope Proof** | A. Assume env pinning proves scope<br>B. Validate launch edges directly | **Option B: Validate launch edges** | `ChainLlm::judge` tries Codex first; `run_minimax_judge` inherits Claude config without model pin. Validate edges directly. |
| 3 | **AO Engine Contract** | A. Edit AO TS source<br>B. Approve TS bridge<br>C. Repair factory Go adapter | **Option C: Factory Go adapter repair** | Dual runtimes: systemd runs `ao-go` (PID 2049478); user wrapper calls TS CLI (PID 3274). Align adapter to Go policy without editing AO code. |
| 4 | **Operator Holds** | A. Bulk recover<br>B. Leave untouched | **Option B: Preserve all 745 holds** | Recovery allowlist matches 0 rows. Preserving all 745 identities is mandatory; no unscoped recovery. |
| 5 | **Verifier Contract** | A. 7 gates<br>B. All 8 canonical gates | **Option B: All 8 canonical gates** | `daemon/src/verifier.rs` defines exactly 8 gates, including Gate 8 (`vacuous_red_green`). |

---

## 4. Factory Prerequisite Remediation (Launch Edges & AO Go Dispatch)
1. **AO Go Dispatch Alignment**: First inspect the supported Go factory adapter and its configuration against the running service. If it is insufficient, the bounded adapter implementation must be owned by the factory through authorized `/af`; it may use factory source or supported configuration. Do not edit the AO repository or user wrappers. If the factory cannot bootstrap the supported adapter, escalate the exact bootstrap blocker and do not hand-fix it.
2. **Launch-Edge Scope Validation**: Account repair uses the existing `i92jy` factory path. Setting `CODEX_HOME` or pinning MiniMax alone does not prove real child account scope. Before every AI process is created, validate its intended account/provider scope and construct its scoped child environment; fail closed before launch if invalid, and permit fallback only to another already-validated scoped lane. Runtime child inspection corroborates this pre-launch validation and cannot substitute for it. If no compliant lane can bootstrap repairs, report the exact bootstrap blocker without launching an unscoped worker. Record child/process names and boolean validation results only; never record credential values. The dated `validate_ao_worker_agent_scope` finding covered a definition without caller evidence; require live repair and child-scope proof.

---

## 5. Target Canary Specification (`dark-factory-c4zhq`)
1. **Narrow Defect**: In the installed baseline and main baseline, `daemon/src/dispatch.rs` parks a bead `HUMAN_HELD` with reason `BranchRegistrationConflict` upon non-transient registration failure. However, `daemon/src/tick.rs` lacks a handler for `phase == "register_branch"`, falling through to emit `BEAD_DISPATCH_TRANSIENT_ERROR` with `lifecycleState: DISPATCHING`.
2. **Truthful State & Telemetry Contract**:
   - Deliver truthful event/state/reason: emit `PARKED_HUMAN_HELD` with `reason: branch_registration_conflict` and `OverlayState::HumanHeld` ONLY after durable save succeeds. Preserve real errors via existing telemetry redaction.
   - Do not alter recovery/ownership policies, add an outbox, or rename historical schemas.
3. **Worker-only Behavioral Test Cases** (`daemon/tests/tick_integration.rs`): After intake and dispatch, the Linux factory worker owns the allocated worktree and branch and writes all tests and code. Production edits are owned by `daemon/src/dispatch.rs` and `daemon/src/tick.rs`; all three newly named end-to-end tests belong in `daemon/tests/tick_integration.rs`. There is no provisioned Mac canary worktree, hand coding, or preimplemented canary. The worker must add:
   - `register_branch_non_transient_collision_emits_parked_human_held_after_durable_save`: exercise a real SQLite collision, durably persist `HUMAN_HELD`, then verify correct `PARKED_HUMAN_HELD` state/reason and no worker spawn.
   - `register_branch_transient_failure_remains_retryable_without_parking`: verify transient registration failure is retryable, leaves no persisted hold, and emits no park telemetry.
   - `register_branch_park_save_failure_suppresses_park_telemetry`: verify failed durable park save is retryable and emits no false park telemetry.
   The worker must first read and retain the existing `daemon/src/dispatch.rs` unit tests `register_branch_conflict_on_one_bead_parks_only_that_item_without_refusing_unrelated_work` and `register_branch_conflict_does_not_persist_rejected_branch_or_mislabel_reason`, then run each selected target with a nonzero selected-test count and output naming each new test.

---

## 6. All Eight Verifier Gates (`daemon/src/verifier.rs`)
To reach `READY`, the canary review artifact must autonomously pass all eight gates evaluated at exact HEAD, with runtime evidence for the resulting state and review record:
1. `ci_green` (`Ci`): Test suite and compilation clean.
2. `no_conflicts` (`NoConflicts`): Clean mergeability into `main`.
3. `coderabbit` (`CodeRabbitApproved`): Automated review pass.
4. `bugbot` (`BugbotClean`): Static defect inspection clean.
5. `comments_resolved` (`CommentsResolved`): All review conversations resolved.
6. `evidence_review` (`EvidenceFloor`): Required cryptographic receipts intact.
7. `skeptic` (`Skeptic`): Adversarial behavioral validation passed.
8. `vacuous_red_green` (`VacuousRedGreen`): Verifies test fails prior to fix and passes afterward, rejecting tautological tests.

---

## 7. Implementation Preconditions & Fail-Closed Rules
1. **Preconditions**: Explicit operator authorization; zero hand execution of coding; zero AO source or user wrapper edits; zero global daemon replacement; all 745 held identities preserved; the Linux factory allocates the worker worktree and branch after dispatch; dated baselines are replaced by freshly resolved exact release/head; and store health is checked with `ssh jeff-ubuntu 'br --db /home/jleechan/.local/state/dark-factory/.beads/beads.db --no-auto-flush --no-auto-import doctor --quick'` followed separately by `ssh jeff-ubuntu 'br --db /home/jleechan/.local/state/dark-factory/.beads/beads.db --no-auto-flush --no-auto-import sync --status --json'`.
2. **Fail-Closed Stop Rules**: Stop the affected canary if its identity or owner invariants fail, if the canary records `adoption_branch_collision`, or if a child environment leaks operator personal profiles. Continue read-only diagnosis for unrelated ambient activity; an unrelated historical adoption collision is not a global stop.
