# Explore: authorities for adopted-PR target drift before AO spawn

Scope: visible `dark-factory` sources only. This maps current ownership for the P0 goal “Reject adopted-PR target drift before AO spawn” and does not inspect sealed holdouts or evaluator internals.

## Current authorities

| Concept | Current authoritative component | Projections / notes |
|---|---|---|
| Live adopted PR identity and current target | GitHub, read through `Scm::open_pr_head_ref_for_repo(repo, pr)`; the production boundary is `CliScm`'s REST `repos/{repo}/pulls/{pr}` query and `parse_open_pr_head_ref`, which jointly decide OPEN vs missing and same-repo vs fork, and return the live head ref (`daemon/src/adapters.rs:2310`, `daemon/src/adapters.rs:2317`, `daemon/src/adapters.rs:2713`, `daemon/src/adapters.rs:2745`). | This is the only visible component that can answer “does adopted PR N still target branch B in repo R now?” `BeadOverlay.branch` is a snapshot, not an upstream fact. |
| Adoption-time PR snapshot | `LabeledPr`, populated by the SCM adapter from the GitHub PR list/REST response (`daemon/src/tools.rs:113`, `daemon/src/adapters.rs:1165`, `daemon/src/adapters.rs:1259`). `ExistingPrIntake` carries the normalized snapshot into the slow tier (`daemon/src/intake.rs:404`). | `head_ref_name`, `head_sha`, repo, and PR number are point-in-time projections. The permission/adoption cache is not a live target authority. |
| Whether a PR is eligible for adoption | `normalize_labeled_prs_with_cache`: same-repo/fork eligibility, authorization, deduplication, and bead creation (`daemon/src/intake.rs:939`, `daemon/src/intake.rs:985`, `daemon/src/intake.rs:1019`, `daemon/src/intake.rs:1089`). `same_repo_pr` owns the intake fork decision (`daemon/src/intake.rs:552`). | The legacy `normalize_labeled_prs` wrapper is a second entry point but delegates the same concepts only for `cfg.target_repo` (`daemon/src/intake.rs:1341`). |
| Durable adopted-bead identity | SQLite `bead_overlay`, accessed through `StateStore`; `BeadOverlay` owns `target_repo`, `pr_number`, `branch`, and explicit `is_adopted` provenance (`daemon/src/state.rs:87`, `daemon/src/state.rs:95`, `daemon/src/state.rs:98`, `daemon/src/state.rs:147`). `SqliteStateStore::save` upserts those fields as one overlay row (`daemon/src/state.rs:1609`, `daemon/src/state.rs:1643`). | Durable authority for daemon lifecycle/recovery, but a cached projection of live GitHub PR targeting. |
| Adopted provenance | Explicit `BeadOverlay.is_adopted`, set by labeled-PR adoption in the slow tier and by initial drive-PR dispatch when `branch_mode == "pr_head"` (`daemon/src/tick.rs:1812`, `daemon/src/tick.rs:1823`, `daemon/src/dispatch.rs:552`, `daemon/src/dispatch.rs:570`). | Never infer adoption from branch syntax or merely from `pr_number`; those are projections (`daemon/src/dispatch.rs:459`). |
| Per-bead repository identity inside the daemon | `BeadOverlay::repo(cfg)` is the canonical accessor (`daemon/src/state.rs:178`). Intake parsing authority is `intake::resolve_target_repo`, with body `target_repo:` winning over `external_ref` (`daemon/src/intake.rs:459`, `daemon/src/intake.rs:475`). Config-to-AO routing authority is `Config::resolve_repo` (`daemon/src/config.rs:238`, `daemon/src/config.rs:249`). | For a labeled adopted PR, `tick` derives `target_repo` from the SCM-supplied `external_ref`, not the PR body (`daemon/src/tick.rs:1788`, `daemon/src/tick.rs:1794`). |
| Initial drive-existing-PR branch decision | `tick::resolve_drive_pr_head_branch` performs a fresh SCM query and returns `DriveBranchDecision`; `dispatch_ready_with_vcs` consumes that decision (`daemon/src/tick.rs:6414`, `daemon/src/tick.rs:6438`, `daemon/src/tick.rs:6460`, `daemon/src/dispatch.rs:77`, `daemon/src/dispatch.rs:194`). | This protects the initial ordinary `QUEUED -> DISPATCHING -> AO spawn` path, not adopted remediation after the overlay has aged. |
| Ordinary dispatch admission and pre-spawn intent | `dispatch::dispatch_ready_with_vcs` owns capacity, repo recovery/mapping, checkout selection, branch registration, durable `DISPATCHING`, expected revision, and the normal `Sessions::spawn` call (`daemon/src/dispatch.rs:162`, `daemon/src/dispatch.rs:213`, `daemon/src/dispatch.rs:220`, `daemon/src/dispatch.rs:496`, `daemon/src/dispatch.rs:648`, `daemon/src/dispatch.rs:668`). | This is the shared normal spawn pipeline. |
| Adopted remediation dispatch | `reroll::execute` selects the adopted path solely from durable `is_adopted`, then `execute_adopted` owns duplicate-session checks, checkout/routing, remote branch SHA capture, durable pre-spawn `DISPATCHING`, and direct AO spawn (`daemon/src/reroll.rs:313`, `daemon/src/reroll.rs:448`, `daemon/src/reroll.rs:1186`, `daemon/src/reroll.rs:1223`, `daemon/src/reroll.rs:1341`, `daemon/src/reroll.rs:1410`, `daemon/src/reroll.rs:1419`). | Today it treats the stored branch as its target authority and never calls `open_pr_head_ref_for_repo` before spawn. That is the goal's authority gap. |
| Remote branch revision used to seed/verify the worker | `Vcs::remote_head_sha(branch)` supplies `SpawnSpec.expected_revision` for adopted remediation (`daemon/src/tools.rs:1030`, `daemon/src/reroll.rs:1343`, `daemon/src/reroll.rs:1393`). | Authoritative for the named git branch tip, but cannot prove that the PR still points to that branch. |
| Spawn contract | `SpawnSpec` is the immutable handoff containing repo, AO project, remote, branch, checkout, expected revision, and expected cwd (`daemon/src/tools.rs:236`). `CliSessions::spawn` delegates to the fallback spawn boundary (`daemon/src/adapters.rs:6309`, `daemon/src/adapters.rs:6319`). | A handoff projection. It should be constructed only after the live adopted PR target is validated. |
| Worker checkout identity after AO spawn | `CliSessions::run_spawn_process` validates AO's reported branch and validates the returned workspace against `spec.repo` and `spec.expected_revision` (`daemon/src/adapters.rs:3290`, `daemon/src/adapters.rs:3320`, `daemon/src/adapters.rs:3328`, `daemon/src/adapters.rs:3333`). | Post-spawn validation can kill a misbound worker, but it is too late to satisfy “before AO spawn” and validates the branch/revision requested, not live PR ownership. |
| Durable proof that adopted remediation actually spawned | The SQLite `remediation_session_spawned` marker, written atomically with the post-spawn `DISPATCHED` overlay by `save_remediation_session_spawned` (`daemon/src/state.rs:283`, `daemon/src/state.rs:304`, `daemon/src/state.rs:2127`, `daemon/src/state.rs:2159`). | `pre_session_head_sha` is only pre-spawn intent and explicitly is not proof (`daemon/src/reroll.rs:324`). |
| Adoption probe cache | On-disk `AdoptionProbeCache`, keyed by `(external_ref, head_sha, updated_at_epoch)`, with TTL and incomplete-key rejection (`daemon/src/intake.rs:90`, `daemon/src/intake.rs:138`, `daemon/src/intake.rs:204`, `daemon/src/intake.rs:318`, `daemon/src/intake.rs:349`). | Caches permission/adoption decisions only. It is not authoritative for a later pre-spawn target check. **ponytail:** keep this cache as a compatibility/performance ceiling; do not reuse it as the final safety authority. |
| Branch ownership registry | SQLite `branch_registry`, with uniqueness enforced by `StateStore::register_branch` (`daemon/src/state.rs:196`, `daemon/src/state.rs:1754`). | Authority for which bead the daemon registered against a branch, not for which branch GitHub's PR currently targets. |
| Telemetry | Append-only event records emitted after decisions (for example `EXISTING_PR_ADOPTED` at `daemon/src/tick.rs:1841` and `REROLL_ADOPTED_SESSION_SPAWNED` at `daemon/src/reroll.rs:1457`). | Read-only audit projection; never state authority. |

## Conflicting authorities

### Stored adopted target versus live PR target

- `tick` persists adoption-time `(target_repo, pr_number, branch)` into `bead_overlay` (`daemon/src/tick.rs:1794`, `daemon/src/tick.rs:1812`). Later, `execute_adopted` loads only `bead.branch` and uses it through session attach, SHA capture, prompt construction, and `SpawnSpec` (`daemon/src/reroll.rs:1190`, `daemon/src/reroll.rs:1227`, `daemon/src/reroll.rs:1340`, `daemon/src/reroll.rs:1393`).
- GitHub can change the effective target after that snapshot: the PR may close, become unavailable/forked, or its live head ref may no longer equal the stored branch. The code already recognizes the same conflict before gate assessment and re-resolves it (`daemon/src/tick.rs:4779`, `daemon/src/tick.rs:4807`), but not before adopted remediation spawn.
- Single authority that should win: the fresh `Scm::open_pr_head_ref_for_repo(overlay.repo(cfg), overlay.pr_number)` result immediately upstream of `execute_adopted`'s `Sessions::spawn`. The overlay fields, prompt branch, `SpawnSpec`, remote SHA, worktree HEAD, AO output, and telemetry must be read-only projections of that verdict for this attempt.
- **ponytail:** one guard in the shared adopted-remediation function (`execute_adopted`) is the ceiling; guards in `tick`, each caller, each agent fallback, or post-spawn cleanup would duplicate policy and still leave a bypass.

### Repo identity body field versus external PR identity

- Generic/manual intake says an explicit body `target_repo:` overrides `external_ref` (`daemon/src/intake.rs:459`, `daemon/src/intake.rs:475`). Initial drive-PR binding then refuses to bind when `external_ref`'s repo differs from already-resolved `target_repo` (`daemon/src/tick.rs:6417`, `daemon/src/tick.rs:6451`).
- Labeled-PR adoption instead derives repo from the SCM-generated canonical external ref (`daemon/src/tick.rs:1788`, `daemon/src/tick.rs:1794`). This is safer for adopted PRs because the PR query's repo owns the identity.
- Single authority for adopted PR targeting should be the SCM query key `(repo, pr_number)` established by adoption, not mutable descriptive body fields. `target_repo:` remains authoritative only for generic/manual bead routing before an adopted PR identity is positively established.

### `cfg.target_repo` versus per-bead `target_repo`

- `BeadOverlay::repo` retains `None -> cfg.target_repo` fallback (`daemon/src/state.rs:178`), while modern dispatch explicitly parks unresolved `None` before that fallback can mask missing identity (`daemon/src/dispatch.rs:265`, `daemon/src/dispatch.rs:316`). Other call sites can still observe the legacy default.
- `Config::resolve_repo` then maps the chosen repo to AO project/remote (`daemon/src/config.rs:238`).
- Single authority should be non-null persisted per-bead repo once intake resolves it. `cfg.target_repo` is only a legacy default/configuration seed, not permission to overwrite adopted identity.
- **ponytail:** retain `None -> cfg.target_repo` only as a legacy read compatibility projection; do not let it write or silently repair an adopted row.

### Normal dispatch policy versus adopted direct-spawn policy

- `dispatch_ready_with_vcs` is the normal spawn authority and centralizes capacity, mapping, checkout, branch registry, intent persistence, retries, and cleanup (`daemon/src/dispatch.rs:213`).
- `execute_adopted` is a second writer of spawn state and independently reconstructs routing, checkout, expected revision, `DISPATCHING`, and post-spawn persistence (`daemon/src/reroll.rs:1279`, `daemon/src/reroll.rs:1312`, `daemon/src/reroll.rs:1393`, `daemon/src/reroll.rs:1410`). Its `resolve_repo(...).unwrap_or_else(...)` fallback can manufacture AO routing that normal dispatch would reject (`daemon/src/reroll.rs:1280`, `daemon/src/reroll.rs:1382`).
- For this goal, adopted target validation belongs in the one shared adopted spawn function, because refactoring all spawn mechanics is outside explore scope. Its target verdict must not be delegated to adapter fallback or caller-specific checks.

### Remote branch SHA versus PR head association

- `remote_head_sha(stored_branch)` is current for that branch (`daemon/src/tools.rs:1030`), and `expected_revision`/worktree validation guarantees AO starts at that SHA (`daemon/src/tools.rs:262`, `daemon/src/adapters.rs:3333`).
- It says nothing about whether `(repo, pr_number)` still points at `stored_branch`. These are complementary facts, not interchangeable authorities. Live PR association must be validated first; only then may remote branch SHA become authoritative for the checkout revision.

### Legacy projection fields

- `BeadOverlay.target_repo = None` means legacy fallback to global config (`daemon/src/state.rs:147`, `daemon/src/state.rs:188`). Read-compatible mirror only.
- `CliSessions.project` is constructor-time global state, while per-spawn `spec.ao_project` now wins (`daemon/src/adapters.rs:3266`, `daemon/src/adapters.rs:3290`). Legacy mirror only.
- Shell-normalized `## existing_pr`, `## existing_branch`, and `## target_repo` body lines are descriptive compatibility fields created by `factory-intake-from-gh.sh` (`daemon/factory-intake-from-gh.sh:186`). The Rust path does not parse `existing_pr:` or `existing_branch:` as target authority; it uses `external_ref` plus SCM (`daemon/src/tick.rs:6414`).
- `pre_session_head_sha` is durable pre-spawn intent, while `remediation_session_spawned` is durable post-spawn fact (`daemon/src/reroll.rs:324`, `daemon/src/state.rs:283`).
- `ExistingPrIntake.head_sha`, adoption cache entries, telemetry context, `SpawnSpec`, in-memory `spawned_worktrees`, and AO stdout `Branch:`/`Worktree:` are snapshots/mirrors (`daemon/src/intake.rs:404`, `daemon/src/adapters.rs:3366`).
- **ponytail:** keep these mirrors where compatibility, crash recovery, backpressure, or observability needs them; prohibit them from competing with live SCM at the pre-spawn decision boundary.

## Implicit state machines

### `bead_overlay.state`

- Key: `(bead_id, state, attempt, pr_number, branch, session_id, is_adopted, target_repo, pre_session_head_sha, park_reason)` in SQLite `bead_overlay` (`daemon/contracts/schema.sql:4`, `daemon/src/state.rs:87`).
- Adoption lifecycle: live PR snapshot -> overlay `ATTESTED`, `pr_number`, `branch`, `is_adopted=true`, `target_repo` (`daemon/src/tick.rs:1776`, `daemon/src/tick.rs:1812`). Red gate -> `reroll::execute` persists `RE_ROLL` (`daemon/src/reroll.rs:313`, `daemon/src/reroll.rs:338`). Adopted pre-spawn -> `DISPATCHING` plus SHA intent (`daemon/src/reroll.rs:1410`). Spawn success -> `DISPATCHED`, incremented attempt/reroll, session id, atomic remediation marker (`daemon/src/reroll.rs:1420`). Quiescent worker -> `ATTESTED` again; green -> `READY`; unsafe/inconclusive conditions -> `HUMAN_HELD`.
- Persistence: `SqliteStateStore::save` is the durable overlay boundary (`daemon/src/state.rs:1609`, `daemon/src/state.rs:1704`). Startup treats any surviving `DISPATCHING` row as ambiguous and parks it (`daemon/src/state.rs:1644`).
- Readers: slow-tier adoption/dispatch, fast-tier session/gate monitoring, reroll, recovery, escalation, and shell compatibility scripts.
- Gap: the state machine has no durable “adopted target revalidated at time T” state, and `execute_adopted` transitions to `DISPATCHING` without comparing the current PR head to the stored branch.

### Adopted remediation proof

- Keys: `pre_session_head_sha` in `bead_overlay` and `(bead_id, attempt)` in `remediation_session_spawned` (`daemon/src/state.rs:120`, `daemon/src/state.rs:283`).
- Lifecycle: SHA is saved before AO; marker is written only after spawn plus `DISPATCHED` save; circuit-breaker logic reads the marker to distinguish a real remediation attempt from a preflight/spawn failure (`daemon/src/reroll.rs:324`, `daemon/src/reroll.rs:330`, `daemon/src/state.rs:2159`).
- Persistence boundary: the marker and overlay are committed in one SQLite transaction (`daemon/src/state.rs:2164`). AO itself remains external; cleanup compensates if post-spawn persistence fails (`daemon/src/reroll.rs:1430`).

### Branch ownership

- Key: `branch_registry.branch -> bead_id` (`daemon/src/state.rs:1754`).
- Lifecycle: labeled adoption checks for another bead's ownership, registers the branch, then writes the overlay (`daemon/src/tick.rs:1721`, `daemon/src/tick.rs:1774`). Normal dispatch also registers before `DISPATCHING` (`daemon/src/dispatch.rs:496`). Adopted reroll reuses the already-registered branch and does not re-register it.
- Persistence boundary: registry is durable SQLite authority for daemon ownership, while GitHub remains authoritative for PR-to-branch binding.

### Adoption probe cache

- Key: `(external_ref, head_sha, updated_at_epoch) -> permission decision + cached_at` (`daemon/src/intake.rs:138`, `daemon/src/intake.rs:204`).
- Lifecycle: load at slow-tier start, use fresh complete entries, insert misses, persist at tick end (`daemon/src/tick.rs:1668`, `daemon/src/intake.rs:318`, `daemon/src/intake.rs:349`, `daemon/src/tick.rs:2997`).
- Persistence boundary: JSON file under runtime state survives restart but degrades to cold cache on corruption/missing data (`daemon/src/intake.rs:90`). GitHub remains authoritative; cache freshness only suppresses permission probes.

### AO session/worktree state

- Keys: external AO project/session records plus in-memory maps `(ao_project, branch) -> workspace` and `session_id -> (branch, workspace)` (`daemon/src/adapters.rs:3266`).
- Lifecycle: spawn output supplies session/worktree/branch; adapter verifies, then caches mappings (`daemon/src/adapters.rs:3311`, `daemon/src/adapters.rs:3366`). Session attach/quiescence governs duplicate prevention in adopted reroll (`daemon/src/reroll.rs:1223`).
- Persistence boundary: AO owns durable session truth; `CliSessions` maps are process-local caches lost on restart. The SQLite overlay's `session_id` is another projection reconciled through AO queries.

### Legacy shell overlay

- Keys: the same SQLite `bead_overlay` schema plus detached spawn state files (`pending|ok|fail:rc=N`) (`daemon/factory-ao-remediate.sh:197`, `daemon/factory-ao-remediate.sh:205`).
- Lifecycle: `factory-af-tick.sh` selects a row, spawns first, then calls `factory-overlay.sh dispatch-record` (`daemon/factory-af-tick.sh:350`, `daemon/factory-af-tick.sh:357`, `daemon/factory-af-tick.sh:366`). This is the inverse of Rust's durable-intent-before-spawn lifecycle.
- Persistence boundary: SQLite may say `DISPATCHED` while the detached state file later says failure; rollback tooling reconciles them on later ticks. **ponytail:** compatibility ceiling only; it must not become a second accepted authority for adopted-target validation.

## Streaming / non-streaming branches

### Rust daemon

- There is no semantic streaming versus non-streaming adopted-target branch. `CliSessions::run_spawn_process` uses `Command::output`, buffers complete AO stdout/stderr, then classifies and validates the returned session (`daemon/src/adapters.rs:3302`, `daemon/src/adapters.rs:3305`, `daemon/src/adapters.rs:3311`). All fallback agents reuse this same function through `spawn_with_fallback` (`daemon/src/adapters.rs:3492`).
- Generic `run_tool` drains stdout/stderr concurrently on reader threads to prevent pipe backpressure, but still returns a buffered string only after process completion (`daemon/src/tools.rs:1090`, `daemon/src/tools.rs:1184`, `daemon/src/tools.rs:1191`). This is transport concurrency, not a distinct state-authority path.
- **ponytail:** keep concurrent draining for backpressure; do not split target validation by output mode because the spawn contract is non-streaming at the decision boundary.

### Legacy shell remediation

- Sync mode waits for `ao spawn`, classifies final output, and returns success/failure (`daemon/factory-ao-remediate.sh:169`, `daemon/factory-ao-remediate.sh:185`).
- Default async mode writes `pending`, detaches the same foreground spawn helper, polls up to five seconds, and may return success while the final state is still pending (`daemon/factory-ao-remediate.sh:197`, `daemon/factory-ao-remediate.sh:208`, `daemon/factory-ao-remediate.sh:210`, `daemon/factory-ao-remediate.sh:226`, `daemon/factory-ao-remediate.sh:259`).
- Both branches bypass the Rust pre-spawn authority pipeline. Async additionally crosses a persistence split where caller acknowledgment can precede authoritative AO outcome.

## God-mode paths

### `reroll::execute_adopted` direct spawn

- Call chain: `tick`'s red-gate remediation invokes `reroll::execute`; `execute` branches on `is_adopted` and calls `execute_adopted`; `execute_adopted` calls `deps.sessions.spawn` directly (`daemon/src/reroll.rs:313`, `daemon/src/reroll.rs:448`, `daemon/src/reroll.rs:1186`, `daemon/src/reroll.rs:1419`).
- Normal flow bypassed: `dispatch_ready_with_vcs`, whose callers rely on it for capacity/batch admission, missing/unmapped repo rejection, checkout validation, `DriveBranchDecision`, branch registration, spawn retry accounting, and common cleanup (`daemon/src/dispatch.rs:213`).
- Every production Rust `Sessions::spawn` caller is either normal dispatch (`daemon/src/dispatch.rs:668`) or this adopted reroll (`daemon/src/reroll.rs:1419`); `spawn_batch` has no production caller beyond its trait/default and adapter implementation. Adapter test calls are not runtime paths.
- Most important bypass for this goal: initial drive-PR dispatch gets a fresh `open_pr_head_ref_for_repo` decision in `tick`, but adopted reroll does not. A guard only in `dispatch_ready_with_vcs`, `tick`, or `CliSessions` would leave `execute_adopted` uncovered or too late.

### Legacy `factory-af-tick.sh -> factory-ao-remediate.sh`

- Runtime caller: `factory-af-tick.sh` invokes the remediation script directly (`daemon/factory-af-tick.sh:37`, `daemon/factory-af-tick.sh:357`); the repository also ships a launchd template that invokes `factory-af-tick.sh` (`daemon/launchd/ai.dark-factory.af-tick.plist.template:38`). Tests and the bze8.2 canary invoke/stub these scripts, but they are not additional production writers.
- Bypasses: Rust `intake::normalize_labeled_prs*`, `tick::resolve_drive_pr_head_branch`, `dispatch_ready_with_vcs`, `reroll::execute_adopted`, `SpawnSpec`, `CliSessions` workspace verification, and Rust's intent-before-spawn state transition.
- It takes repo/PR/project arguments, uses AO `--claim-pr`, spawns before `dispatch-record`, and has no visible fresh comparison between the selected overlay branch and current PR head immediately before AO (`daemon/factory-ao-remediate.sh:142`, `daemon/factory-af-tick.sh:350`, `daemon/factory-af-tick.sh:357`, `daemon/factory-af-tick.sh:366`).
- **ponytail:** treat this as a legacy compatibility lane, not a co-authority. If still runnable, it needs to delegate to the same upstream adopted-target guard or be explicitly excluded from the claimed safety invariant.

### Adapter fallback chain

- `CliSessions::spawn_with_fallback` may try multiple AO agents, but every agent goes through `run_spawn_process(spec)` (`daemon/src/adapters.rs:3492`). This is not a separate target authority: all attempts reuse the same caller-provided `SpawnSpec`.
- A target change between fallback attempts is therefore invisible unless validation occurs before each external AO attempt. The smallest shared boundary is the adopted remediation path before calling `Sessions::spawn`; if the invariant requires protection across a long fallback sequence, the `Sessions` contract would need a live SCM capability it currently does not have. That is a ceiling to make explicit, not a reason to accept several writers.

## Authority conclusion

The present bug is a stale-projection conflict: `execute_adopted` treats durable `BeadOverlay.branch` plus a fresh remote SHA as sufficient authority, while only GitHub's live `(repo, pr_number) -> OPEN same-repo head ref` query can prove the adopted PR still targets that branch. The single upstream authority that should win is `Scm::open_pr_head_ref_for_repo` at the shared adopted-remediation pre-spawn boundary. Every later field is a read-only mirror for that attempt.
