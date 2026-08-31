## Authors / Authorities

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

## Concepts

# Explore: concepts — reject adopted-PR target drift before AO spawn

Goal: `[P0][factory][RED] Reject adopted-PR target drift before AO spawn (jleechanorg/worldarchitect.ai)`

## Concept inventory

### Ponytail seven-rung gate

1. **Does this need to be built?** The visible code has adopted-PR repo identity at intake and AO-target identity at dispatch, but no named pre-spawn comparison between those two facts. The repository does have post-spawn wrong-worktree/remote checks. The requested failure class therefore maps to a real boundary; whether another existing guard already makes the new comparison redundant is an open question, not an invented concept (`daemon/src/tick.rs:1788`, `daemon/src/dispatch.rs:648`, `daemon/src/dispatch.rs:851`).
2. **Does it already exist?** Reusable owners already exist for every operand: `ExistingPrIntake.repo`, canonical `external_ref`, `BeadOverlay.target_repo`, `BeadOverlay::repo`, `Config::resolve_repo`, `DriveBranchDecision`, `SpawnSpec.repo`, and the `DispatchFailure`/`HumanHoldReason` park path (`daemon/src/intake.rs:405`, `daemon/src/state.rs:178`, `daemon/src/config.rs:238`, `daemon/src/dispatch.rs:77`, `daemon/src/tools.rs:248`, `daemon/src/dispatch.rs:123`, `daemon/src/state.rs:618`). There is no visible `target_drift` concept to reuse.
3. **Standard library?** The relevant existing checks are ordinary string/option comparisons; no additional library concept is visible (`daemon/src/intake.rs:552`, `daemon/src/tick.rs:6451`).
4. **Native platform?** GitHub supplies PR base/head repository identity and AO supplies project/worktree identity, but the code owns their reconciliation (`daemon/src/adapters.rs:1240`, `daemon/src/adapters.rs:2310`, `daemon/src/adapters.rs:3320`).
5. **Installed dependency?** Existing `serde`, `toml`, and `rusqlite` persist/transport the facts; none owns adopted-target consistency (`daemon/src/config.rs:8`, `daemon/src/state.rs:1609`, `daemon/src/telemetry.rs:6`).
6. **Can it be one line?** Equality is small, but a fail-closed rejection also has durable state, phase/reason, telemetry, escalation, and cleanup semantics. The visible analogues are multi-surface (`daemon/src/dispatch.rs:350`, `daemon/src/tick.rs:2656`).
7. **Minimum code only.** This explore artifact proposes no code. Rung 2 gates the inventory below: every listed term has a visible writer/owner and reader. Speculative names are kept in **Open questions**.

### Inventory

| Term | Short definition | Source of truth |
|---|---|---|
| `Config.target_repo` | Process-default GitHub `owner/repo`; production is `jleechanorg/worldarchitect.ai`. | `daemon/src/config.rs:29`; `config/daemon.toml:1` |
| `Config.repos` / `RepoConfig` / `RepoRouting` | Per-repository mapping from GitHub repo to AO project, push remote, and optional checkout. `Config::resolve_repo` is the canonical resolver and returns `None` for unknown targets. | `daemon/src/config.rs:5`; `daemon/src/config.rs:19`; `daemon/src/config.rs:71`; `daemon/src/config.rs:238`; `config/daemon.toml:17` |
| `LabeledPr` repo/head identity | SCM intake record containing PR number, canonical external reference, head branch, fork/same-repo metadata, head SHA, and update epoch. | `daemon/src/tools.rs:111`; adapters populate it at `daemon/src/adapters.rs:1259` |
| Same-repository adoption eligibility | `same_repo_pr` rejects fork/cross-repository heads by comparing PR head metadata to the repository currently being swept. | `daemon/src/intake.rs:552`; consumed at `daemon/src/intake.rs:1003` |
| `ExistingPrIntake` | Normalized adopted-PR handoff from intake to the slow tick: bead id, PR number, head branch, external ref, source repo, head SHA, and whether the bead was newly created. | `daemon/src/intake.rs:405`; written at `daemon/src/intake.rs:1095` and `daemon/src/intake.rs:1159`; read at `daemon/src/tick.rs:1716` |
| Canonical `external_ref` | Cross-module identity string `owner/repo#number`; intake canonicalizes URL/short forms before dedup and derives repo identity from it. | `daemon/src/intake.rs:484`; `daemon/src/intake.rs:497`; adapter writer `daemon/src/adapters.rs:1264` |
| `target_repo:` body field | Bead-description repo override. `resolve_target_repo` gives it precedence over `external_ref`; blank fields fall through. This is the only drive-field parsed by Rust production code. | `daemon/src/intake.rs:459`; parser at `daemon/src/intake.rs:534` |
| `BeadOverlay.target_repo` / `BeadOverlay::repo` | Durable per-bead repo identity. The accessor uses the explicit value or falls back to `cfg.target_repo`. | `daemon/src/state.rs:145`; accessor `daemon/src/state.rs:178`; SQL column `daemon/contracts/schema.sql:81`; persistence `daemon/src/state.rs:1609` |
| Adopted-PR provenance (`is_adopted`) | Durable boolean saying the branch is an external PR head, not inferred from branch naming. It selects append-only remediation and branch reuse. | `daemon/src/state.rs:98`; SQL contract `daemon/contracts/schema.sql:33`; adoption writer `daemon/src/tick.rs:1815`; dispatch reader/writer `daemon/src/dispatch.rs:459` |
| Adopted PR binding (`pr_number`, `branch`, `head_sha`) | The adopted PR number and head branch are copied into the overlay; the intake head SHA is emitted for attribution/cache invalidation but is not persisted into the overlay during initial adoption. | intake fields `daemon/src/intake.rs:405`; overlay fields `daemon/src/state.rs:95`; adoption write `daemon/src/tick.rs:1812`; adoption event `daemon/src/tick.rs:1842` |
| Branch registry | Durable branch-to-bead ownership guard used before adoption and again before dispatch; collisions are rejected before a worker exists. | interface `daemon/src/state.rs:193`; adoption writer/check `daemon/src/tick.rs:1721`; dispatch writer/check `daemon/src/dispatch.rs:496` |
| Drive-existing-PR branch resolution | For queued beads, `external_ref` repo must equal the resolved overlay repo; the SCM must confirm the PR is open and same-repo before returning `DriveBranchDecision::PrHead`. Otherwise it returns fork/generated fallback. | enum `daemon/src/dispatch.rs:77`; resolver `daemon/src/tick.rs:6418`; ready handoff `daemon/src/tick.rs:2284`; SCM result `daemon/src/tools.rs:694` |
| `DispatchReport` / `DispatchFailure.phase` | Per-bead dispatch outcome boundary. Permanent config/safety failures park only that bead and are translated by `tick.rs` into operator-visible lifecycle events. | `daemon/src/dispatch.rs:102`; `daemon/src/dispatch.rs:123`; `daemon/src/dispatch.rs:133`; reader `daemon/src/tick.rs:2380` |
| `HUMAN_HELD` / `park_reason` | Fail-closed durable state and machine-readable reason. Recovery uses an allow-list, so unknown/new permanent reasons do not auto-requeue. | state `daemon/src/state.rs:12`; reason enum `daemon/src/state.rs:618`; writer helper `daemon/src/state.rs:782`; recovery rule `daemon/src/state.rs:786` |
| `SpawnSpec` | Final per-spawn contract: bead, branch, prompt, repo, AO project, remote, checkout, expected revision, managed-checkout flag, expected cwd. | `daemon/src/tools.rs:236`; constructed at `daemon/src/dispatch.rs:648`; consumed at `daemon/src/adapters.rs:3290` |
| AO spawn CLI boundary | Rust passes `--project <SpawnSpec.ao_project>`, `--agent`, prompt, target checkout, expected revision, and branch env vars; the bridge validates project/source/revision and calls AO with the exact branch. | command writer `daemon/src/adapters.rs:2990`; argv `daemon/src/adapters.rs:3020`; bridge reader `daemon/scripts/ao-spawn-v013-bridge.mjs:44`; source/revision guard `daemon/scripts/ao-spawn-v013-bridge.mjs:120`; final spawn `daemon/scripts/ao-spawn-v013-bridge.mjs:606` |
| Pre-/post-spawn workspace guards | Before AO, managed checkout/revision/remote are prepared; after AO returns, the adapter checks returned branch/worktree/revision, and dispatch verifies the worktree remote matches `SpawnSpec.repo`. | pre-command `daemon/src/adapters.rs:2990`; returned workspace guard `daemon/src/adapters.rs:3320`; post-spawn dispatch guard `daemon/src/dispatch.rs:809`; remote guard `daemon/src/dispatch.rs:851` |
| Telemetry envelope | JSONL record with `beadId`, attempt, lifecycle state, event type, metrics, and context. | schema/writer `daemon/src/telemetry.rs:6`; file append `daemon/src/telemetry.rs:53` |
| Existing lifecycle event types | `EXISTING_PR_ADOPTED` identifies adopted origin; `TASK_ROUTED` records resolved target; `TASK_DISPATCHED` records successful AO dispatch; `PARKED_HUMAN_HELD`, `ESCALATION_REQUIRED`, and `BEAD_DISPATCH_TRANSIENT_ERROR` carry current rejection/escalation outcomes. | writers `daemon/src/tick.rs:1842`, `daemon/src/tick.rs:2273`, `daemon/src/tick.rs:2965`, `daemon/src/tick.rs:2538`, `daemon/src/tick.rs:2636`, `daemon/src/tick.rs:2949`; readers `runner/funnel_lanes.py:152`, `daemon/scripts/fe_audit_query.py:94`, `daemon/scripts/fe_audit_query.py:108` |
| Legacy shell drive-PR route | Separate shell path normalizes `existing_pr`, `existing_branch`, and `target_repo`, stores target repo in the overlay, resolves AO project per repo, then calls remediation before `dispatch-record`. It is not the Rust `ExistingPrIntake` route. | writer `daemon/factory-intake-from-gh.sh:186`; reader/dispatch loop `daemon/factory-af-tick.sh:313`; AO caller `daemon/factory-ao-remediate.sh:142` |

## Grep coverage

Searches were repository-wide with `.git`, `holdouts`, and `_holdout` excluded. Sealed evaluator/holdout content was not opened.

### Adopted PR / intake terms

- `LabeledPr`: definition `daemon/src/tools.rs:111`; GitHub REST/GraphQL writers `daemon/src/adapters.rs:1165`, `daemon/src/adapters.rs:1259`, `daemon/src/adapters.rs:1907`; normalizer reader `daemon/src/intake.rs:939`; representative integration readers `daemon/tests/intake.rs:667`, `daemon/tests/tick_integration.rs:2105`, `daemon/tests/tick_integration.rs:14419`.
- `ExistingPrIntake`: definition `daemon/src/intake.rs:405`; outcome container `daemon/src/intake.rs:127`; write sites `daemon/src/intake.rs:1095`, `daemon/src/intake.rs:1159`; legacy wrapper `daemon/src/intake.rs:1349`; slow-tick reader `daemon/src/tick.rs:1716`.
- `same_repo_pr`, `is_cross_repository`, `head_repo_full_name`, `head_repo_owner_login`: guard `daemon/src/intake.rs:552`; guard call `daemon/src/intake.rs:1005`; adapter derivation `daemon/src/adapters.rs:1240`; data fields `daemon/src/tools.rs:120`; tests `daemon/tests/intake.rs:667`, `daemon/tests/tick_integration.rs:2114`, `daemon/tests/tick_integration.rs:14428`.
- `external_ref`: field owner `daemon/src/tools.rs:118`; canonical parser/normalizer `daemon/src/intake.rs:484`, `daemon/src/intake.rs:497`; adapter writer `daemon/src/adapters.rs:1264`; adoption handoff `daemon/src/intake.rs:1099`; repo derivation `daemon/src/tick.rs:1794`; drive-branch parser `daemon/src/tick.rs:6446`.
- `head_ref_name`, `head_sha`, `pr_number`, `branch`: intake declaration `daemon/src/intake.rs:405`; adoption writes `daemon/src/tick.rs:1802`; telemetry writer `daemon/src/tick.rs:1849`; overlay fields `daemon/src/state.rs:95`; drive resolver `daemon/src/tick.rs:6460`; dispatch branch selection `daemon/src/dispatch.rs:445`.

### Repo-target terms

- `target_repo`: config declaration `daemon/src/config.rs:29`; production value `config/daemon.toml:1`; routing map `config/daemon.toml:17`; body parser/resolver `daemon/src/intake.rs:459`, `daemon/src/intake.rs:534`; adoption write `daemon/src/tick.rs:1794`; ordinary/manual intake writes `daemon/src/tick.rs:1883`, `daemon/src/tick.rs:2013`; durable accessor `daemon/src/state.rs:178`; SQL contract/write `daemon/contracts/schema.sql:81`, `daemon/src/state.rs:1612`; dispatch read/recovery `daemon/src/dispatch.rs:265`, `daemon/src/dispatch.rs:350`; dispatch telemetry `daemon/src/tick.rs:2977`; shell writer `daemon/factory-intake-from-gh.sh:186`; shell reader `daemon/factory-af-tick.sh:313`.
- `RepoConfig`, `RepoRouting`, `resolve_repo`, `ao_project`, `push_remote`, `local_checkout`: declarations `daemon/src/config.rs:5`, `daemon/src/config.rs:19`; resolver `daemon/src/config.rs:238`; checkout eligibility `daemon/src/config.rs:292`; production config `config/daemon.toml:17`; dispatch consumer `daemon/src/dispatch.rs:355`; spawn-spec construction `daemon/src/dispatch.rs:648`; CLI consumer `daemon/src/adapters.rs:3290`.
- `BeadOverlay.target_repo`, `repo(cfg)`: owner/accessor `daemon/src/state.rs:145`, `daemon/src/state.rs:178`; initial adopted write `daemon/src/tick.rs:1794`; ready-stage read `daemon/src/tick.rs:2293`; dispatch read `daemon/src/dispatch.rs:294`; state-store write/read `daemon/src/state.rs:1570`, `daemon/src/state.rs:1609`.
- `target drift`, `target_drift`, `adopted.*target_repo`: no production symbol or literal hit. Related but distinct “drift” guards are pre-gate PR/branch drift (`daemon/src/config.rs:80`), checkout drift in the legacy shell tick (`daemon/factory-af-tick.sh:126`), branch/revision drift in the AO bridge (`daemon/scripts/ao-spawn-v013-bridge.mjs:220`), and post-spawn remote mismatch (`daemon/src/dispatch.rs:905`).

### Drive-field terms

- `existing_pr:` / `existing_branch:` protocol owners: `AGENTS.md:69`, `CLAUDE.md:68`, `.claude/skills/auto-factory/SKILL.md:142`, `docs/multirepo-dispatch-investigation-2026-07-11.md:20`.
- Shell production writer/reader: `daemon/factory-intake-from-gh.sh:186` writes all three fields; `.claude/skills/auto-factory/SKILL.md:137` describes reading all three.
- Rust references are comments/test data, not parsers: `daemon/src/intake.rs:459`, `daemon/src/intake.rs:534`, `daemon/src/intake.rs:1604`, `daemon/src/dispatch.rs:3288`, `daemon/tests/tick_integration.rs:3769`, `daemon/tests/tick_integration.rs:3925`. Rust drive binding actually reads `Bead.external_ref`, resolved repo, and live SCM state at `daemon/src/tick.rs:6418`.

### Adopted state and dispatch boundary terms

- `is_adopted`: declaration `daemon/src/state.rs:98`; SQL owner `daemon/contracts/schema.sql:33`; adoption writer `daemon/src/tick.rs:1805`, `daemon/src/tick.rs:1823`; dispatch branch reader/writer `daemon/src/dispatch.rs:459`, `daemon/src/dispatch.rs:562`; persistence `daemon/src/state.rs:1612`; remediation reader `daemon/src/reroll.rs:1279`.
- `DriveBranchDecision` / `PrHeadBranch`: declarations `daemon/src/dispatch.rs:77`, `daemon/src/tools.rs:694`; ready writer `daemon/src/tick.rs:2284`; resolver `daemon/src/tick.rs:6418`; dispatch reader `daemon/src/dispatch.rs:445`; SCM adapter `daemon/src/adapters.rs:2310`; tests `daemon/tests/tick_integration.rs:3750`.
- `SpawnSpec`, `Sessions::spawn`, `ao spawn`: contract `daemon/src/tools.rs:236`; trait reader `daemon/src/tools.rs:753`; construction/call `daemon/src/dispatch.rs:648`, `daemon/src/dispatch.rs:668`; production adapter `daemon/src/adapters.rs:3290`, `daemon/src/adapters.rs:6319`; argv/env writer `daemon/src/adapters.rs:2990`, `daemon/src/adapters.rs:3020`; bridge parser/final consumer `daemon/scripts/ao-spawn-v013-bridge.mjs:44`, `daemon/scripts/ao-spawn-v013-bridge.mjs:606`.
- Existing pre-spawn inputs/flags: `SpawnSpec.repo`, `ao_project`, `remote`, `local_checkout`, `expected_revision`, `managed_checkout`, `expected_cwd` at `daemon/src/tools.rs:248`; `--project`/`--agent` at `daemon/src/adapters.rs:3022`; `DARK_FACTORY_AO_TARGET_CHECKOUT`, `DARK_FACTORY_AO_MANAGED_CHECKOUT`, `DARK_FACTORY_AO_EXPECTED_REVISION`, `DARK_FACTORY_AO_SPAWN_BRANCH` at `daemon/src/adapters.rs:2998`, `daemon/src/adapters.rs:3006`, `daemon/src/adapters.rs:3011`, `daemon/src/adapters.rs:3033`; bridge readers `daemon/scripts/ao-spawn-v013-bridge.mjs:123`, `daemon/scripts/ao-spawn-v013-bridge.mjs:126`, `daemon/scripts/ao-spawn-v013-bridge.mjs:141`.
- Existing rejection phases/reasons: `unmapped_repo` `daemon/src/dispatch.rs:316`; `unmapped_target_repo` `daemon/src/dispatch.rs:350`; `target_checkout_unconfigured` `daemon/src/dispatch.rs:389`; `spawn_failed` `daemon/src/dispatch.rs:793`; `spawn_branch_mismatch` `daemon/src/dispatch.rs:820`; `worktree_remote_unverifiable` `daemon/src/dispatch.rs:866`; `worktree_remote_mismatch` `daemon/src/dispatch.rs:905`; reason enum/string owners `daemon/src/state.rs:618`, `daemon/src/state.rs:713`; tick phase readers `daemon/src/tick.rs:2518`, `daemon/src/tick.rs:2656`, `daemon/src/tick.rs:2805`.

### Event/state terms

- `EXISTING_PR_ADOPTED`: writer `daemon/src/tick.rs:1842`; dedup reader `daemon/src/tick.rs:677`; origin-classification reader `runner/funnel_lanes.py:152`; tests `daemon/tests/tick_integration.rs:2183`, `daemon/tests/tick_integration.rs:14485`.
- `TASK_ROUTED`: writer `daemon/src/tick.rs:2273`; recovery/funnel test reader `tests/test_funnel_lanes.py:296`.
- `TASK_DISPATCHED`: writer `daemon/src/tick.rs:2965`; audit reader `daemon/scripts/fe_audit_query.py:108`; funnel reader through stage sets `runner/funnel_lanes.py:54`; integration reader `daemon/tests/tick_integration.rs:3835`.
- `PARKED_HUMAN_HELD`: writers for relevant fail-closed paths `daemon/src/tick.rs:2075`, `daemon/src/tick.rs:2538`, `daemon/src/tick.rs:2673`; funnel reader `runner/funnel_lanes.py:17`; tests `daemon/tests/tick_integration.rs:4307`, `daemon/tests/tick_integration.rs:11296`.
- `ESCALATION_REQUIRED`: relevant writers `daemon/src/tick.rs:1754`, `daemon/src/tick.rs:2129`, `daemon/src/tick.rs:2636`; funnel reader `runner/funnel_lanes.py:19`; tests `daemon/tests/tick_integration.rs:14622`.
- `BEAD_DISPATCH_TRANSIENT_ERROR`: fallback writer `daemon/src/tick.rs:2949`; audit reader matches `TRANSIENT_ERROR` at `daemon/scripts/fe_audit_query.py:94`; integration reader `daemon/tests/tick_integration.rs:549`.
- Telemetry keys `eventType`, `beadId`, `attemptId`, `lifecycleState`, `metrics`, `context`: Rust schema `daemon/src/telemetry.rs:6`; JSONL consumer `runner/funnel_lanes.py:61`; schema test `daemon/src/telemetry.rs:91`.

## Writers / readers table

| Concept | Writers / owners | Readers / consumers |
|---|---|---|
| PR source repo | `CliScm` constructs `external_ref`, head repo metadata, and cross-repo flag (`daemon/src/adapters.rs:1240`) | `same_repo_pr` and normalizer (`daemon/src/intake.rs:552`, `daemon/src/intake.rs:1005`) |
| `ExistingPrIntake.repo` | Normalizer copies the swept repo (`daemon/src/intake.rs:1095`, `daemon/src/intake.rs:1159`) | Slow-tick collision/adoption telemetry reads it (`daemon/src/tick.rs:1716`, `daemon/src/tick.rs:1731`, `daemon/src/tick.rs:1849`) |
| Canonical `external_ref` | Adapter and canonicalizer (`daemon/src/adapters.rs:1264`, `daemon/src/intake.rs:514`) | Dedup, repo resolution, drive binding (`daemon/src/intake.rs:1091`, `daemon/src/intake.rs:475`, `daemon/src/tick.rs:6446`) |
| `target_repo:` body field | Human/shell intake (`.claude/skills/auto-factory/SKILL.md:137`, `daemon/factory-intake-from-gh.sh:186`) | Rust `resolve_target_repo` (`daemon/src/intake.rs:475`) and shell dispatch query (`daemon/factory-af-tick.sh:412`) |
| Overlay `target_repo` | Adoption/manual/issue intake and dispatch recovery (`daemon/src/tick.rs:1794`, `daemon/src/tick.rs:1883`, `daemon/src/tick.rs:2013`, `daemon/src/dispatch.rs:294`) | `BeadOverlay::repo`, routing, telemetry (`daemon/src/state.rs:178`, `daemon/src/dispatch.rs:355`, `daemon/src/tick.rs:2977`) |
| `is_adopted` | PR adoption and PR-head dispatch (`daemon/src/tick.rs:1823`, `daemon/src/dispatch.rs:562`) | Branch reuse and append-only reroll (`daemon/src/dispatch.rs:474`, `daemon/src/reroll.rs:1279`) |
| `pr_number` / `branch` | Adoption writes (`daemon/src/tick.rs:1812`) and dispatch writes (`daemon/src/dispatch.rs:550`) | Gate/drive/remediation paths (`daemon/src/tick.rs:6460`, `daemon/src/reroll.rs:1279`) |
| Repo routing | Config/TOML (`config/daemon.toml:17`; `daemon/src/config.rs:249`) | Dispatch prompt/spec and AO command (`daemon/src/dispatch.rs:355`, `daemon/src/dispatch.rs:625`, `daemon/src/adapters.rs:3022`) |
| `DriveBranchDecision` | Tick live-PR resolver (`daemon/src/tick.rs:6418`) | Dispatch branch selector (`daemon/src/dispatch.rs:452`) |
| `SpawnSpec.repo` and AO target facts | Dispatch (`daemon/src/dispatch.rs:648`) | Adapter command/worktree verification and bridge (`daemon/src/adapters.rs:2990`, `daemon/src/adapters.rs:3333`, `daemon/scripts/ao-spawn-v013-bridge.mjs:120`) |
| Dispatch failure phase | Dispatch helpers/guards (`daemon/src/dispatch.rs:145`, `daemon/src/dispatch.rs:350`, `daemon/src/dispatch.rs:905`) | Tick telemetry/escalation classifier (`daemon/src/tick.rs:2380`, `daemon/src/tick.rs:2656`, `daemon/src/tick.rs:2805`) |
| `park_reason` | `set_human_hold_reason` and state persistence (`daemon/src/state.rs:782`, `daemon/src/state.rs:1618`) | Recovery allow-list and operators/tests (`daemon/src/state.rs:786`, `daemon/tests/tick_integration.rs:10743`) |
| Lifecycle telemetry | Tick's `emit` wrapper and telemetry writer (`daemon/src/tick.rs:1842`, `daemon/src/telemetry.rs:53`) | Funnel/audit tooling (`runner/funnel_lanes.py:110`, `daemon/scripts/fe_audit_query.py:94`) |
| Legacy drive fields | Shell normalizer (`daemon/factory-intake-from-gh.sh:186`) | Shell `/af` skill/dispatcher (`.claude/skills/auto-factory/SKILL.md:137`, `daemon/factory-af-tick.sh:313`) |

## Open questions

- **What exact operands define “adopted-PR target drift”?** The visible code exposes at least three repo identities: `ExistingPrIntake.repo`, the repo prefix of `external_ref`, and persisted `BeadOverlay.target_repo`; dispatch adds `SpawnSpec.repo`/AO project. No visible goal text specifies which mismatch is the RED case.
- **Where must rejection occur?** “Before AO spawn” could mean the slow-tick adoption loop, queued-to-ready construction, `dispatch_ready_with_vcs` immediately before `sessions.spawn`, or the AO bridge before `sessions.spawn`. All exist and have different state/telemetry ownership.
- **What should happen to an already-`ATTESTED` adopted PR?** Initial labeled-PR adoption intentionally verifies without spawning (`daemon/tests/tick_integration.rs:2105`). AO spawn normally happens only after a failed gate enters adopted remediation. The requested state transition and retry policy are not named in visible code.
- **No current `target_drift` owner/reader exists.** There is no `target_drift` field, flag, phase, `HumanHoldReason`, or event type. Under the rung-2 gate it is not a concept inventory item. The plan should decide whether to reuse `PARKED_HUMAN_HELD` plus a new/existing reason, and must create both a writer and a visible reader/test if it introduces a name.
- **No Rust production reader exists for `existing_pr:` or `existing_branch:` body lines.** Rust drive mode is derived from `external_ref` plus live SCM lookup. Treating those strings as Rust state would be a YAGNI trap unless the plan explicitly changes the contract.
- **`ExistingPrIntake.repo` is not persisted directly.** The adoption loop recomputes `target_repo` from `adopted.external_ref` (`daemon/src/tick.rs:1794`) rather than copying or comparing `adopted.repo`. Whether that recomputation is the intended drift seam is not stated.
- **Initial adopted `head_sha` is telemetry/cache data, not the overlay target revision.** `pre_session_head_sha` is populated later around dispatch/remediation. The plan must not conflate repository-target drift with branch-head/revision drift already guarded by the AO bridge.
- **Case sensitivity is inconsistent by role.** Same-repo PR checks use case-insensitive GitHub identity (`daemon/src/intake.rs:556`), while `resolve_drive_pr_head_branch` uses direct string inequality (`daemon/src/tick.rs:6451`) and config lookup is exact-keyed (`daemon/src/config.rs:249`). The desired equality semantics are not specified.
- **Rust and shell `/af` routes coexist in the visible repo.** The goal does not state whether acceptance covers only the Rust daemon (the route containing `ExistingPrIntake`/`SpawnSpec`) or also `factory-af-tick.sh` → `factory-ao-remediate.sh`.
- **Telemetry consumer contract is unspecified.** Existing generic event types have visible readers. A dedicated rejection event/reason would need a reader/test; otherwise it is an unowned YAGNI concept.

## Reuse

# Reuse and centralization exploration: adopted-PR target drift

Goal: `[P0][factory][RED] Reject adopted-PR target drift before AO spawn (jleechanorg/worldarchitect.ai)`

Lens: Ponytail rung 2 first (does it already exist?), then stdlib, platform,
installed dependencies, and the one-line test. Search scope was the visible
repository only; no sealed holdout or evaluator content was consulted.

## Reuse candidates

### Rung 2 — existing code to extend

- `daemon/src/tick.rs:6438` — `resolve_drive_pr_head_branch` is the existing
  pre-spawn decision seam with access to the bead, resolved repo, config, and
  live SCM. Extend this seam (or a narrowly extracted authority immediately
  beside it) for adopted-target validation instead of adding a second PR probe
  in `dispatch.rs` or `adapters.rs`.
- `daemon/src/tick.rs:6451` — the repo-consistency comparison already exists:
  an `external_ref` repo unequal to the resolved overlay repo is detected before
  the ready item reaches dispatch. The missing behavior is its adopted-only
  consequence: it currently returns `Generated`, but an adopted target must be
  rejected/parked.
- `daemon/src/tools.rs:637` — `Scm::open_pr_head_ref_for_repo` is the canonical
  repo-scoped live-PR capability. Reuse it for the last responsible-moment
  check rather than introducing a direct `gh` subprocess at the call site.
- `daemon/src/tools.rs:694` — `PrHeadBranch` already models the relevant live
  states (`SameRepo(head)`, `Fork`, `NotFound`) as a typed result. Extend its
  contract only if the implementation needs to distinguish closed/missing from
  lookup failure; do not replace it with strings or booleans.
- `daemon/src/adapters.rs:2317` — `CliScm::open_pr_head_ref_for_repo` already
  issues the single GitHub REST lookup `repos/{repo}/pulls/{pr}`. The upcoming
  implementation should enrich/fix this adapter's result semantics, not add a
  second endpoint wrapper.
- `daemon/src/adapters.rs:2724` — `parse_open_pr_head_ref` is the pure,
  unit-testable JSON parsing seam for PR state, head ref, and same-repo status.
  Extend its deserialized view if target identity needs more fields; keep
  subprocess-free cases here.
- `daemon/src/dispatch.rs:87` — `DriveBranchDecision` is already the typed value
  passed from the SCM-owning tick layer into the SCM-free dispatcher. A
  rejection outcome (or a sibling typed pre-spawn verdict) belongs in this
  existing handoff, avoiding recomputation after routing.
- `daemon/src/dispatch.rs:201` — `dispatch_ready` is the single AO admission
  boundary and already consumes caller-resolved decisions. Preserve its
  deliberate lack of `Scm`; it should consume a validated target or refusal,
  never query GitHub itself.
- `daemon/src/dispatch.rs:648` — `SpawnSpec` construction is the final point at
  which validated `repo`, `branch`, checkout, revision, and remote become the AO
  request. Make those fields projections of one validated adopted target rather
  than independently derived values.
- `daemon/src/state.rs:178` — `BeadOverlay::repo` is the current single accessor
  for persisted per-bead repo identity. Reuse it for ordinary routing, while
  requiring a non-fallback explicit repo for adopted-PR validation.
- `daemon/src/state.rs:618` — `HumanHoldReason` plus
  `set_human_hold_reason` (`daemon/src/state.rs:782`) is the canonical durable
  park policy. Add any target-drift reason here so recovery classification,
  SQLite state, and telemetry use one stable value.
- `daemon/src/dispatch.rs:502` — the pre-spawn branch registration conflict path
  is the closest failure pattern: reject one bead, persist `HUMAN_HELD`, report
  a stable phase, continue unrelated dispatches, and never call `Sessions::spawn`.
  Reuse this control-flow pattern, not its reason.
- `daemon/src/config.rs:249` — `Config::resolve_repo` is the sole repo-to-AO
  project/remote/checkout routing authority. Validate adopted identity before
  calling it, then reuse it unchanged to produce execution routing.
- `daemon/src/intake.rs:405` — `ExistingPrIntake` already carries the adoption
  snapshot `(repo, pr_number, head_ref_name, head_sha)`. This is the best source
  to initialize immutable adopted-target identity; avoid re-parsing the PR body
  during adoption.
- `daemon/src/tick.rs:1794` — the labeled-PR adoption path already persists
  `target_repo`, `pr_number`, `branch`, and `is_adopted`. Centralize their write
  as one adopted-target assignment/invariant rather than letting later call
  sites update the fields independently.
- `daemon/src/tools.rs:491` — `parse_external_ref_repo` is already public and
  used by repo-scoped SCM filtering. It is a partial centralization candidate,
  but should become (or delegate to) one strict typed external-ref parser that
  also returns the numeric issue/PR id.
- `daemon/src/intake.rs:475` — `resolve_target_repo` is the current authority for
  manual/general bead routing precedence. Reuse it only for non-adopted intake
  and legacy recovery; adopted PR identity should come from the adopted PR's
  canonical external ref/live SCM record, not mutable body precedence.
- `daemon/src/dispatch.rs:866` — the just-spawned worktree remote check is good
  defense in depth after AO returns. Keep it, but do not treat it as satisfying
  the requested pre-spawn target-drift rejection.
- `daemon/src/tools.rs:428` — `check_cwd_guard` and
  `daemon/src/target_worktree.rs:106`'s target-worktree validation already own
  filesystem/worktree containment. Reuse those checks after logical PR target
  validation; do not fold GitHub PR identity into filesystem helpers.
- `daemon/src/tick.rs:4779` — the pre-gate PR/head drift validation demonstrates
  the existing live-check and telemetry style. Its parser/probe can be shared,
  but its post-spawn re-resolution policy must not be copied for adopted
  dispatch (see anti-reuse traps).

Search evidence included `rg` passes for `adopted`, `existing_pr`,
`target_repo`, `open_pr_head_ref_for_repo`, `DriveBranchDecision`,
`parse_external_ref`, `HumanHoldReason`, `Sessions::spawn`, `baseRefName`, and
`target drift` across `daemon/src`, `daemon/tests`, visible docs, and config.

### Rung 3 — stdlib before hand-roll

- `str::split_once('#')` can replace the repeated `split('#').collect::<Vec<_>>()`
  parsers at `daemon/src/tools.rs:491`, `daemon/src/intake.rs:488`,
  `daemon/src/adapters.rs:620`, and `daemon/src/tick.rs:6405`. It expresses the
  strict delimiter operation without allocation; an additional `contains('#')`
  check preserves the “exactly one separator” rule.
- `Option`, `Result`, and an enum are sufficient for the validation pipeline;
  no registry, callback framework, or new module graph is needed.
- `str::eq_ignore_ascii_case` is already used for GitHub repo identity at
  `daemon/src/intake.rs:557` and `daemon/src/adapters.rs:2753`; reuse that
  normalization rather than lowercasing ad hoc at each comparison.
- `std::path::Path::canonicalize` remains the correct stdlib primitive for
  filesystem identity, but it is not a substitute for SCM target validation.

### Rung 4 — native platform features

- GitHub's existing pull-request REST resource (`repos/{repo}/pulls/{pr}`),
  already called by `CliScm`, is the native source for open state, head repo,
  and head branch. Extend the existing serde view instead of composing multiple
  `gh pr view`/`gh pr list` calls.
- Rust's exhaustively matched enums are the native fail-closed mechanism here:
  add an explicit rejected/drifted state and force every caller/test fake to
  handle it, instead of interpreting error strings or keywords.
- The existing AO `SpawnSpec` boundary is the native admission point. A ready
  item should reach it only after target validation; no AO wrapper or new
  scheduler layer is warranted.
- Existing structured telemetry (`DispatchFailure.phase`, typed park reasons,
  and `emit`) should project the decision. Do not create a parallel log or
  drift ledger.

### Rung 5 — already-installed dependencies

- `serde`/`serde_json` (already in `daemon/Cargo.toml`) cover any extra fields
  needed from the existing PR JSON response and keep parsing in
  `parse_open_pr_head_ref` testable.
- `thiserror` already owns `DaemonError`; use it only if target drift must cross
  an error-returning boundary. Prefer a domain verdict for an expected policy
  refusal, reserving `DaemonError` for tool/config failures.
- `rusqlite` already persists overlay state and park reasons through
  `StateStore`; no new persistence dependency or cache is justified.

### Rung 6 — can this be one line?

- Yes: external-ref parsing can centralize to one strict helper based on
  `split_once`, replacing four allocation-heavy copies. This is a clear
  centralization win and should delete code overall.
- Yes: the core consistency predicate is conceptually one conjunction — adopted
  `(repo, pr, branch)` equals explicit/canonical bead identity and the live
  same-repo open PR head. Keep that comparison small, but return a typed verdict
  so mismatch and unverifiable are not collapsed.
- Yes: `SpawnSpec.repo` and `.branch` should be direct projections from one
  validated target value. If their construction still needs separate fallback
  chains, authority remains duplicated.
- No: parking, telemetry, and cleanup are not safely one line because persistence
  failure and per-bead batch isolation are established contracts. Reuse the
  existing dispatch failure pattern rather than abstracting it prematurely.

## Centralization proposal

The single authority should live at the existing slow-tier pre-spawn seam,
beside `tick::resolve_drive_pr_head_branch`, and produce one typed adopted-target
verdict before the item is appended to `ready`.

Suggested authority shape (name illustrative, not a mandate):

```text
AdoptedPrTarget { repo, pr_number, head_branch }
        |
        +-- validate persisted adoption provenance
        +-- validate canonical external_ref identity
        +-- validate live OPEN + same-repo head via Scm
        |
        +-- Valid(target) ------> routing + SpawnSpec projections
        +-- Drift(details) -----> durable HUMAN_HELD, no Sessions::spawn
        +-- Unverifiable(error) -> defer or durable fail-closed policy, no spawn
```

Why this location:

1. `tick.rs` already owns `Scm` access and constructs the ready tuple.
2. `dispatch.rs` intentionally owns deterministic admission/state transitions
   without SCM access.
3. The decision occurs before branch registration, `DISPATCHING`, revision
   resolution, prompt construction, and AO spawn, satisfying the requested
   boundary with the smallest mutation surface.
4. The same validated value can populate branch mode, routing lookup, telemetry,
   and `SpawnSpec`; downstream consumers become projections instead of rival
   authorities.

Highest-value companion centralization: replace all strict short-form
external-ref parsers with one typed `ExternalRef { repo, number }` parser in the
shared low-level `tools` layer. `resolve_target_repo`, tracker comment routing,
SCM filtering, drive-PR detection, and dispatch's PR-number extraction should
delegate to it. Keep the special corrupted `#local-*` recovery at
`daemon/src/adapters.rs:704` as an explicit caller-side repair, not part of the
canonical parser.

Do not create a generic “identity utils” module. The useful authority is the
domain contract (adopted PR target), not a bag of normalization functions.

## Migration notes

- `BeadOverlay.target_repo`, `.pr_number`, `.branch`, and `.is_adopted` should
  be written/read as one adopted-target projection. Legacy rows can be
  reconstructed once from `external_ref` plus a positive live PR probe; if that
  cannot be proven, do not fall back to global config for an adopted spawn.
- `resolve_target_repo(body, external_ref)` remains the general/manual intake
  projection. For adopted PRs, mutable `target_repo:` body text must not override
  the canonical adopted PR repo. A conflict becomes drift, not precedence.
- `DriveBranchDecision::PrHead` becomes a projection of a validated adopted
  target. `Generated` remains valid for ordinary work only; it must not silently
  downgrade an adopted mismatch or unavailable live probe.
- `Config::resolve_repo`, `target_worktree_path`, and `RepoRouting` remain
  execution projections from the validated repo. They should not decide which
  repo the adopted PR belongs to.
- `SpawnSpec.repo`, `.branch`, `.remote`, `.ao_project`, `.local_checkout`, and
  `.expected_revision` remain transport projections. AO adapters must not
  reinterpret bead bodies or external refs.
- Post-spawn `session_branch`, worktree-remote, cwd, and target-worktree checks
  remain defense-in-depth projections of the already validated target. Retain
  their cleanup behavior for TOCTOU/adapter defects.
- Existing pre-gate re-resolution remains a gate-assessment repair mechanism for
  factory-created work. Adopted PR identity should not be silently rebound to a
  different PR after dispatch; surface a dedicated immutable-target failure.
- Tests should extend the current `FakeScm` scripting in
  `daemon/tests/common/mod.rs:357` and dispatch spawn call logs. The decisive
  assertion is `Sessions::spawn` call count remains zero on repo, PR, head-branch,
  fork, closed/missing, and chosen unverifiable cases, while unrelated ready
  beads still proceed according to the established batch-isolation contract.

## Anti-reuse traps

- `tick::resolve_drive_pr_head_branch` looks almost sufficient, but its current
  `NotFound | Err -> Generated` behavior (`daemon/src/tick.rs:6463`) is unsafe
  to reuse unchanged for adopted PRs. It converts uncertainty/drift into new-work
  dispatch instead of rejecting before AO.
- `CliScm::open_pr_head_ref_for_repo` swallows every `run_tool` error into
  `PrHeadBranch::NotFound` (`daemon/src/adapters.rs:2318`). Reusing that exact
  error collapse would make a GitHub outage indistinguishable from a closed or
  missing PR. Fix the contract or add a strict sibling capability; do not infer
  safety from `NotFound`.
- `resolve_target_repo` deliberately lets body `target_repo:` override
  `external_ref` (`daemon/src/intake.rs:475`). That is an established general
  routing policy, but extending it to immutable adopted PR identity entrenches
  the drift vector the goal is meant to close.
- `BeadOverlay::repo` falls back to global `cfg.target_repo` when the persisted
  repo is absent (`daemon/src/state.rs:188`). Backward compatibility is useful
  for ordinary legacy beads, but an adopted spawn requires positive identity;
  a default is not proof.
- `PrHeadBranch::SameRepo(String)` proves live open/same-repo head identity, but
  it does not by itself prove it matches the persisted adopted branch. The
  caller must compare it to the adoption snapshot.
- The pre-gate drift code at `daemon/src/tick.rs:4812` deliberately re-resolves
  a different PR by branch. That is too late (after spawn) and its rebinding
  semantics are wrong for an immutable adopted target.
- The worktree remote check at `daemon/src/dispatch.rs:866`, session branch check
  at `daemon/src/dispatch.rs:820`, and cwd guard are valuable but occur during
  or after spawn. They cannot replace the requested pre-AO rejection.
- `Config.base_branch` and `Vcs::base_head_for_repo` describe the factory's
  configured execution baseline, not necessarily the adopted PR's immutable
  target. Do not use them as a proxy for live PR target identity.
- The four current external-ref parsers are not safely interchangeable:
  `adapters.rs` accepts full GitHub URLs and contains a narrow corruption repair,
  while the others are strict short-form parsers. Centralize the strict grammar
  first and preserve URL/corruption normalization as explicit adapters.
- A generic helper that accepts optional repo/branch/PR fields and silently
  skips missing dimensions would recreate the wart in abstract form. Adopted
  validation should require all identity dimensions or return an explicit
  unverifiable/refused verdict.
- Do not add a new dependency. Rungs 2–4 already cover the feature completely.

## Risks

# Risks and invariants: reject adopted-PR target drift before AO spawn

Scope: the Stage-2 adopted-remediation path from PR intake through `reroll::execute_adopted`. This review uses visible source and tests only. The root trust-boundary gap is that adoption records PR identity in several independent fields (`pr_number`, `branch`, `is_adopted`, `target_repo`), while `execute_adopted` treats `BeadOverlay::repo()` as authoritative without proving that it still names the repository containing the adopted PR.

## Edge cases

- **An existing overlay can retain a repo that disagrees with the PR being adopted.** In `tick::run_slow_tier`, the `should_adopt` block derives `target_repo` from `adopted.external_ref` only for the `unwrap_or(BeadOverlay { ... })` constructor. If the bead already has an overlay, the code updates `state`, `pr_number`, `branch`, and `is_adopted` but leaves `overlay.target_repo` untouched. `reroll::execute_adopted` later calls `bead.repo(cfg)` and builds `SpawnSpec.repo`, `ao_project`, checkout, and remote from that stale field. Failure surface: AO can be spawned in a valid checkout for repo A while the adopted `(PR, branch)` came from repo B. Existing checks: `test_non_default_repository_labeled_pr_tick_telemetry_attribution` proves attribution for a newly created overlay; no test seeds an existing mismatched overlay and proves zero spawn.

- **The remote-head baseline is itself process-global, not bead-scoped.** `execute_adopted` calls `deps.vcs.remote_head_sha(&branch)`. The production `CliVcs::remote_head_sha` queries `self.target_repo`, fixed when `CliVcs` is constructed, even though `CliVcs::with_repo` and repo-scoped VCS methods already exist. A non-default adopted bead can therefore capture a SHA from the daemon default repo; if the same branch name exists there, the call succeeds and produces a plausible but wrong `expected_revision`. Failure surface: wrong-repo baseline, false checkout failure, or—if SHAs happen to coincide—misdirected spawn. Existing checks: `cli_vcs_gh_tests::remote_head_sha_targets_configured_repo_and_preserves_branch_slash` checks only the adapter's configured repo; `test_reroll_adopted_unconfigured_repo_uses_daemon_owned_target_worktree` uses a branch-only `FakeVcs` and cannot detect this cross-repo error. No end-to-end check covers a routed adopted repo different from `CliVcs.target_repo`.

- **PR identity can change between adoption and remediation.** The contributor can close/merge the PR, delete or rename the branch, or open another PR from the same branch before a later RED gate invokes remediation. `execute_adopted` validates only that `bead.branch` is present and that a branch SHA can be read; it never re-confirms that `bead.pr_number` is currently open, same-repo, and bound to that branch. `Scm::open_pr_head_ref_for_repo` already expresses the relevant live check, but is used by drive-PR dispatch rather than adopted reroll. Failure surface: a worker changes an orphan branch or a branch now associated with a different PR. Existing checks: `parse_open_pr_head_ref` unit tests cover open/same-repo, fork, deleted-fork, closed, and malformed shapes; no adopted-reroll test invokes that check immediately before spawn.

- **A live push can race the pre-spawn snapshot.** `execute_adopted` reads `pre_session_sha`, then persists DISPATCHING, then calls `Sessions::spawn`. The spawn adapter's `expected_revision` validation and AO bridge's origin-ref check correctly reject a checkout or remote ref that moved in that interval. Failure surface should be a hold, never an AO invocation on an unverified revision. Existing checks: `worker_spawn_rejects_stale_expected_revision_before_ao`, `bridge_fails_closed_when_adopted_origin_ref_head_diverges_from_expected_revision`, and target-worktree stale/dirty tests. Missing check: the full `execute_adopted` path does not assert the target tuple before this race guard, so it can strongly validate the wrong repo.

- **Repository identity has case and syntax edge cases.** Intake's `same_repo_pr` and repository sweep dedup compare GitHub repo names case-insensitively, while `Config::resolve_repo` is an exact map lookup and `BeadOverlay::repo` returns the stored string verbatim. A drift assertion that uses raw case-sensitive equality would reject an otherwise identical GitHub repository; conversely, trimming or case-folding must not accept malformed extra path components. Existing checks: intake tests cover case-insensitive same-repo and sweep dedup; `target_worktree::validate_repo` checks owner/repo shape. No shared canonical repo-identity assertion currently spans intake, state, config lookup, and reroll.

- **Legacy/null target state must not silently become proof.** `BeadOverlay::repo()` maps `target_repo=None` to `cfg.target_repo` for backward compatibility. That fallback is appropriate only when independent adopted-PR provenance proves the PR is in the default repo. Failure surface: a legacy adopted row with no target identity is spawned into the default by assumption. Existing checks: dispatch explicitly recovers/parks unresolved ordinary beads; no equivalent adopted-reroll test proves that a null target is reconciled against the live PR before spawn.

- **Unknown repo routing currently falls through in the adopted path.** `execute_adopted` uses `resolve_repo(...).unwrap_or_else(...)`, derives an AO project and `origin`, and permits a daemon-owned checkout for many repos absent from `[repos]`. Ordinary `dispatch_ready` instead parks an explicitly unmapped target. Failure surface: a drifted value can become a clone-and-spawn request instead of a loud routing error. Existing check: `test_reroll_adopted_unconfigured_repo_uses_daemon_owned_target_worktree` deliberately pins this behavior; there is no test distinguishing an intentionally supported default/managed repo from a contradictory adopted-PR target.

## Persistence risks

- **Adopted identity is denormalized without an atomic invariant.** `target_repo`, `pr_number`, `branch`, and `is_adopted` are columns on one overlay row, but `StateStore::save` accepts any combination and the adoption code mutates only three of the four. A crash is not required to create drift; an earlier persisted target survives a later adoption. Existing check: schema/load-save tests preserve each field, but no SQLite constraint or save-time assertion enforces a coherent adopted tuple.

- **Branch registration and adopted-overlay persistence are separate writes.** `run_slow_tier` calls `register_branch` before loading/mutating/saving the overlay. A process death or save failure can leave a branch registration pointing at a bead whose durable overlay does not record the adoption. On retry, collision logic can treat that registration as authoritative. Existing checks: branch-collision tests and same-bead idempotency checks cover normal calls; no fault-injection test covers failure between `register_branch` and adopted overlay save.

- **The pre-spawn intent is deliberately ambiguous and must stay durable.** `execute_adopted` saves `state=DISPATCHING`, clears `session_id`, and records `pre_session_head_sha` before crossing the AO boundary. Startup reconciliation must park this state rather than blindly spawn again. Existing checks: `adopted_spawn_crash_is_reconciled_without_duplicate_redispatch` and `AmbiguousDispatchingRecovery` state tests. A new target-drift rejection must occur before writing DISPATCHING, or persist a distinct permanent hold; otherwise a deterministic identity error is mislabeled as an ambiguous external-boundary crash.

- **Spawn success and remediation-marker persistence are correctly coupled only in SQLite.** Production `SqliteStateStore::save_remediation_session_spawned` writes the DISPATCHED overlay and marker in one `BEGIN IMMEDIATE` transaction. The trait default performs two separate writes, so alternate stores/fakes can observe a torn state. Existing checks: `adopted_remediation_marker_is_migrated_and_persistent`, `adopted_marker_persistence_failure_stops_worker_before_holding`, and spawn-cleanup tests. Any new persisted target assertion should not weaken this transaction or move the marker ahead of successful spawn.

- **Telemetry is not the source of truth.** `EXISTING_PR_ADOPTED` carries the correct `repo`, PR, branch, and head SHA from intake, but the durable overlay does not store that head SHA at adoption and may retain another repo. Log persistence or dedup cannot repair the state row used at spawn. Existing check: telemetry attribution/dedup tests. No check compares the emitted adoption tuple with the later `SpawnSpec` tuple.

- **Adoption-cache loss is safe only if it remains a performance failure.** `AdoptionProbeCache::persist` uses temp-file rename and tick treats failure as a warning; a lost/corrupt cache causes fresh probes, not reuse of unverifiable identity. Existing cache load/persist tests cover corruption/defaulting. A target binding must not be sourced solely from this best-effort cache.

## Concurrency risks

- **The freshness guard is load-then-save, not compare-and-swap.** `reroll::execute` loads an ATTESTED/RE_ROLL row and later saves RE_ROLL. Two daemon processes can both pass that read before either persists. `execute_adopted` then performs attach/quiescence and spawn as separate operations. AO may have its own reservation/dedup, but the state transition does not itself serialize the decision. Existing checks: duplicate-active-session and crash-reconciliation tests are sequential; no two-controller race test exists.

- **Duplicate-session reconciliation is branch-only at the trait boundary.** `Sessions::attach(&branch, &bead_id)` carries no repo or AO-project identity, while spawn is explicitly repo/project scoped. Identical branch names across repos can collide or reconcile against the wrong project depending on the adapter's construction-time project. `StateStore::register_branch` is also keyed by bare branch. Existing checks: branch collision tests protect against branch stealing within the current global key space; no test proves correct isolation of identical branch names across two repositories.

- **Idle/quiescent is not a finish-commit barrier.** In `tick::run_fast_tier`, an adopted worker is considered ready when `is_quiescent` is true, or when `session_activity` is Idle (the daemon then stops it), Terminal, or NotFound. The append-only check runs earlier while the overlay is DISPATCHED, then the overlay becomes ATTESTED and is no longer checked by that block. A final commit/push that races the idle observation or survives a failed stop can land after the last ancestry check. Failure surface: verification assesses an unstable head, or a late force-push escapes the adopted append-only guard. Existing checks: `test_dispatched_adopted_idle_session_reaped_and_promoted` pins prompt-finished promotion and `adopted_branch_history_rewrite_park_kills_associated_ao_session` pins detected rewrites; no test schedules a final push between the ancestry check and promotion.

- **Concurrent contributor pushes should defer/reject, not trigger refresh loops.** Managed target worktree refresh is serialized and refuses dirty trees; `expected_revision` pins a point-in-time checkout. Repeated branch movement can therefore cause repeated deterministic spawn failures and HUMAN_HELD transitions. Existing checks cover single stale/dirty cases; no test covers a moving adopted head across repeated recovery cycles or verifies escalation dedup prevents retry storms.

- **Failure cleanup must never create a second worker.** If AO returns a session but workspace/branch/revision validation fails, `CliSessions` kills it; if kill fails, the session id is preserved in a permanent hold. Existing checks: `adopted_spawn_failures_never_leave_an_untracked_or_recoverable_live_worker`, `worker_spawn_rejects_same_origin_stale_ao_workspace_after_spawn`, and spawn cleanup tests. The drift check should run before `Sessions::spawn` so this expensive post-spawn cleanup remains defense in depth, not the primary target validator.

## Invariants the design must preserve

- **No AO spawn until the complete adopted target tuple is positively bound:** live PR is open, queried repo equals the adopted repo identity, PR head repo is the same repo, and live head branch equals the stored adopted branch. Current check: none at `execute_adopted`; `Scm::open_pr_head_ref_for_repo` plus parser tests cover pieces elsewhere.

- **The repo used for live head lookup, `SpawnSpec.repo`, `SpawnSpec.ao_project`, checkout origin, push remote, expected revision, post-spawn workspace validation, and later append-only checks must be one repo.** Current checks: `worker_spawn_rejects_stale_expected_revision_before_ao`, routed checkout/remote tests, and bridge tests check internal consistency after a `SpawnSpec` exists; no test anchors that spec back to the adopted PR.

- **Target mismatch or inability to prove the target is fail-closed and pre-spawn.** It must persist a non-recoverable/operator-actionable hold (or leave a retryable state only for explicitly transient SCM failure), emit target/observed tuple telemetry, and call `Sessions::spawn` zero times. Current check: none for adopted target drift. Analogues exist for `unmapped_repo`, `unmapped_target_repo`, target checkout mismatch, and stale expected revision.

- **Same-repo/fork protection remains strict.** Never turn a fork PR's head branch name into a same-named base-repo branch. Current checks: intake fork tests, `parse_open_pr_head_ref` fork/deleted-fork tests, and drive-PR fallback tests.

- **Adopted remediation stays append-only and never closes/replaces the contributor PR.** Current checks: `test_reroll_adopted_success_spawns_remediation_session_leaves_pr_open`, `adopted_red_pr_stage2_reroll_spawns_remediation_session_leaves_pr_open`, remediation prompt test, append-only ancestry checks, and history-rewrite park tests.

- **A moved head between snapshot and spawn never reaches AO.** Current checks: stale expected-revision and adopted bridge divergence tests. Preserve exact revision matching; do not weaken it to "commit exists in repository" or an ancestry-only check.

- **Ambiguous external-boundary outcomes never auto-retry into a duplicate.** Current checks: `adopted_spawn_crash_is_reconciled_without_duplicate_redispatch`, duplicate-active-session tests, `SpawnCleanupFailed` tests, and `recover_human_held`'s session-null/reason allow-list.

- **Successful adopted spawn persists DISPATCHED state and the semantic remediation marker atomically before being treated as started.** Current checks: SQLite marker migration/transaction behavior and marker-persistence failure cleanup tests.

- **One bad bead does not starve the rest of a tick.** Target drift should become a per-bead held/deferred outcome with telemetry rather than an uncaught permanent error that aborts the fast tier. Current checks: per-candidate intake isolation and reroll permanent/transient error integration tests; no target-drift-specific batch test exists.

- **Repository comparison follows GitHub identity semantics without weakening path validation.** Case differences may compare equal, but empty values, extra `/` components, control/path syntax, and non-GitHub checkout remotes remain invalid. Current checks: case-insensitive intake tests, `remote_url_matches_repo` tests, and `target_worktree::validate_repo` tests; no single canonical comparison helper currently owns all uses.

- **No sealed-path or daemon-cwd fallback enters a worker spawn.** Current checks: holdout sandbox tests, missing-checkout tests, target-worktree origin checks, and AO cwd validation. A target-drift fix must reuse these layers rather than bypassing them with a direct AO call.

## Patch-trap warnings

- **Do not only overwrite `overlay.target_repo` during adoption.** That is a short diff, but it silently chooses one conflicting source and destroys evidence of drift. It also misses already-adopted rows and changes made between adoption and later remediation. The wart it entrenches is denormalized identity with last-writer-wins semantics. Prefer a pre-spawn assertion against live PR identity, with adoption-time coherence as an earlier defense.

- **Do not add a string comparison only inside `CliSessions::spawn`.** By then the PR identity is absent; the adapter can compare checkout origin only to the already-trusted `SpawnSpec.repo`. Passing a second "expected repo" string merely duplicates the same potentially stale value and creates a false two-source check.

- **Do not infer repo from branch names, AO project names, local checkout basenames, or the daemon cwd.** Branch names are not globally unique, AO project aliases intentionally differ (`worldarchitect.ai` -> `worldarchitect`), and basename/cwd confusion caused earlier cross-repo incidents. The wart would be another heuristic identity channel beside `external_ref`/live SCM.

- **Do not use `cfg.target_repo` as the comparator or VCS retargeting shortcut.** This fixes the single named incident only while breaking non-default `[repos.*]` adoption. The upstream cause is `execute_adopted`'s unscoped VCS call and unverified overlay tuple; use the bead/adopted PR's resolved repo consistently.

- **Do not treat `PrHeadBranch::NotFound` as a permanent mismatch without revisiting its contract.** The variant intentionally conflates closed/missing PRs with lookup/parse failures. A three-line match can park on a transient GitHub outage forever. Either the pre-spawn API must preserve transient `Err` distinctly or the caller must have an independently typed live-target probe.

- **Do not weaken `expected_revision` to tolerate a moving branch.** Resetting/fetching to the newest head inside spawn makes the worker's reviewed target non-deterministic and hides the race. A moved head should cause a bounded retry/reassessment with a newly captured tuple.

- **Do not solve controller races with a process-global mutex.** It would serialize unrelated repos/beads, fail across processes, and leave the SQLite load/save race intact. If multi-controller operation is supported, the reservation belongs in a durable compare-and-swap/transaction keyed by repo+branch or bead.

- **Do not key new locks or registrations by bare branch.** That preserves today's cross-repo collision wart. Any new target reservation should use canonical `(repo, branch)` identity while migration preserves existing registrations safely.

- **Do not make telemetry the validation mechanism.** Comparing a prior `EXISTING_PR_ADOPTED` log line at spawn time creates a second, best-effort database and fails after rotation/loss. Persist authoritative adoption identity in state or re-read it from live SCM.

- **Do not classify every mismatch as transient spawn failure.** Deterministic target drift will otherwise consume `spawn_failure_count`, enter recovery, and create a retry storm. It needs a distinct permanent hold reason and deduplicated escalation; only SCM unavailability should defer.

## Open risks

- **Authoritative target source is not explicit in the current state model.** The overlay does not retain the adopted `external_ref` or adoption-time repo/head tuple, and `RerollDeps` has no tracker bead payload. The implementation must decide whether live `(repo, pr_number)` comes from a new durable adoption field, a tracker lookup, or a richer SCM method. This cannot be resolved from `execute_adopted` alone.

- **`PrHeadBranch::NotFound` lacks enough error fidelity for retry policy.** Visible code documents that lookup/parse failure maps to NotFound. A safe design needs to distinguish definitive closed/missing/fork/mismatch from transient or malformed responses before choosing HUMAN_HELD versus retry.

- **Multi-daemon support is unclear.** The systemd deployment suggests one daemon, but SQLite and AO can be accessed by other processes. Without an explicit single-writer invariant, the non-CAS reroll reservation remains a real duplicate-spawn window.

- **The intended policy for unlisted production repos is inconsistent.** Ordinary dispatch parks unmapped repos; adopted reroll deliberately provisions a daemon-owned checkout for a repo absent from `[repos]`. The target-drift fix needs a product decision on whether live-PR proof is sufficient or explicit routing configuration is also mandatory.

- **Finish-commit semantics are weaker than the comments imply.** `Idle` is treated as finished and `stop()` errors are ignored in the promotion block. It is not clear whether AO guarantees that Idle cannot have a live descendant about to commit/push. Without that external guarantee, a final-head stability/ancestry barrier is still needed before ATTESTED.

- **Cross-repo branch registration/reconciliation migration has broader blast radius.** Correctly keying by `(repo, branch)` touches `StateStore`, schema, `Sessions::attach`, reaper/session lookup, and existing rows. It should not be smuggled into the target-drift patch unless acceptance criteria require multi-controller/cross-repo identical-branch concurrency now.

Summary: Ponytail verdict: the requested pre-AO fail-closed live-SCM check is real and should reuse `Scm::open_pr_head_ref_for_repo` plus existing typed hold/dispatch machinery with no new dependency or broader cross-repo/concurrency work; the partials disagree on the single shared placement (`execute_adopted` immediately before spawn versus the slow-tier seam beside `resolve_drive_pr_head_branch`) and this remains unresolved.
