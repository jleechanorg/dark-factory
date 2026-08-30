# Auto-Factory Daemon (Rust, Stage 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Stage-1 verifier plane of the Auto-Factory Daemon in Rust — intake, routing, dispatch, and 7-gate verification; re-roll verdicts recorded but never executed.

**Architecture:** Single binary, single-threaded poll loop, five tool-boundary traits (`Tracker`/`Scm`/`Sessions`/`Vcs`/`Llm`) with subprocess implementations; all state in SQLite per `daemon/contracts/schema.sql`; telemetry JSONL per spec §4.2.9. Binding interfaces: `docs/auto-factory-daemon-design-rust.md`. Binding behavior: `docs/auto-factory-daemon-spec.md` (Final r3).

**Tech Stack:** Rust 1.96, `rusqlite` (bundled), `serde`+`serde_json`, `toml`, `thiserror`. NO tokio, NO git2, NO octocrab — all tools shelled via `std::process::Command`.

---

### Task 0: Prerequisites

- [ ] `rustc --version` ≥ 1.90; `sqlite3 --version` present; `gh auth status` OK; `br --help` OK
- [ ] Read `docs/auto-factory-daemon-design-rust.md` fully — its names are binding

### Task 1: Crate scaffold

**Files:** Create `daemon/Cargo.toml`, `daemon/src/main.rs`

- [ ] **Step 1:** Write `daemon/Cargo.toml`:

```toml
[package]
name = "daemon"
version = "0.1.0"
edition = "2021"

[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "2"
```

- [ ] **Step 2:** Write `daemon/src/main.rs`:

```rust
fn main() {
    println!("auto-factory daemon (stage 1)");
}

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_compiles() { assert!(true); }
}
```

- [ ] **Step 3:** Run `cd daemon && cargo test` — expected: `1 passed`
- [ ] **Step 4:** Commit: `git add daemon/Cargo.toml daemon/src/main.rs && git commit -m "feat(daemon): rust crate scaffold"`

### Task 2: Errors + config (`config.rs`) — Layer 1

**Files:** Create `daemon/src/errors.rs`, `daemon/src/config.rs`; Modify `daemon/src/main.rs` (add `mod` lines)

- [ ] **Step 1:** Write `daemon/src/errors.rs` (design doc §6, verbatim):

```rust
#[derive(thiserror::Error, Debug)]
pub enum DaemonError {
    #[error("tool {tool} failed (rc={rc}): {stderr}")]
    Tool { tool: String, rc: i32, stderr: String },
    #[error("parse: {0}")]  Parse(String),
    #[error("timeout: {0}")] Timeout(String),
    #[error("config: {0}")]  Config(String),
}
```

- [ ] **Step 2:** Write the failing test at the bottom of `daemon/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_example_config() {
        let cfg = load(std::path::Path::new("contracts/daemon.toml.example")).unwrap();
        assert_eq!(cfg.stage, 1);
        // Canonical factory capacity contract (bead dark-factory-59wt): 40/15.
        assert_eq!(cfg.max_workers, 40);
        assert_eq!(cfg.max_batch, 15);
        assert_eq!(cfg.base_branch, "main");
    }
    #[test]
    fn missing_key_is_config_error() {
        let dir = std::env::temp_dir().join("afd_cfg_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(&p, "target_repo = \"x/y\"\n").unwrap();
        assert!(matches!(load(&p), Err(crate::errors::DaemonError::Config(_))));
    }
}
```

- [ ] **Step 3:** `cargo test` — expected FAIL: `load` not found
- [ ] **Step 4:** Implement above the tests (Config struct = design doc §5, field-for-field):

```rust
use std::path::Path;
use crate::errors::DaemonError;

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Config {
    pub target_repo: String,
    pub base_branch: String,
    pub stage: u8,
    pub max_workers: usize,
    pub max_batch: usize,
    pub fast_tick_secs: u64,
    pub slow_tick_secs: u64,
    pub autonomy_timebox_secs: u64,
    pub budget_warn_usd: f64,
    pub spec_dir: String,
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))
}
```

- [ ] **Step 5:** Add `mod config; mod errors;` to `main.rs`; `cargo test` — expected: all pass
- [ ] **Step 6:** Commit: `git commit -am "feat(daemon): config + error taxonomy"`

### Task 3: Telemetry (`telemetry.rs`) — Layer 1

**Files:** Create `daemon/src/telemetry.rs`; Modify `daemon/src/main.rs`

- [ ] **Step 1:** Failing test (schema fields per spec §4.2.9 — exact names):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emits_schema_compliant_jsonl() {
        let dir = std::env::temp_dir().join("afd_tel_test");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.jsonl");
        let _ = std::fs::remove_file(&log);
        let ev = TelemetryEvent {
            timestamp: "2026-07-03T19:00:00Z".into(), bead_id: "b1".into(), attempt_id: 1,
            lifecycle_state: "QUEUED".into(), event_type: "INTAKE_BEAD_CREATED".into(),
            metrics: serde_json::json!({}), context: serde_json::json!({"title":"t"}),
        };
        emit(&log, &ev).unwrap();
        emit(&log, &ev).unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        for key in ["timestamp","beadId","attemptId","lifecycleState","eventType","metrics","context"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}
```

- [ ] **Step 2:** `cargo test emits_schema` — expected FAIL
- [ ] **Step 3:** Implement:

```rust
use std::io::Write;
use std::path::Path;
use crate::errors::DaemonError;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent {
    pub timestamp: String,
    pub bead_id: String,
    pub attempt_id: u32,
    pub lifecycle_state: String,
    pub event_type: String,
    pub metrics: serde_json::Value,
    pub context: serde_json::Value,
}

pub fn emit(log_path: &Path, ev: &TelemetryEvent) -> Result<(), DaemonError> {
    if let Some(parent) = log_path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(log_path)
        .map_err(|e| DaemonError::Config(format!("telemetry open: {e}")))?;
    let line = serde_json::to_string(ev).map_err(|e| DaemonError::Parse(e.to_string()))?;
    writeln!(f, "{line}").map_err(|e| DaemonError::Config(format!("telemetry write: {e}")))
}
```

- [ ] **Step 4:** `cargo test` pass → commit `feat(daemon): telemetry JSONL emitter`

### Task 4: State store (`state.rs`) — Layer 1

**Files:** Create `daemon/src/state.rs`; Modify `daemon/src/main.rs`

- [ ] **Step 1:** Failing tests (roundtrip; CHECK-constraint rejection; branch-registry deletion guard):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> SqliteStateStore {
        SqliteStateStore::open_in_memory_with_schema(
            include_str!("../contracts/schema.sql")).unwrap()
    }
    #[test]
    fn overlay_roundtrip() {
        let s = store();
        let o = BeadOverlay { bead_id: "b1".into(), state: OverlayState::Queued, attempt: 1,
            reroll_count: 0, autonomy_secs: 0, spend_usd: 0.0, pr_number: None, branch: None };
        s.save(&o).unwrap();
        let got = s.load("b1").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::Queued);
        assert_eq!(got.attempt, 1);
    }
    #[test]
    fn illegal_state_string_rejected_by_schema() {
        let s = store();
        let r = s.conn.execute(
            "INSERT INTO bead_overlay (bead_id,state,updated_at) VALUES ('x','BOGUS','now')", []);
        assert!(r.is_err(), "CHECK constraint must reject unknown states");
    }
    #[test]
    fn owned_branches_lists_only_registered() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        assert_eq!(s.owned_branches().unwrap(), vec!["factory/b1-r1".to_string()]);
    }
}
```

- [ ] **Step 2:** `cargo test state` — expected FAIL
- [ ] **Step 3:** Implement `OverlayState` (8 variants, `SCREAMING_SNAKE_CASE` string mapping via `as_str()`/`from_str`), `BeadOverlay`, `StateStore` trait exactly per design doc §3, and `SqliteStateStore` with `open(path)` (WAL + busy_timeout 5000, applies `contracts/schema.sql`) plus `open_in_memory_with_schema` for tests. `save` = `INSERT ... ON CONFLICT(bead_id) DO UPDATE`, `updated_at` = ISO-8601 UTC now.
- [ ] **Step 4:** `cargo test` pass → commit `feat(daemon): sqlite state store per contract schema`

### Task 5: Tool boundary (`tools.rs` + fakes) — Layer 1

**Files:** Create `daemon/src/tools.rs`, `daemon/tests/fakes.rs` (as `mod fakes` shared via `daemon/tests/common/mod.rs`)

- [ ] **Step 1:** Define the five traits + data types (`Bead`, `Issue`, `Permission`, `PrSnapshot`, `SpawnSpec`, `SessionId`) verbatim from design doc §4, plus:

```rust
pub fn run_tool(cmd: &str, args: &[&str], timeout_secs: u64) -> Result<String, DaemonError>
```

subprocess helper: spawn, wait with deadline (poll `try_wait` every 100ms; kill on deadline → `DaemonError::Timeout`), non-zero rc → `DaemonError::Tool`, else stdout as String.
- [ ] **Step 2:** Failing tests: `run_tool("true",&[],5)` ok; `run_tool("false",&[],5)` → `Tool{rc:1}`; `run_tool("sleep",&["2"],1)` → `Timeout` (mark `#[cfg(unix)]`).
- [ ] **Step 3:** Implement; `cargo test tools` pass.
- [ ] **Step 4:** Write scripted fakes implementing all five traits: each fake holds `Vec`/`HashMap` scripted responses + a `RefCell<Vec<String>>` call log. No subprocess use in fakes.
- [ ] **Step 5:** Commit `feat(daemon): tool traits, subprocess runner, test fakes`

### Task 6: Intake (`intake.rs`) — Layer 1

**Files:** Create `daemon/src/intake.rs`

- [ ] **Step 1:** Failing tests with fakes: (a) new labeled issue from write-tier collaborator → `create_bead` called once with `external_ref="owner/repo#N"`, returns 1 bead id, INTAKE event emitted; (b) same issue again (tracker fake already knows the external_ref) → no duplicate; (c) issue by read-tier user → skipped, zero `create_bead` calls, audit context in the emitted event.
- [ ] **Step 2:** Implement `normalize(scm,tracker,cfg)` per design doc §5. Permission gate: only `Permission::Write | Permission::Admin` pass (spec §4.2.3).
- [ ] **Step 3:** `cargo test intake` pass → commit `feat(daemon): intake normalizer with write-tier gate`

### Task 7: Router (`router.rs`) — Layer 1 (+ manual Layer 3 smoke)

**Files:** Create `daemon/src/router.rs`

- [ ] **Step 1:** Failing tests: fake `Llm` returning `{"routingVerdict":"SMALL_PATH","justification":"x"}` → `RoutingVerdict::SmallPath`; returning prose (`"it looks small to me"`) → `Err(DaemonError::Parse(_))` — assert it is NOT silently defaulted; returning `STANDARD_PATH` → `StandardPath`.
- [ ] **Step 2:** Implement `route(llm,bead)`: render prompt (bead title/description; contract = spec Appendix C item 1), call `llm.judge`, strict `serde_json` parse of the LAST `{...}` block in the reply. Parse failure → `Parse` (caller parks bead HUMAN_HELD).
- [ ] **Step 3:** `cargo test router` pass → commit `feat(daemon): ZFC task router`
- [ ] **Layer 3 (manual, pre-pilot only, not CI):** `daemon route-smoke --bead <id>` against the real LLM chain once, verify a parseable verdict.

### Task 8: Verifier (`verifier.rs`) — Layer 1

**Files:** Create `daemon/src/verifier.rs`

- [ ] **Step 1:** Failing table-driven tests over fake `PrSnapshot`s: all-green snapshot → `all_green=true`; failing CI → gate 1 `Red`; API error on thread query → gate 5 `Unknown` and `all_green=false` but assert `Unknown != Red` (distinct variants with distinct reasons); >100 non-test changed LOC without integration-evidence marker → gate 6 `Red("evidence floor")`; ≤100 LOC → gate 6 `Green`.
- [ ] **Step 2:** Implement `assess(scm,pr,cfg) -> GateReport` with `results: [(GateName, GateResult); 7]` per design doc §5 and gates per spec §4.2.5 (CI, conflicts, CodeRabbit APPROVED, Bugbot clean, threads `isResolved`, evidence floor, Skeptic — Stage 1 Skeptic consumes the recorded verdict from the lite verifier's `GATE_ASSESSMENT` events or an `Llm` adversarial call; verdict grammar `pass|warn|fail`, `warn` non-blocking).
- [ ] **Step 3:** `cargo test verifier` pass → commit `feat(daemon): 7-gate verifier with Unknown/Red distinction`

### Task 9: Dispatch (`dispatch.rs`) — Layer 1

**Files:** Create `daemon/src/dispatch.rs`

- [ ] **Step 1:** Failing tests: 40 ready beads, 0 active → exactly 15 spawned (max_batch); 38 active of 40 → exactly 2 spawned; 40 active → 0 spawned and no `Sessions::spawn` calls (assert via fake call log); each spawn registers `factory/<bead>-r<attempt>` in the store and flips state QUEUED→DISPATCHED. (Canonical factory capacity contract, bead dark-factory-59wt: max_workers=40, max_batch=15.)
- [ ] **Step 2:** Implement `dispatch_ready(sessions,store,cfg,ready)`; `cargo test dispatch` pass → commit `feat(daemon): dispatcher with 30/15 caps`

### Task 10: Tick loop + integration — Layer 2

**Files:** Modify `daemon/src/main.rs` (add `run_tick`, `--once`, `--dry-run` flags via `std::env::args`); Create `daemon/tests/tick_integration.rs`

- [ ] **Step 1:** Failing integration test: wire ALL real components against the five fakes only (scripted: 1 new issue → intake → route SMALL_PATH → dispatch → fake PR appears → verifier all-green). Drive `run_tick` twice; assert final overlay state `ATTESTED`, a `READY_FOR_MERGE`-equivalent gate report, and the telemetry file containing ≥5 events in schema order. Real call stack, fakes ONLY at trait boundaries — this is the Layer-2 / evidence-floor test.
- [ ] **Step 2:** Implement `run_tick` (fast tier: ATTESTED beads → verifier; slow tier: intake+route+dispatch every `slow_tick_secs/fast_tick_secs` ticks; startup reconciliation = reload overlays from SQLite; stage gate: `stage==1` → re-roll verdicts recorded via telemetry + HUMAN_HELD park, never executed).
- [ ] **Step 3:** `cargo test` (all) pass → commit `feat(daemon): stage-1 tick loop + integration test`
- [ ] **Step 4:** `cargo clippy -- -D warnings` clean → fix or commit fixes

## Testing-layer map

| Test | Layer | Why sufficient |
|---|---|---|
| Tasks 2–9 unit tests | L1 (unit, fakes) | pure logic; LLM judgment excluded by design (ZFC — judgment is the model's, code only parses) |
| `tests/tick_integration.rs` | L2 (integration) | real call stack end-to-end; fakes only at the 5 tool-CLI boundaries = external-API mock boundary, evidence-floor compliant |
| `route-smoke` (manual) | L3 (live LLM) | the ONLY seam L1/L2 can't cover is real-model verdict parseability; run once pre-pilot, not in CI (cost ∝ risk) |

## Definition of done (Stage 1)

- `cargo test` fully green; `cargo clippy -- -D warnings` clean
- `daemon --once --dry-run` from the repo root: produces ≥1 `TICK` telemetry event, zero SCM writes, zero session spawns (assert by absence of spawn/comment tool calls in `--dry-run` log output)
- No new dependencies beyond the five in Task 1
- Every file within design-doc LOC budget (±20%)
