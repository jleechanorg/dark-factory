# Multi-repo /af coder dispatch — investigation + fix spec (2026-07-11)

Operator directive: "the whole point is the coder is using /af — the factory
needs to work in any repo." Tonight's evidence: the /af daemon's intake,
routing, /er evidence loop, gate assessment, circuit breaker, and recovery all
work; the CODER delivery loop only works for the single configured repo, and
silently strands work for any other repo. This doc is the verified root-cause
chain and the implementation spec.

## Root-cause chain (each step verified live 2026-07-10/11)

1. **Config is single-repo, process-global.** `config/daemon.toml`:
   `target_repo = "jleechanorg/worldarchitect.ai"`, `ao_project =
   "worldarchitect"`. `Config` has exactly one of each (config.rs:6,8).
2. **Adapters bind the globals once.** main.rs:431-435 constructs
   `CliScm::new(cfg.target_repo)`, `CliVcs::new(cfg.target_repo)`,
   `CliSessions::new(&ao_project, &default_agent)` a single time. 27 call
   sites across tick.rs/intake.rs/reroll.rs/er_runner.rs/gates_compute.rs
   consume `cfg.target_repo` directly.
3. **Per-bead repo identity EXISTS but is dropped.** Beads carry
   `external_ref` (`owner/repo#N`), and the /auto-factory drive-existing-pr
   protocol defines body fields `target_repo:`/`existing_branch:`/
   `existing_pr:`. `BeadOverlay` (state.rs:65) has **no repo field** — repo
   identity is lost the moment a bead enters the overlay store.
4. **Every spawn lands in the one AO project.** Observed: dark-factory beads
   jleechan-{7t92,haux,kk64,l4ki} and jleechan-nil4 all dispatched into
   AO project `worldarchitect` → worktrees cloned from
   `~/projects/worldarchitect.ai`, whose remotes are `origin=jleechanclaw`,
   `worldai=worldarchitect.ai` (deliberate dual-remote). For a dark-factory
   bead NEITHER remote is correct; coders defaulting to `origin` drift
   toward jleechanclaw (near-miss wrong-repo PR from wa-3086; bead
   jleechan-9sh5).
5. **The watcher watches the wrong place.** The daemon's coder-silence
   detection and PR-detection look at `cfg.target_repo` — so even a coder
   that delivered to the right repo (if it weren't the configured one) would
   be invisible to verification: parked `coder_silent` (observed: nil4/wa-3089
   at 01:00:52Z). The failure is total, not partial: dispatch, delivery,
   detection, verification, and escalation all assume the global.
6. **AO itself is already multi-repo.** `~/.agent-orchestrator.yaml`
   registers `dark-factory → ~/projects/dark-factory` (plus worldarchitect,
   smartclaw, …). The capability exists one layer down; the daemon never
   passes anything but the global project name.

## Fix spec (stages, each independently shippable)

### Stage A — carry repo identity on the bead (schema + intake)
- `BeadOverlay` += `target_repo: Option<String>` (serde default `None` =
  legacy ⇒ `cfg.target_repo`). Single accessor
  `overlay.repo(cfg) -> &str` so call sites never re-implement the default.
- Intake sets it: from explicit body `target_repo:` field first (existing
  drive-existing-pr grammar), else from `external_ref`'s `owner/repo`
  prefix, else None. Manual `br` beads: same body-field parse.
- Telemetry: TASK_ROUTED/DISPATCHED context gains `target_repo`.

### Stage B — repo-parameterized adapters
- `Scm`/`Vcs` trait methods gain a repo parameter (or a `for_repo(&str)`
  factory returning a bound handle — smaller diff: keep traits, add
  `with_repo` constructor cloning the CLI adapter with a different repo
  string; construction is cheap, no state).
- `SpawnSpec` += `ao_project: String`, `remote: String`, `repo: String`.
- Config += `[repos]` table:
  ```toml
  [repos."jleechanorg/worldarchitect.ai"]
  ao_project = "worldarchitect"
  push_remote = "worldai"   # dual-remote clone; bare `git push` is WRONG here
  [repos."jleechanorg/dark-factory"]
  ao_project = "dark-factory"
  push_remote = "origin"
  ```
  Unknown repo ⇒ bead parks HUMAN_HELD with reason `unmapped_target_repo`
  (fail loud, never guess — jleechan-9sh5 discipline).

### Stage C — dispatch prompt + spawn assertion (9sh5 proper fix)
- Coder prompt template MUST state: repo full name, exact remote name, exact
  branch, and the literal push command (`git push <remote> <branch>`).
- `run_spawn_process` asserts before handing the prompt over: the spawned
  worktree's `git remote get-url <remote>` matches the bead repo; mismatch ⇒
  kill session, park `worktree_remote_mismatch` (fail loud).
- Coder-silence watcher polls the branch on the bead's repo via the bead's
  remote, not `cfg.target_repo`.

### Stage D — verification loop per-repo
- `pr_snapshot`, er_runner prompts/ext_ref, skeptic prompt, escalation
  comment targeting: all switch from `cfg.target_repo` to `overlay.repo(cfg)`.
  (Escalation already broke cross-repo — the twa0/mdgr failures are partly
  this same global-repo assumption.)

### Stage E — E2E proof (the actual acceptance test)
- Fixture: one bead labeled `factory` per repo (worldarchitect + dark-factory)
  in the same daemon run; both must reach a genuine GATE_ASSESSMENT on a PR
  in THEIR OWN repo with zero human steering. This is the "works in any
  repo" criterion — the single-repo version of tonight's sniw.2/C2 proof is
  necessary but not sufficient.

## Explicitly out of scope
- AO-side `[spawning]` state desync (jleechan-52gs) and zombie reaping
  (jleechan-d0wn) — same neighborhood, separate mechanisms.
- Multi-repo CXDB partitioning — the event log already keys by bead id;
  no change needed for correctness.

## Beads
- Stage A+B: jleechan-35y4
- Stage C: jleechan-bqdv (subsumes jleechan-9sh5)
- Stage D: jleechan-9xrs (closes the cross-repo half of the twa0 escalation class)
- Stage E: jleechan-393z (the real jleechan-sniw.2 successor)
