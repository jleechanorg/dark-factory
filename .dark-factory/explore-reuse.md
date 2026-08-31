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
