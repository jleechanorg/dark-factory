# Explore: authorities for stale PR seed envelopes

Goal: **[P0][factory][RED] Reject stale PR seed envelopes before AO spawn (`jleechanorg/worldarchitect.ai`)**.

Scope note: this maps the visible code only. No sealed holdouts or evaluator internals were inspected.

## Current authorities

| Concept | Current authoritative component | Projections / consumers |
|---|---|---|
| Live GitHub PR identity | `CliScm` is the external-read authority. Bulk intake obtains `number`, `headRefName`, `headRefOid`, repository provenance, and `updatedAt` from GitHub in `daemon/src/adapters.rs:1832`; the REST fallback extracts the same fields in `daemon/src/adapters.rs:1141` and `daemon/src/adapters.rs:1186`. | `LabeledPr` is an in-memory snapshot (`daemon/src/tools.rs:111`), not durable truth. `ExistingPrIntake` carries `(repo, pr_number, head_ref_name, head_sha)` only within one slow-tier pass (`daemon/src/intake.rs:404`). |
| Live “PR is open and same-repo” decision | `Scm::open_pr_head_ref_for_repo(repo, pr)` is the dispatch-time authority (`daemon/src/tools.rs:611`); `CliScm` executes the GitHub REST lookup (`daemon/src/adapters.rs:2310`) and `parse_open_pr_head_ref` enforces open state plus same-repo head ownership (`daemon/src/adapters.rs:2713`). | Its return type `PrHeadBranch` carries only the branch or fork/not-found classification (`daemon/src/tools.rs:694`); it drops the live head SHA. `DriveBranchDecision` is a branch-only projection (`daemon/src/dispatch.rs:77`). |
| Branch selected for a normal Rust dispatch | `tick::resolve_drive_pr_head_branch` makes the last SCM-backed decision before the ready queue (`daemon/src/tick.rs:6414`), and `dispatch_ready_with_vcs` consumes that decision without re-deriving it (`daemon/src/dispatch.rs:194`). | `overlay.branch`, the branch registry, the prompt preamble, and `SpawnSpec.branch` mirror the selected value (`daemon/src/dispatch.rs:445`, `daemon/src/dispatch.rs:496`, `daemon/src/dispatch.rs:550`, `daemon/src/dispatch.rs:625`, `daemon/src/dispatch.rs:648`). |
| Checkout revision for a normal Rust spawn | `Vcs::base_head_for_repo(repo, base_branch)` currently wins in `dispatch_ready_with_vcs` (`daemon/src/dispatch.rs:593`). The resulting SHA is put in `SpawnSpec.expected_revision` (`daemon/src/dispatch.rs:648`) and enforced before AO by `ensure_*_target_worktree` (`daemon/src/adapters.rs:2960`). | This is the **base branch SHA**, not the adopted PR head SHA. For a PR-head dispatch, the branch and revision therefore come from different authorities. `overlay.pre_session_head_sha` is also assigned this base SHA at `daemon/src/dispatch.rs:623`.
| Mutable daemon lifecycle | `bead_overlay` in `~/.dark-factory/daemon-cxdb.sqlite` is the durable Rust lifecycle authority. The state vocabulary is `OverlayState` (`daemon/src/state.rs:9`), and `SqliteStateStore::save` upserts the complete overlay (`daemon/src/state.rs:1609`). | Telemetry JSONL is an audit projection. `BeadOverlay` fields are process-local copies until `StateStore::save` succeeds. The schema is shared by Rust and shell (`daemon/contracts/schema.sql:1`). |
| Bead identity and goal text | The `br` tracker is authoritative for bead id, title, description/notes, status, labels, and `external_ref`; `CliTracker::fetch_candidates` materializes them as `Bead` (`daemon/src/adapters.rs:106`). | The overlay intentionally stores lifecycle/routing fields, not the goal body. Worker prompts re-read the tracker (`daemon/src/dispatch.rs:1270`; legacy shell at `daemon/factory-ao-remediate.sh:66`). |
| Repo identity | `intake::resolve_target_repo` is the canonical parser and precedence rule: body `target_repo:` first, then `external_ref`, else none (`daemon/src/intake.rs:459`). For an already-persisted bead, `BeadOverlay::repo` is the accessor (`daemon/src/state.rs:178`). `Config::resolve_repo` owns repo-to-AO-project/remote routing (`daemon/src/dispatch.rs:350`). | `BeadOverlay.target_repo` is the durable projection. `NULL -> cfg.target_repo` is a legacy fallback, although dispatch now attempts recovery from the current bead before parking (`daemon/src/dispatch.rs:265`). |
| PR number for a branch after spawn | GitHub branch-to-open-PR lookup is authoritative through `Scm::pr_number_for_branch` (`daemon/src/tools.rs:641`) and `CliScm` (`daemon/src/adapters.rs:2325`). The slow tier overwrites stale overlay values for `DISPATCHED` beads (`daemon/src/tick.rs:4458`), and optional pre-gate validation repeats the consistency check for `ATTESTED` beads (`daemon/src/tick.rs:4779`). | `BeadOverlay.pr_number` is explicitly a cached projection and can be cleared or rewritten (`daemon/src/tick.rs:4484`, `daemon/src/tick.rs:4831`). Adopted beads are exempt from one stale-clear arm (`daemon/src/tick.rs:4505`). |
| Branch ownership | `branch_registry` in the state DB is authoritative for daemon ownership. Rust calls `StateStore::register_branch` before saving `DISPATCHING` (`daemon/src/dispatch.rs:496`). | `BeadOverlay.branch` is a lifecycle mirror, not proof of ownership. The shell harness has a parallel branch-registry transaction in `dispatch-record` (`daemon/factory-overlay.sh:167`). |
| AO session identity and actual workspace | AO's spawn response/live session is authoritative once a process exists. `CliSessions::run_spawn_process` parses `SESSION`, `Worktree`, and `Branch`, rejects mismatch, and records in-memory workspace maps (`daemon/src/adapters.rs:3290`). Dispatch then rechecks live branch and remote before persisting `DISPATCHED` (`daemon/src/dispatch.rs:809`, `daemon/src/dispatch.rs:851`). | `BeadOverlay.session_id` becomes durable only after the successful post-spawn save (`daemon/src/dispatch.rs:947`). The `CliSessions` workspace maps are process caches and disappear on restart (`daemon/src/adapters.rs:3266`). |
| PR adoption authorization cache | GitHub remains authoritative; `AdoptionProbeCache` only memoizes permission decisions under `(external_ref, head_sha, updated_at_epoch)` (`daemon/src/intake.rs:138`) and refuses incomplete keys (`daemon/src/intake.rs:158`). | The disk file `~/.dark-factory/adoption_probe_cache.json` is a TTL cache, not PR state (`daemon/src/intake.rs:214`, `daemon/src/intake.rs:226`). |

### Legacy projection fields

- `existing_pr:`, `existing_branch:`, and `target_repo:` in bead bodies are compatibility protocol fields produced by the shell intake normalizer (`daemon/factory-intake-from-gh.sh:186`). Rust reads only `target_repo:` as routing input; it does not treat `existing_pr:` or `existing_branch:` as live PR authority. The worker sees them because the body is copied into its prompt. **ponytail:** keep these fields as read-only compatibility mirrors; a single live GitHub envelope must win.
- `BeadOverlay.pr_number` and `BeadOverlay.branch` are durable cached projections of GitHub/AO state (`daemon/src/state.rs:95`). Existing code already repairs `pr_number` from branch after dispatch, proving the stored number is not authoritative (`daemon/src/tick.rs:4460`). **ponytail:** retain the columns for recovery and observability, but do not admit a spawn solely from them.
- `BeadOverlay.target_repo = NULL` means the global configured repository for pre-migration rows (`daemon/src/state.rs:147`). Dispatch's recovery from the current bead is the compatibility path (`daemon/src/dispatch.rs:281`). **ponytail:** this legacy projection ceiling is intentional; new dispatches should persist an explicit repo.
- `CliSessions.project` is a construction-time legacy default (`daemon/src/adapters.rs:3276`); per-spawn `SpawnSpec.ao_project` is the routed value used by `run_spawn_process` (`daemon/src/adapters.rs:3290`).
- Telemetry `EXISTING_PR_ADOPTED` includes `head_sha` (`daemon/src/tick.rs:1841`), but telemetry is append-only evidence, not a state source. No reader feeds that SHA back into dispatch.

## Conflicting authorities

### 1. Live PR envelope versus body/SQLite seed tuple

There are three writers for the same conceptual tuple:

1. GitHub supplies live `(repo, PR number, branch, head SHA, updated-at)` through `CliScm`.
2. Shell intake writes body projections `(existing_pr, existing_branch, target_repo)` (`daemon/factory-intake-from-gh.sh:186`).
3. Rust intake and operator commands write `(target_repo, pr_number, branch, is_adopted)` into `bead_overlay` (`daemon/src/tick.rs:1794`; `daemon/factory-overlay.sh:524`).

Today the live intake SHA is lost at the persistence boundary: `ExistingPrIntake.head_sha` is emitted to telemetry but `BeadOverlay` has no PR-seed SHA field (`daemon/src/tick.rs:1776`). A later dispatch rechecks open/same-repo status but receives only a branch (`daemon/src/tick.rs:6460`). The shell path does not perform that recheck at all.

**Single authority that should win:** a fresh GitHub PR envelope immediately before AO spawn. Body fields and overlay columns are read-only mirrors. **ponytail:** one shared admission decision is the ceiling; accepting several writers as co-authoritative would preserve the stale-seed bug.

### 2. PR branch authority versus checkout revision authority

For `DriveBranchDecision::PrHead`, the branch comes from a live PR lookup (`daemon/src/tick.rs:6460`), while `expected_revision` comes from the configured base branch (`daemon/src/dispatch.rs:593`). The adapter strongly validates the checkout against that base SHA (`daemon/src/adapters.rs:2978`, `daemon/src/adapters.rs:3333`), so the guard can be perfectly green while the PR envelope changed between resolution and spawn.

**Single authority that should win for adopted/drive-PR spawn identity:** the live PR head SHA paired with its branch. The configured base head remains authoritative only for generated-new-work branches.

### 3. `pre_session_head_sha` has two meanings

The field is documented as the adopted branch's pre-remediation remote HEAD (`daemon/src/state.rs:120`) and `reroll::execute_adopted` writes exactly that (`daemon/src/reroll.rs:1341`, `daemon/src/reroll.rs:1416`). Normal dispatch instead writes `Vcs::base_head_for_repo` for every bead (`daemon/src/dispatch.rs:593`, `daemon/src/dispatch.rs:623`). Readers performing append-only/force-push checks cannot infer which semantic produced the value from the column alone.

**Single authority that should win:** `pre_session_head_sha` should mean the actual dispatched branch's pre-session remote SHA, as its schema/docs say. **ponytail:** do not add another alias with overlapping meaning; either populate the existing field consistently or keep base revision in the spawn-only contract.

### 4. Rust lifecycle writer versus shell lifecycle writer

`SqliteStateStore::save` and `factory-overlay.sh` both write the same `bead_overlay` table (`daemon/src/state.rs:1609`; `daemon/factory-overlay.sh:8`). The shell file calls itself the executable spec that Rust replaces (`daemon/factory-overlay.sh:10`), yet the scheduled shell dispatcher remains wired by launchd (`daemon/launchd/ai.dark-factory.af-tick.plist.template:42`). Their transition semantics differ: Rust persists `DISPATCHING` before spawn and validates after spawn; shell spawns first and records `DISPATCHED` afterward (`daemon/factory-af-tick.sh:354`).

**Single authority that should win:** the Rust `StateStore` plus `dispatch_ready_with_vcs` state machine. **ponytail:** keep the shell harness only as a compatibility/operator projection until its caller is retired; it must not define a second admission policy.

### 5. Async spawn state file versus SQLite session state

The shell async path writes `pending|ok|fail:rc=N` to a per-bead/PR file (`daemon/factory-ao-remediate.sh:205`) while the caller independently writes `DISPATCHED` to SQLite (`daemon/factory-af-tick.sh:357`). A spawn still `pending` is optimistically accepted (`daemon/factory-ao-remediate.sh:251`); the next tick later rolls a `fail:*` file back to `QUEUED` (`daemon/factory-overlay.sh:500`). Either file can lag or outlive the other.

**Single authority that should win:** AO session creation reconciled transactionally into the SQLite overlay. The state file is a temporary delivery receipt only. **ponytail:** the async branch is retained for tick backpressure, but its file is not durable lifecycle truth.

### 6. Repo identity precedence can contradict PR identity

An explicit body `target_repo:` wins over `external_ref` in `resolve_target_repo` (`daemon/src/intake.rs:459`). `resolve_drive_pr_head_branch` detects disagreement and silently falls back to a generated branch (`daemon/src/tick.rs:6451`), while `BeadOverlay.repo` can further project `NULL` to the global repo (`daemon/src/state.rs:178`).

**Single authority that should win for a PR seed:** the repository inside the validated live PR envelope. Body and overlay routing may select which repo to query, but they cannot override the identity returned for the PR.

## Implicit state machines

### PR intake/adoption

| State key | Lifecycle | Persistence | Readers |
|---|---|---|---|
| `LabeledPr { external_ref, number, head_ref_name, head_sha, updated_at_epoch, repo provenance }` | GitHub list/REST -> permission/fork/dedup checks -> `ExistingPrIntake` or skip outcome (`daemon/src/intake.rs:939`). | In memory only; permission decision may be cached by the complete key. | `normalize_labeled_prs_with_cache`; telemetry. |
| `ExistingPrIntake` | Adopted result -> branch collision check -> branch registration -> overlay update (`daemon/src/tick.rs:1716`). | Only `repo`, `pr_number`, `branch`, and `is_adopted` survive in SQLite; `head_sha` survives only in telemetry. | `run_slow_tier` adoption loop. |
| tracker `external_ref` | GitHub PR/issue -> canonical `owner/repo#N` -> unique bead association (`daemon/src/intake.rs:497`). | `br` database. | Intake dedup, repo resolution, dispatch-time PR-number parse, escalation comments. |

### Rust dispatch reservation

`QUEUED|REDISPATCHED -> route -> DriveBranchDecision -> branch_registry -> DISPATCHING -> expected_revision save -> Sessions::spawn -> live branch/worktree/remote checks -> DISPATCHED` (`daemon/src/dispatch.rs:162`).

- Durable keys: `state`, `attempt`, `target_repo`, `pr_number`, `branch`, `is_adopted`, `pre_session_head_sha`, `session_id`, `spawn_failure_count` in `bead_overlay` (`daemon/contracts/schema.sql:10`).
- Volatile keys: routing verdict, `DriveBranchDecision`, `SpawnSpec`, current batch capacity, and `CliSessions` workspace maps.
- Failure lifecycle: deferred/transient spawn returns to `QUEUED`; permanent validation failures park `HUMAN_HELD`; a post-spawn save failure kills the new session before requeue (`daemon/src/dispatch.rs:668`, `daemon/src/dispatch.rs:971`).
- Persistence boundary: `DriveBranchDecision` is computed before the branch registry/`DISPATCHING` writes and is not persisted as a full PR envelope. A PR update in this interval is invisible to dispatch admission.

### Post-spawn PR reconciliation

`DISPATCHED + branch -> pr_number_for_branch -> set/replace/clear pr_number -> ATTESTED -> optional pre-gate open/head-branch validation -> gate snapshot` (`daemon/src/tick.rs:4449`, `daemon/src/tick.rs:4779`).

- GitHub branch/PR state is authoritative; the overlay is repaired afterward.
- This machine detects stale identity **after** a worker may already exist. It is not a pre-spawn stale-envelope guard.
- Adopted rows preserve a stale `pr_number` in the `Ok(None)` slow-tier arm (`daemon/src/tick.rs:4505`), based on provenance rather than a fresh envelope.

### Adopted reroll/remediation

`ATTESTED/RE_ROLL adopted bead -> ensure no active session -> read remote branch SHA -> save pre-session SHA -> Sessions::spawn -> DISPATCHED` (`daemon/src/reroll.rs:1223`, `daemon/src/reroll.rs:1341`, `daemon/src/reroll.rs:1416`).

- `reroll::execute_adopted` owns this alternate transition and calls `Sessions::spawn` directly (`daemon/src/reroll.rs:1419`).
- It has a stronger branch-SHA capture than normal dispatch, but it still starts from persisted `branch`/`pr_number` and does not validate one atomic live `(repo, PR, branch, SHA)` envelope.

### Legacy shell async remediation

`QUEUED|ATTESTED SQLite row -> shell SELECT -> AO duplicate probe -> background spawn -> state file pending/ok/fail -> optimistic dispatch-record -> later rollback on fail` (`daemon/factory-af-tick.sh:291`, `daemon/factory-ao-remediate.sh:197`, `daemon/factory-overlay.sh:492`).

- Persistence is split across SQLite, an async state file, AO's own session store, and a spawn log.
- There is no session id stored in the overlay on success.
- The selection tuple is read once from SQLite at `daemon/factory-af-tick.sh:411`; neither branch nor head SHA is refreshed before `ao spawn --claim-pr`.

### Adoption probe cache

`missing/incomplete/stale key -> GitHub permission probe -> cache entry -> TTL hit -> invalidate on SHA/updated-at/external-ref change` (`daemon/src/intake.rs:138`, `daemon/src/intake.rs:1019`). Cache persistence is atomic and disk-backed under the runtime state directory, but a corrupt/missing file deliberately becomes a cold cache (`daemon/src/intake.rs:239`).

## Streaming / non-streaming branches

### Rust AO path: non-streaming

- `CliSessions::run_spawn_process` uses `Command::output`, collecting all stdout/stderr before classifying the result (`daemon/src/adapters.rs:3302`). Both `dispatch_ready_with_vcs` (`daemon/src/dispatch.rs:668`) and adopted reroll (`daemon/src/reroll.rs:1419`) converge on this non-streaming `Sessions::spawn` implementation.
- `Sessions::spawn_batch` is also deliberately serial and reuses the single-spawn path (`daemon/src/adapters.rs:6323`). **ponytail:** serial batch execution is an intentional ceiling because AO's spawn lock is per project; keep it for lock correctness, not as a separate authority.
- There is no Rust streaming branch that changes PR-envelope ownership. A guard added only to stdout classification would be too late: AO has already spawned.

### Shell remediation: sync versus async/non-blocking

- Sync callers (`SYNC=1` or `ASYNC=0`) block in `run_spawn_foreground`, classify the complete output, and return (`daemon/factory-ao-remediate.sh:59`, `daemon/factory-ao-remediate.sh:169`).
- Default async callers run the same foreground function in a detached subshell, stream no output to the caller, write the full captured output to a log, and communicate through a state file (`daemon/factory-ao-remediate.sh:197`). The caller waits only for a bounded fast-fail window, then accepts `pending` (`daemon/factory-ao-remediate.sh:226`).
- Both branches call the same `run_spawn_foreground` command builder (`daemon/factory-ao-remediate.sh:142`), so that is the shell path's last shared pre-spawn seam. **ponytail:** async is retained for tick backpressure; sync/async must not grow separate PR-validation policies.

## God-mode paths

### `factory-af-tick.sh` + `factory-ao-remediate.sh`

This is a complete parallel dispatcher. It reads the overlay directly, invokes `ao spawn --claim-pr`, then asks the overlay harness to mark dispatch (`daemon/factory-af-tick.sh:291`, `daemon/factory-ao-remediate.sh:142`). It bypasses all of:

- `intake::normalize_labeled_prs_outcome` live PR envelope construction;
- `tick::resolve_drive_pr_head_branch` open/same-repo check;
- `dispatch_ready_with_vcs` branch registry + `DISPATCHING`-before-process transaction;
- `SpawnSpec.expected_revision` and target-checkout verification;
- `CliSessions::run_spawn_process` session/worktree/branch validation;
- dispatch's post-spawn remote validation and durable `session_id` save.

Every non-test caller found:

- `daemon/launchd/ai.dark-factory.af-tick.plist.template:42` schedules `factory-af-tick.sh` (legacy macOS deployment artifact; project policy now says Linux systemd is production).
- `daemon/factory-af-tick.sh:37` calls `factory-ao-remediate.sh`.
- Deployment tooling copies the scripts but does not itself dispatch (`daemon/scripts/deploy-af-tick.sh:174`).
- `daemon/factory-tick.sh:8` calls only the shell intake script, not the remediation/spawn script.

Tests call these scripts through `tests/scripts/test_factory_af_tick*.sh`, `tests/scripts/test_factory_ao_remediate.sh`, and `tests/scripts/test_rollback_dispatched.sh`; those are verification callers, not runtime authorities.

### `factory-overlay.sh redrive-pr`

`redrive-pr <id> <pr> <branch>` directly overwrites `state`, `attempt`, `pr_number`, `branch`, `is_adopted`, and `session_id` (`daemon/factory-overlay.sh:524`). It validates only syntax. It bypasses:

- GitHub existence/open-state, repo, branch, and head-SHA reads;
- PR author permission/fork admission;
- `resolve_target_repo` and configured repo mapping;
- branch registry collision registration (that happens later, if dispatch reaches `dispatch-record`);
- active-session reconciliation before resetting `session_id`.

Every caller found is operator/documentation or test-facing: the command is exposed by `.claude/skills/auto-factory/SKILL.md:112`; direct executable uses are in `tests/scripts/test_factory_overlay.sh:316` and discovery coverage in `tests/scripts/test_callpath_overlay_harness.sh:44`. No production Rust caller uses it. **ponytail:** preserve it as an explicit recovery tool, but its written tuple is a seed to validate, never spawn authority.

### `reroll::execute_adopted`

This Rust handler calls `Sessions::spawn` directly (`daemon/src/reroll.rs:1419`) rather than re-entering `dispatch_ready_with_vcs`. It intentionally bypasses normal route selection, `DriveBranchDecision`, branch registration, and base-head selection. It performs its own active-session, checkout, and remote-head capture first (`daemon/src/reroll.rs:1223`, `daemon/src/reroll.rs:1279`, `daemon/src/reroll.rs:1341`).

Production `Sessions::spawn` callers are exactly:

- normal dispatch at `daemon/src/dispatch.rs:668`;
- adopted reroll at `daemon/src/reroll.rs:1419`.

All other source hits are adapter/dispatch tests. Therefore any Rust-only pre-spawn authority must cover both production callers; guarding only `dispatch_ready_with_vcs` leaves adopted reroll as a bypass.

### Direct AO command surface

The shell remediation command at `daemon/factory-ao-remediate.sh:146` is the only production direct `ao spawn` outside the Rust `CliSessions` adapter. The JavaScript preload bridge (`daemon/scripts/ao-spawn-v013-bridge.mjs`) belongs to the Rust adapter command path and does not create a third product-level admission policy.

## Authority conclusion

The system already knows the necessary PR seed fields at intake, but no durable or shared pre-spawn contract keeps them together. The authoritative object for this goal is the fresh GitHub tuple **`(repo, PR number, open state, same-repo head branch, head SHA)`**. Today:

- Rust reduces it to a branch before dispatch and validates the checkout against the base SHA.
- SQLite/body fields retain only stale-capable projections.
- adopted reroll and the legacy shell dispatcher are independent spawn entrances.
- post-spawn reconciliation repairs PR identity only after the unsafe action boundary.

**ponytail:** the smallest authority shape is one shared pre-spawn envelope check covering every AO entrance; all body, overlay, cache, telemetry, and async-state representations remain read-only mirrors or receipts.
