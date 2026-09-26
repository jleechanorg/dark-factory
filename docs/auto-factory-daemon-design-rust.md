# Auto-Factory Daemon — Rust Design Doc (interfaces + code)

**Status:** Draft r1 — companion to the no-code spec [`docs/auto-factory-daemon-spec.md`](auto-factory-daemon-spec.md) (Final r3)
**Split of concerns:** the spec owns *behavior* (states, gates, safety envelope); this doc owns *shape* (crate layout, traits, types, LOC budget). If the two disagree, the spec wins.
**Language:** Rust (user decision 2026-07-03). Single binary, no async runtime — the daemon is a poll loop, not a server.

---

## 1. Design tenets (minimal glue)

1. **Shell out, don't link.** `git`, `gh`, `br`, `ao`/`aow`, and LLM CLIs (`claude -p`, fallback chain) are invoked via `std::process::Command`. No git2, no octocrab, no LLM SDK. The daemon is glue; the tools are the factory.
2. **No async runtime.** Single-threaded tick loop with `std::time::Instant`. Concurrency lives in AO workers, not in the daemon. Dropping tokio removes ~⅓ of the dependency tree and every `Send + Sync` puzzle.
3. **Traits at the tool boundary, structs everywhere else.** Exactly five traits — one per external tool — so tests fake tools, not internals.
4. **Every file ≤ ~220 LOC.** A component that outgrows its budget is re-implementing something a wrapped tool already does.
5. **Stage gating in `main`, not in components.** Stage 1 (verifier plane) vs Stage 2 (re-roll writer plane) is a config flag checked at the dispatch site; `reroll.rs`/`constraints.rs` are not compiled out, just never called.

## 2. Crate layout & LOC budget

```
daemon/                     # new Rust crate in this repo (not a workspace)
├── Cargo.toml
├── launchd/org.darkfactory.daemon.plist.template   # @HOME@ placeholders (spec §4.2.9)
├── install-launchd.sh                              # renders template, ~40 lines shell
└── src/
    ├── main.rs         ~180  poll loop, tick tiers, startup reconciliation, stage gate
    ├── config.rs        ~60  config/daemon.toml via `toml` crate
    ├── telemetry.rs     ~50  JSONL events → ~/Library/Logs/dark-factory/daemon.jsonl
    ├── state.rs        ~160  CXDB overlay store (rusqlite, WAL, own daemon-cxdb.sqlite)
    ├── tools.rs        ~220  the 5 tool traits + subprocess implementations
    ├── intake.rs       ~140  issue→bead normalizer + write-tier auth (spec §4.2.3)
    ├── router.rs        ~70  ZFC task router: render prompt, 1 LLM call, parse verdict
    ├── dispatch.rs     ~120  slot supervisor (≤40 workers, batch ≤15) + handoff
    ├── verifier.rs     ~220  7/8-green gates, ETag cache, evidence floor (spec §4.2.5)
    ├── reroll.rs       ~180  [Stage 2] lock → quiesce(60s) → baseline → fresh branch
    └── constraints.rs  ~100  [Stage 2] extractor call + append-only spec mutation
```

**Total ≈ 1,500 LOC** production (~1,220 for Stage 1). Tests target ≈ 1:1. Rust runs ~1.4× the Python estimate (types + error plumbing) — accepted for the deploy story (single static binary under launchd, no venv drift).

**Dependencies (exactly five):** `rusqlite` (bundled), `serde` + `serde_json`, `toml`, `thiserror`. Nothing else without a design-doc revision.

## 3. Core types (`state.rs`, shared)

```rust
/// Spec §4.2.7 overlay states, incl. r3 pre-PR states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OverlayState {
    Queued,        // bead accepted by intake, awaiting dispatch
    Dispatched,    // worker/pipeline running, no PR yet
    Attested,      // PR open, under verification
    Ready,         // terminal: 7-green, readiness posted, daemon stops driving
    ReRoll,        // re-roll in progress (Stage 2)
    Recovery,      // spec mutated, awaiting re-dispatch (Stage 2)
    Redispatched,  // handed back to the queue
    BudgetHeld,    // budget exhaustion (monitoring-only in Stage 1/2)
    HumanHeld,     // terminal until human action
}

#[derive(Debug, Clone)]
pub struct BeadOverlay {
    pub bead_id: String,
    pub state: OverlayState,
    pub attempt: u32,              // r<n> counter
    pub reroll_count: u32,
    pub autonomy_secs: u64,        // cumulative — nothing on the automated path resets it
    pub spend_usd: f64,            // monitoring-only metric (spec §4.2.8)
    pub pr_number: Option<u64>,
    pub branch: Option<String>,
}

pub trait StateStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError>;
    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError>;
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError>;
    /// Deletion guard: daemon may delete ONLY refs returned here (spec §4.2.8).
    fn owned_branches(&self) -> Result<Vec<String>, DaemonError>;
}
```

`SqliteStateStore` implements `StateStore` against `~/.dark-factory/daemon-cxdb.sqlite` (WAL mode, 5s busy_timeout — same discipline as `runner/cxdb.py`, but a **separate DB file**: no cross-language schema coupling with the Python runner's CXDB).

## 4. Tool boundary (`tools.rs`) — the only traits in the system

```rust
/// br CLI. fetch_candidates == `br list --status open --label factory --json`.
pub trait Tracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError>;
    fn create_bead(&self, title: &str, body: &str, external_ref: &str) -> Result<String, DaemonError>;
    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError>;
}

/// gh CLI (REST + GraphQL). Every fetch goes through the ETag cache (spec §4.2.5).
pub trait Scm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError>;
    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError>; // write-tier gate
    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError>; // CI, mergeable, reviews, threads
    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError>;
}

/// ao / aow CLIs.
pub trait Sessions {
    fn active_count(&self) -> Result<usize, DaemonError>;
    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError>;
    fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError>; // remediation mode
    fn stop(&self, id: &SessionId) -> Result<(), DaemonError>;
    fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError>; // terminal + stable HEAD
}

/// git CLI, always `git -C <workdir>`.
pub trait Vcs {
    fn base_head(&self, base_branch: &str) -> Result<String, DaemonError>; // CURRENT head, never merge-base
    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError>;
    fn head_sha(&self, branch: &str) -> Result<String, DaemonError>;
}

/// LLM judgment calls (router, in-place-vs-reroll verdict, constraint extraction).
/// Implementation = prioritized CLI fallback chain (spec §4.2.5): transient error → retry same
/// (≤2, backoff); substantive error → next in chain. ZFC: ALL judgment goes through here.
pub trait Llm {
    fn judge(&self, prompt: &str) -> Result<String, DaemonError>; // raw text; caller parses JSON
}
```

Each trait has exactly one production impl (`CliTracker`, `CliScm`, `CliSessions`, `CliVcs`, `ChainLlm`) — thin `Command` wrappers sharing one `run_tool(cmd, args, timeout)` helper — plus one test fake in `tests/fakes.rs` (scripted responses, call log).

## 5. Component signatures

```rust
// config.rs — config/daemon.toml (spec §4.2.9)
#[derive(serde::Deserialize)]
pub struct Config {
    pub target_repo: String,            // exactly one (pilot scope)
    pub base_branch: String,            // "main"
    pub stage: u8,                      // 1 = verifier plane only; 2 = re-roll enabled
    pub max_workers: usize,             // 40 (spec §4.2.8)
    pub max_batch: usize,               // 15
    pub fast_tick_secs: u64,            // 60
    pub slow_tick_secs: u64,            // 600
    pub autonomy_timebox_secs: u64,     // 10_800
    pub budget_warn_usd: f64,           // 80% warning threshold (monitoring-only)
    pub spec_dir: String,               // ".factory/specs/" default (spec Appendix B)
}
pub fn load(path: &Path) -> Result<Config, DaemonError>;

// telemetry.rs — schema pinned by spec §4.2.9 (camelCase field names on the wire)
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent<'a> {
    pub timestamp: String, pub bead_id: &'a str, pub attempt_id: u32,
    pub lifecycle_state: OverlayState, pub event_type: &'a str,
    pub metrics: serde_json::Value, pub context: serde_json::Value,
}
pub fn emit(log_path: &Path, ev: &TelemetryEvent) -> Result<(), DaemonError>;

// intake.rs — spec §4.2.3
pub fn normalize(scm: &dyn Scm, tracker: &dyn Tracker, cfg: &Config)
    -> Result<Vec<String>, DaemonError>;  // returns new bead ids; idempotent via external_ref;
                                          // silently skips non-write-tier creators (audit-logged)

// router.rs — ZFC: verdict is the model's; code only renders + parses
pub enum RoutingVerdict { SmallPath, StandardPath }
pub fn route(llm: &dyn Llm, bead: &Bead) -> Result<RoutingVerdict, DaemonError>;
    // parse failure = DaemonError::Parse → bead HUMAN_HELD; NEVER a silent default path

// verifier.rs — spec §4.2.5; read-only
pub struct GateReport { pub results: [(GateName, GateResult); 7], pub all_green: bool }
pub enum GateResult { Green, Red(String), Unknown(String) }  // Unknown ≠ Red: infra vs verdict
pub fn assess(scm: &dyn Scm, pr: u64, cfg: &Config) -> Result<GateReport, DaemonError>;

// dispatch.rs — spec §4.2.2/§4.2.4
pub fn dispatch_ready(sessions: &dyn Sessions, store: &dyn StateStore, cfg: &Config,
                      ready: &[Bead]) -> Result<usize, DaemonError>; // enforces 40/15 caps

// reroll.rs — [Stage 2] spec §4.2.6 numbered handover; each step emits telemetry
pub fn execute(deps: &RerollDeps, bead: &mut BeadOverlay) -> Result<RerollOutcome, DaemonError>;
pub enum RerollOutcome { Rerolled { new_branch: String }, Aborted(String), Held(String) }

// constraints.rs — [Stage 2] spec §4.2.6/§4.2.7
pub struct Extracted { pub inhibition_specs: Vec<String>, pub positive_assertions: Vec<String>,
                       pub security_redaction_encountered: bool }
pub fn extract(llm: &dyn Llm, review_text: &str) -> Result<Extracted, DaemonError>;
pub fn append_mutation(spec_path: &Path, block: &str) -> Result<(), DaemonError>; // temp→fsync→rename
```

## 6. Error taxonomy

```rust
#[derive(thiserror::Error, Debug)]
pub enum DaemonError {
    #[error("tool {tool} failed (rc={rc}): {stderr}")]
    Tool { tool: String, rc: i32, stderr: String },   // transient candidate → retry policy
    #[error("parse: {0}")]  Parse(String),            // substantive → HUMAN_HELD, never defaulted
    #[error("timeout: {0}")] Timeout(String),         // e.g. 60s quiescence → abort re-roll
    #[error("config: {0}")]  Config(String),          // fatal at startup only
}
```

The `Tool`/`Parse` split implements the spec's transient-vs-substantive retry distinction (§4.2.5) mechanically: `Tool` is retryable, `Parse` is not.

## 7. Test strategy (binds the TDD plan)

- **Unit:** every component tested against the `tests/fakes.rs` fakes; zero subprocess calls in unit tests. Table-driven tests for verifier gates and state transitions.
- **Integration (Layer 2):** one `tests/tick_integration.rs` driving `run_tick()` end-to-end over fakes scripted with recorded `gh`/`br` JSON fixtures — real callstack, fakes only at the tool boundary (evidence-floor compliant).
- **Live smoke (manual, pre-Stage-1 pilot):** `daemon --once --dry-run` against the real target repo; asserts read-only behavior by diffing `owned_branches()` before/after.
- **Forbidden in tests:** mocking internal functions, asserting on prompt strings (prompts are model-owned), sleeping to "wait" for the loop.

## 8. Deferred (YAGNI until the spec says otherwise)

Offline bead fail-safe (spec §4.1.6), hygiene sweeps (§4.1.8), the review-fleet orchestrator (§4.1.7 — Skeptic gate shells out to the existing `/green`-style flow in Stage 1), and any dashboard. Each lands as its own ≤1-file addition when its stage arrives.
