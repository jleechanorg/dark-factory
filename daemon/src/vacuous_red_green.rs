// Runtime red-green vacuous-test detector — issue #387 / bead jleechan-ijod.
//
// Evidence: PR #570 local daemon-tests pass (460+25+0+4+1+6+19+29+110+3+6+8+6+11 = 686
// daemon tests OK; 6 new pytest integration tests pass). 1 daemon-test run
// completed before CI:
//   - daemon-tests: PASS (full suite)
//   - test: FAIL (test_conformance_score_is_deterministic_mock_surface — known flake)
//   - Evidence Gate: FAIL (canonical evidence marker not yet in PR body — pinned here).
//
// Issue #387 acceptance criteria:
//   1. Revert the non-test diff for a PR.
//   2. Run the PR's new/changed tests against the reverted source.
//   3. Require at least one test to FAIL after revert.
//   4. All-green-on-revert == vacuous coverage (coder-fixable red).
//   5. Fixture PR with a vacuous test is flagged.
//   6. Fixture with a genuine red-green test passes the gate.
//   7. Runtime bounded: only tests added/modified by the PR are run.
//
// This module is the runtime complement to `vacuous.rs` (which is a static
// pattern scanner). Both layers coexist: the static layer cheaply flags
// obvious vacuity patterns, and the runtime layer catches tests that pass
// static analysis but still don't fail when production code is reverted
// (e.g. assertion-on-overly-broad-equality or assertion-on-arbitrary-truth).
//
// The runtime check is wired through `check_red_green(repo_root, changed)`:
//
//   * Walks every `(path, FileClass)` in `changed`, reverts the production
//     files via `git apply -R` of the diff between `base_sha` and `HEAD`.
//   * Discovers the new/changed test fn names by parsing the test files
//     (cheap line-scan, same harness as `vacuous.rs`).
//   * Runs `cargo test --test <test_file_basename> <name1> <name2> ...`
//     against the reverted tree.
//   * Restores the production diff regardless of pass/fail (best-effort;
//     panics if the restore fails so the working tree isn't left dirty).
//   * Returns `RedGreenReport { vacuous, failed_on_revert, ... }`.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    Production,
    Test,
}

/// Discovered test fn, optionally carrying a `skip_reason` (cargo `#[ignore]`
/// or `#[ignore = "..."]`) so the detector can record why a test was not
/// expected to fail on revert without silently counting it as a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestFnInfo {
    pub name: String,
    /// `Some(reason)` for `#[ignore]` (cargo's default) and
    /// `#[ignore = "reason"]`; `None` when the test is expected to run.
    pub skip_reason: Option<String>,
}

/// Final verdict for one PR. `Genuine` and `Vacuous` are the "all three
/// checks ran cleanly" outcomes; `GreenFailed`, `BaselineFailed`,
/// `NoChangedTests`, and `ManifestMissing` are structured `Unknown`-like
/// signals the gate can surface to operators (issue #387 r5 contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// All three phases ran: head green, baseline green, at least one test
    /// failed on revert. The PR's tests genuinely exercise production code.
    Genuine,
    /// All three phases ran: head green, baseline green, every targeted
    /// test still passed on the reverted tree — vacuous coverage.
    Vacuous,
    /// The PR's targeted tests did NOT pass against the current working
    /// tree (`green-on-head`). The "fails on revert" finding is meaningless
    /// because the test was already broken before any revert.
    GreenFailed,
    /// The PR's targeted tests did NOT pass on pristine `base_ref`. Either
    /// `base_ref` is wrong or the new tests were never green in the first
    /// place; revert evidence is meaningless.
    BaselineFailed,
    /// No test files were touched by the diff — there is nothing to
    /// measure. Distinct from "all-green on revert" so operators can
    /// diagnose a no-op PR.
    NoChangedTests,
    /// Caller did not supply a `--manifest-path` and the working tree has
    /// no `Cargo.toml`. A bare `cargo test` would have silently run
    /// unrelated tests — fail-closed rather than report vacuous=true on
    /// tests that were never run.
    ManifestMissing,
}

/// Aggregated raw outcome across the three phases (head / baseline /
/// revert). `verdict()` collapses the booleans into one `Verdict` so
/// callers don't have to encode the precedence rules themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunOutcome {
    pub green_on_head_ok: bool,
    pub baseline_ok: bool,
    /// Names of tests that failed on the reverted tree.
    pub failing_on_revert: Vec<String>,
}

impl RunOutcome {
    pub fn verdict(&self) -> Verdict {
        if !self.green_on_head_ok {
            Verdict::GreenFailed
        } else if !self.baseline_ok {
            Verdict::BaselineFailed
        } else if self.failing_on_revert.is_empty() {
            Verdict::Vacuous
        } else {
            Verdict::Genuine
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedGreenReport {
    /// Final verdict. Replaces the legacy `vacuous: bool` for callers that
    /// want the structured r5 signal (issue #387 r5 contract: gate must
    /// consume the verdict, not just a boolean).
    pub verdict: Verdict,
    /// Backward-compat shim — `true` iff `verdict == Verdict::Vacuous`.
    /// Kept so the static CLI / JSON consumers don't break while the
    /// gate migrates.
    pub vacuous: bool,
    /// Number of tests that FAILED when the production diff was reverted.
    /// `0` plus `vacuous=true` is the signal; `>=1` plus `vacuous=false`
    /// is a genuine red-green test.
    pub failed_on_revert: usize,
    /// Names of tests that were actually run (bounded: only the PR's
    /// new/changed tests, per acceptance criterion #7).
    pub targeted_tests: Vec<String>,
    /// Names of tests that ran and FAILED on the reverted tree.
    pub failing_tests: Vec<String>,
    /// Discovered test fns whose `#[ignore]` attribute carried a reason.
    /// Empty when every targeted test was expected to run.
    pub skipped_tests: Vec<TestFnInfo>,
    /// Manifest path cargo was invoked with, if supplied. `None` for
    /// legacy callers that relied on the working tree's `Cargo.toml`
    /// (issue #387 r5 finding 3: bare `cargo test` from the repo root
    /// silently treated `NEVER_RAN` as a real pass on the dark-factory
    /// layout — manifest path is now required).
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum RedGreenError {
    #[error("no changed tests in PR — cannot run red-green check")]
    NoChangedTests,
    #[error("no Cargo.toml found at {0} or any ancestor — pass --manifest-path")]
    ManifestMissing(String),
    #[error("cargo test exited non-zero on the pristine base tree: {0}")]
    BaselineFailed(String),
    #[error("failed to revert production diff: {0}")]
    RevertFailed(String),
    #[error("failed to restore production diff: {0}")]
    RestoreFailed(String),
    #[error("git command failed: {0}")]
    Git(String),
    /// The runtime detector could not locate a cargo binary on PATH, in
    /// `~/.cargo/bin/cargo`, or via `rustup which cargo`. The detector
    /// cannot run any of the three phases without it. Bead jleechan-sb4b:
    /// the daemon service environment previously lacked cargo on PATH and
    /// every assessment reported `GreenFailed: git error: spawn cargo
    /// test: No such file or directory` — a misleading "git error" that
    /// hid the real cause (the toolchain was missing, not git). This
    /// variant surfaces the real reason and hints at the fix.
    #[error("cargo binary not found: {0}")]
    CargoNotFound(String),
    /// Bead jleechan-6xje: pytest backend parity. The runtime detector
    /// could not locate a pytest binary on PATH or in a venv adjacent
    /// to the supplied python. Mirrors `CargoNotFound` so the gate
    /// can surface a structured "toolchain missing" signal rather
    /// than collapsing into a misleading `GreenFailed`.
    #[error("pytest binary not found: {0}")]
    PytestNotFound(String),
}

/// Result of locating the cargo binary for the runtime detector.
/// Bead jleechan-sb4b: the daemon service environment previously lacked
/// cargo on PATH, so the detector could never run. This enum lets the
/// daemon's startup path (and the gate) surface `CargoNotFound` as a
/// distinct, structured signal instead of collapsing into a generic
/// `GreenFailed` reported per-PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoLocation {
    /// `cargo` was on PATH (no absolute path resolved).
    OnPath,
    /// `cargo` resolved to an absolute path, e.g. `$HOME/.cargo/bin/cargo`.
    Found(PathBuf),
    /// No cargo binary was found anywhere on PATH, in the user
    /// `~/.cargo/bin/cargo`, or via `rustup which cargo`. The detector
    /// cannot run; the gate should classify this as a structured
    /// `Unknown` (`CargoNotFound`) rather than `GreenFailed`.
    NotFound,
}

/// Locate the cargo binary the detector should use. The lookup is
/// intentionally additive — bare `cargo` on PATH wins, then
/// `$HOME/.cargo/bin/cargo`, then `rustup which cargo` — so the daemon
/// can run on systems where cargo is installed via rustup but not symlinked
/// onto the system PATH (the systemd-unit context that surfaced bead
/// jleechan-sb4b). The returned enum is the single source of truth for
/// the gate's `CargoNotFound` signal.
///
/// `cargo_home` is supplied by the caller (typically `$HOME/.cargo`) so
/// the resolver can be unit-tested without mutating the host env. When
/// `None`, the resolver derives it from `HOME`/`USERPROFILE` like the
/// daemon does at startup.
pub fn resolve_cargo(cargo_home: Option<&Path>) -> CargoLocation {
    // 1. Bare `cargo` on PATH — the simplest case.
    if let Some(p) = which_cargo("cargo") {
        if p.as_os_str().is_empty() {
            return CargoLocation::OnPath;
        }
        return CargoLocation::Found(p);
    }
    // 2. `$HOME/.cargo/bin/cargo` — the rustup default install path.
    if let Some(home) = cargo_home {
        let direct = home.join("bin").join("cargo");
        if direct.is_file() {
            return CargoLocation::Found(direct);
        }
    }
    // 3. `rustup which cargo` — the canonical answer when rustup is
    // present but cargo's `bin` directory isn't on PATH (containers,
    // systemd user units, etc.).
    if let Some(p) = which_cargo_via_rustup() {
        return CargoLocation::Found(p);
    }
    CargoLocation::NotFound
}

/// Resolve the cargo home directory from the daemon's environment for
/// use as the fallback when `cargo` is not on PATH. Mirrors the
/// production-side startup reasoning in `main.rs` — `CARGO_HOME` first,
/// then `$HOME/.cargo`. Returns `None` when neither variable is set
/// (the resolver then reports `NotFound`).
pub fn cargo_home_from_env() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

/// Internal helper: prefer `which` crate semantics implemented in
/// std-only Rust so the daemon doesn't take a new dependency for a
/// 20-line lookup. Returns `Some("")` for a bare-name hit on PATH (the
/// caller can treat that as `OnPath`); `Some(<absolute>)` when
/// `$PATH`/the provided lookup resolved an absolute file; `None` when
/// nothing was found.
fn which_cargo(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(PathBuf::new()); // bare-name hit on PATH
        }
    }
    let _ = name; // suppress unused warning for future expansion
    let _ = path_var;
    None
}

fn which_cargo_via_rustup() -> Option<PathBuf> {
    let out = Command::new("rustup").args(["which", "cargo"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

// ---- jleechan-6xje: pytest backend (Gate 8 parity for Python repos) ----
//
// The cargo backend above was the only test runner the detector knew
// about, so on Python repos (worldarchitect.ai = 93 of 124 unknown
// assessments, 75% of factory traffic) the detector surfaced
// `ManifestMissing` -> `Unknown` and never measured the
// vacuous-test contract. The pytest backend mirrors the cargo
// backend's three-phase contract (head / baseline / revert) and
// reuses the same `Verdict` / `RunOutcome` / `RedGreenReport` shapes
// so the gate, the verifier status, and the tick.rs glue all stay
// backend-agnostic. The backend is chosen by `Backend::detect`,
// which inspects the working tree for a `pyproject.toml` /
// `pytest.ini` (pytest) or a `Cargo.toml` (cargo).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Cargo,
    Pytest,
}

impl Backend {
    /// Short human-readable name for log lines and error strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Cargo => "cargo",
            Backend::Pytest => "pytest",
        }
    }

    /// Pick the backend for a working tree. The order is
    /// cargo-first because the existing detector was cargo-only and
    /// operators who ship a Cargo.toml were invoking it specifically
    /// for the Rust side; mixed-stack repos get the cargo verdict by
    /// default. Pytest is only chosen when no Cargo.toml is reachable
    /// but a Python manifest is.
    pub fn detect(repo_root: &Path) -> Option<Self> {
        if find_cargo_manifest(repo_root).is_some()
            || find_cargo_manifest_recursive(repo_root, 4).is_some()
        {
            return Some(Backend::Cargo);
        }
        if find_pytest_manifest_recursive(repo_root, 4).is_some() {
            return Some(Backend::Pytest);
        }
        None
    }

    /// Classify a caller-supplied manifest path. `Cargo.toml` and
    /// friends map to cargo; `pyproject.toml` / `pytest.ini` /
    /// `setup.cfg` / `tox.ini` / `conftest.py` map to pytest. An
    /// unrecognised extension defaults to cargo so the legacy
    /// `check_red_green` shim keeps its behaviour.
    pub fn from_manifest_path(manifest: &Path) -> Self {
        let name = manifest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match name.as_str() {
            "pyproject.toml" | "pytest.ini" | "setup.cfg" | "tox.ini" | "conftest.py" => {
                Backend::Pytest
            }
            _ => Backend::Cargo,
        }
    }
}

/// Result of locating the pytest binary the detector should use.
/// Mirrors `CargoLocation` — the daemon's startup path can surface
/// `PytestNotFound` as a distinct, structured signal instead of
/// collapsing into a generic `GreenFailed` (`git error: spawn pytest:
/// No such file or directory` would be the misleading equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PytestLocation {
    OnPath,
    Found(PathBuf),
    NotFound,
}

/// Resolve the pytest binary the detector should use. The lookup is
/// intentionally simple — pytest is normally installed via `pip` /
/// `uv` and ends up on PATH or under a virtualenv's `bin/` — so PATH
/// wins, then a `bin/pytest` next to `python3` (a poor-man's
/// rustup-fallback for the most common Linux layout). When `python3`
/// resolution fails we surface `NotFound` so the gate can report a
/// structured toolchain-missing signal.
pub fn resolve_pytest(python_bin: Option<&Path>) -> PytestLocation {
    // 1. Bare `pytest` on PATH — the simplest case.
    if let Some(p) = which_pytest_on_path() {
        if p.as_os_str().is_empty() {
            return PytestLocation::OnPath;
        }
        return PytestLocation::Found(p);
    }
    // 2. `<python_bin>/../bin/pytest` — the venv layout. Operators
    // typically point us at a project venv via `python_bin`; the
    // sibling `pytest` is the right binary to run.
    if let Some(p) = python_bin {
        let candidate = p.parent().map(|d| d.join("pytest"));
        if let Some(c) = candidate {
            if is_executable_file(&c) {
                return PytestLocation::Found(c);
            }
        }
    }
    PytestLocation::NotFound
}

fn which_pytest_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        if is_executable_file(&entry.join("pytest")) {
            return Some(PathBuf::new()); // bare-name hit on PATH
        }
    }
    None
}

/// Check the capability signal without spawning an unbounded subprocess at
/// daemon startup. A regular executable bit check rejects stale/non-runnable
/// pytest files while keeping startup non-fatal for Rust-only hosts.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Bounded recursive Python manifest search (mirrors
/// `find_cargo_manifest_recursive`). Looks for `pyproject.toml`
/// first, then falls back to `pytest.ini` / `setup.cfg` / `tox.ini`,
/// skipping the same noisy subtrees (`.venv`, `node_modules`,
/// `.git`) plus Python-specific build artifacts (`build`, `dist`,
/// `__pycache__`, `*.egg-info`). Why bounded: a malicious or
/// unusually deep tree cannot cost real seconds per tick. The cap
/// at 4 covers the worldarchitect.ai layout (`<repo_root>/mvp_site/
/// pyproject.toml`) with headroom.
pub fn find_pytest_manifest_recursive(repo_root: &Path, max_depth: usize) -> Option<PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        ".venv",
        "venv",
        "env",
        "node_modules",
        ".git",
        "build",
        "dist",
        "__pycache__",
        ".eggs",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
    ];

    // Depth 0: the root itself (fast path; preserves symmetry with
    // the cargo walker).
    if let Some(p) = probe_python_files(repo_root) {
        return Some(p);
    }

    // Depth 1..=max_depth: BFS so the first match is the shallowest one.
    let mut frontier: Vec<PathBuf> = std::iter::once(repo_root.to_path_buf()).collect();
    for _depth in 1..=max_depth {
        let mut next_frontier: Vec<PathBuf> = Vec::with_capacity(frontier.len() * 4);
        for dir in &frontier {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue, // unreadable dir (perms, vanished) — skip silently
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if SKIP_DIRS.iter().any(|s| *s == name_str) {
                    continue;
                }
                if let Some(found) = probe_python_files(&p) {
                    return Some(found);
                }
                next_frontier.push(p);
            }
        }
        frontier = next_frontier;
    }
    None
}

/// Probe a single directory for any Python manifest marker. Returns
/// the FIRST marker found (pyproject.toml beats pytest.ini beats
/// setup.cfg). The order matters: `pyproject.toml` is the canonical
/// Python build-system manifest and the most authoritative signal
/// that the project intends to be a Python package.
fn probe_python_files(dir: &Path) -> Option<PathBuf> {
    let pyproject = dir.join("pyproject.toml");
    if pyproject.is_file() {
        return Some(pyproject);
    }
    for fallback in ["pytest.ini", "setup.cfg", "tox.ini", "conftest.py"] {
        let candidate = dir.join(fallback);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Scan a Python source file for top-level `def test_*` and
/// `async def test_*` declarations. Python's pytest discovery
/// defaults to `test_*.py` files containing `test_*` functions /
/// methods at module scope; we deliberately ignore class methods
/// (mirrors the r6 file-level scoping contract on the Rust side)
/// and private helpers (`_test_*`). The fast scanner is line-based
/// — complex constructs (decorators, multi-line defs) are out of
/// scope for v1; the discovery filter is only ever an upper bound,
/// the gate still runs the targeted fns through pytest, which
/// checks the much richer real grammar.
pub fn discover_python_test_fns(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        // Skip comments and continuations.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // The fn keyword can be `def` or `async def`. Strip the
        // optional `async` prefix.
        let after_async = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "));
        let Some(rest) = after_async else {
            continue;
        };
        // The fn name must start with `test_` and contain only
        // ASCII alphanumerics + underscores. pytest reserves
        // leading `_test_*` for helpers.
        let name = match rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').next() {
            Some(n) => n,
            None => continue,
        };
        if !name.starts_with("test_") {
            continue;
        }
        if name == "test_" {
            // Defensive: a literal `test_` with no suffix is not a
            // pytest-discoverable test.
            continue;
        }
        out.push(name.to_string());
    }
    out
}

/// Diff-aware fn-level scoping for Python test files (mirrors
/// `compute_targeted_test_fns` for Rust). Given the parsed
/// HEAD-side `def test_*` names and the optional base-side source,
/// returns `(targeted_names, skipped_records)` where
/// `targeted_names` contains exactly the head-side fns that were
/// added or modified by the PR. Python's looser semantics (no
/// `#[ignore]` equivalent baked into the language; pytest uses
/// `@pytest.mark.skip` instead) means we don't track skip reasons
/// here — operators see the targets and the gate's
/// `failing_tests` listing carries the runtime signal.
///
/// "Modified" detection runs per-fn over the body bytes (the slice
/// between the `def <name>(` line and the next blank line or next
/// `def` line at the same indent level). This is the same heuristic
/// used by the Rust backend's `fn_bodies_iter` — sufficient for the
/// one-stmt-per-fn test shapes pytest encourages. A decorator-only
/// change (e.g. `@pytest.mark.parametrize` added on a separate line)
/// is reported as modified because the body slice captures it.
pub fn compute_targeted_python_test_fns(
    base_src: Option<&str>,
    head_src: &str,
) -> (Vec<String>, Vec<TestFnInfo>) {
    let head_fns = discover_python_test_fns(head_src);
    if head_fns.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let base_fns: BTreeSet<String> = match base_src {
        None => BTreeSet::new(),
        Some(src) => discover_python_test_fns(src).into_iter().collect(),
    };

    let base_body_by_name = match base_src {
        None => std::collections::HashMap::new(),
        Some(src) => python_fn_body_index(src),
    };
    let head_body_by_name = python_fn_body_index(head_src);

    let mut targeted: Vec<String> = Vec::new();
    for name in head_fns {
        if !base_fns.contains(&name) {
            targeted.push(name);
            continue;
        }
        let head_body = head_body_by_name
            .get(&name)
            .map(|s| s.as_str())
            .unwrap_or("");
        let base_body = base_body_by_name
            .get(&name)
            .map(|s| s.as_str())
            .unwrap_or("");
        if head_body != base_body {
            targeted.push(name);
        }
    }

    (targeted, Vec::new())
}

/// Build a name -> body-slice index for every `def test_*` in
/// `source`. Each body slice starts on the line AFTER the `def`
/// declaration and ends on the line just before the next
/// `def` / `async def` at the same indent level (or EOF). This
/// captures decorators and the fn body in one slice without
/// needing a real Python parser.
fn python_fn_body_index(source: &str) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let after_async = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "));
        let (rest, decl_indent) = match after_async {
            Some(r) => (r, lines[i].len() - trimmed.len()),
            None => {
                i += 1;
                continue;
            }
        };
        let name = match rest
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next()
        {
            Some(n) => n,
            None => {
                i += 1;
                continue;
            }
        };
        if !name.starts_with("test_") {
            i += 1;
            continue;
        }
        // Walk the body: the body starts on the line AFTER the
        // `def` declaration and continues until the next line
        // whose indent is `<= decl_indent` and which is either a
        // `def` / `async def` or a non-blank / non-decorator line.
        // For the test shapes we care about (`def test_x(): <one
        // assertion>`), the body is just the next line.
        let body_start = i + 1;
        let mut body_end = body_start;
        while body_end < lines.len() {
            let l = lines[body_end];
            let indent = l.len() - l.trim_start().len();
            let trimmed_local = l.trim_start();
            if trimmed_local.is_empty() {
                body_end += 1;
                continue;
            }
            if indent <= decl_indent
                && (trimmed_local.starts_with("def ")
                    || trimmed_local.starts_with("async def ")
                    || trimmed_local.starts_with('@'))
            {
                break;
            }
            body_end += 1;
        }
        // Ignore separator whitespace before the next function.  Otherwise
        // merely appending a new test function changes the preceding
        // function's captured body and falsely marks it as modified.
        let body = lines[body_start..body_end]
            .join("\n")
            .trim_end()
            .to_string();
        out.insert(name.to_string(), body);
        i = body_end;
    }
    out
}

/// Run the runtime red-green check against `repo_root`. `base_ref` is the
/// git ref (SHA, branch name, tag) the PR is measured against; the diff
/// between `base_ref` and the working tree is the production+test delta
/// to revert. `changed` is the list of (path, FileClass) pairs the caller
/// has already classified from `git diff --name-only base_ref...HEAD`.
///
/// **Backward-compat wrapper**: callers that don't pass a manifest_path
/// fall back to the working-tree-root `Cargo.toml` discovery — see
/// `check_red_green_with_manifest` for the r5 contract that requires
/// `--manifest-path`. New callers should prefer that signature.
///
/// Pre-conditions:
///   * `repo_root` is inside a git working tree
///   * `base_ref` resolves to a commit
///   * At least one `Test` path is present in `changed`
///
/// Post-conditions (regardless of return value):
///   * The working tree is restored to its pre-call state.
pub fn check_red_green(
    repo_root: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
) -> Result<RedGreenReport, RedGreenError> {
    check_red_green_with_manifest(repo_root, base_ref, changed, None)
}

/// Run the r5 red-green check with an explicit `manifest_path` (typically
/// `daemon/Cargo.toml` for the dark-factory repo). When `manifest_path`
/// is `Some`, cargo is invoked with `cargo test --manifest-path <m>`
/// so the test runner executes the PR's crate regardless of the caller's
/// cwd — issue #387 r5 finding 3: a bare `cargo test` from the repo root
/// ran `NEVER_RAN` against an unrelated crate and the gate accepted the
/// silent no-op.
///
/// Three-phase contract (issue #387 r5):
///   (a) **green-on-PR-head**: every targeted test passes on the working
///       tree (HEAD) BEFORE any revert. If this fails the report is
///       `Verdict::GreenFailed` and the gate stops — a test that doesn't
///       pass on HEAD can't tell us anything meaningful about the revert.
///   (b) **red-on-revert**: every targeted test passes after reverting the
///       production diff. The legacy vacuous detection: `Vacuous` when
///       all still pass, `Genuine` when at least one fails.
///   (c) **baseline-main sanity**: the same targeted tests pass on the
///       pristine `base_ref` (the tests are sound independently of the PR).
///       Fails closed to `Verdict::BaselineFailed` rather than reporting
///       `Vacuous` on a test that was broken before the PR existed.
pub fn check_red_green_with_manifest(
    repo_root: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
    manifest_path: Option<&Path>,
) -> Result<RedGreenReport, RedGreenError> {
    if changed.is_empty() {
        return Err(RedGreenError::NoChangedTests);
    }

    let test_files: Vec<PathBuf> = changed
        .iter()
        .filter(|(_, k)| *k == FileClass::Test)
        .map(|(p, _)| p.clone())
        .collect();
    if test_files.is_empty() {
        return Err(RedGreenError::NoChangedTests);
    }

    // Resolve manifest path. r5: callers that omit it fall back to a
    // walk-up-the-tree search; a missing manifest at this layer still
    // runs the gate (for backward compat with the legacy CLI), but the
    // report's `manifest_path` field is `None` so downstream consumers
    // can flag "this run was a fallback, not a real daemon flow".
    // jleechan-6xje / P0: when the cargo walker returns None, look for
    // a Python manifest so the gate stops silently returning
    // `ManifestMissing` on the 75% of factory traffic that is Python.
    let backend = match manifest_path {
        Some(p) => Backend::from_manifest_path(p),
        None => Backend::detect(repo_root)
            .ok_or_else(|| RedGreenError::ManifestMissing(no_manifest_reason(repo_root)))?,
    };
    let resolved_manifest: Option<PathBuf> = match manifest_path {
        Some(p) => Some(p.to_path_buf()),
        None => match backend {
            Backend::Cargo => find_cargo_manifest(repo_root),
            Backend::Pytest => find_pytest_manifest_recursive(repo_root, 4),
        },
    };

    // Bead jleechan-sb4b: resolve the cargo binary via the same fallback
    // chain the startup check uses. The detector cannot run if cargo is
    // missing, but we surface a structured `CargoNotFound` error instead
    // of the previous misleading `git error: spawn cargo test: No such
    // file or directory` so the gate reports a real cause.
    // jleechan-6xje: pytest backend has its own resolver. The orchestrator
    // picks the right one based on the backend choice above.
    let cargo_loc = resolve_cargo(cargo_home_from_env().as_deref());
    let pytest_loc = resolve_pytest(None);

    // Step 1: discover the diff-aware added/modified test fns + their
    // per-fn skip reasons across the changed test files. r6 contract
    // (issue #387 r6 P1 #5): scope at the fn level AND only emit fns that
    // were actually added or modified by this PR — not every `#[test]`
    // living in a changed test file. The base blob for each path is
    // fetched via `git show <base_ref>:<rel>` so we can compare fn bodies
    // (added fns = name missing from base; modified fns = same name in
    // both, different body). `#[ignore]` / `#[ignore = "..."]` populate
    // `skip_reason` (issue #387 r5 finding 4).
    // jleechan-6xje: the Python backend uses a parallel diff-aware
    // walker that ignores `#[ignore]` (Python has no equivalent baked
    // into the language; pytest uses `@pytest.mark.skip` which is a
    // future-extension seam).
    let mut targeted: BTreeSet<String> = BTreeSet::new();
    let mut pytest_targets: Vec<PytestTarget> = Vec::new();
    let mut skipped: Vec<TestFnInfo> = Vec::new();
    for path in &test_files {
        let head_src = std::fs::read_to_string(path).map_err(|e| {
            RedGreenError::Git(format!("read test file {}: {e}", path.display()))
        })?;
        let rel = relative_repo_path(repo_root, path);
        let base_src = match rel {
            Some(r) => read_base_blob(repo_root, base_ref, &r),
            None => None,
        };
        let (added_or_modified, skipped_local) = match backend {
            Backend::Cargo => compute_targeted_test_fns(base_src.as_deref(), &head_src),
            Backend::Pytest => {
                compute_targeted_python_test_fns(base_src.as_deref(), &head_src)
            }
        };
        for info in skipped_local {
            skipped.push(info);
        }
        for name in added_or_modified {
            if backend == Backend::Pytest {
                pytest_targets.push(PytestTarget {
                    path: path.clone(),
                    name: name.clone(),
                });
            }
            targeted.insert(name);
        }
    }
    pytest_targets.sort_by(|a, b| a.path.cmp(&b.path).then(a.name.cmp(&b.name)));
    pytest_targets.dedup();
    let targeted_tests: Vec<String> = targeted.iter().cloned().collect();

    // Phase (a) — green-on-PR-head. If the targeted tests don't pass
    // before any revert, the gate reports `GreenFailed` immediately.
    let head_pass = match backend {
        Backend::Cargo => run_cargo_tests(
            repo_root,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        )?,
        Backend::Pytest => run_pytest_tests(
            repo_root,
            &pytest_targets,
            &targeted_tests,
            resolved_manifest.as_deref(),
            pytest_loc.clone(),
        )?,
    };
    if !head_pass.all_passed() {
        return Ok(RedGreenReport {
            verdict: Verdict::GreenFailed,
            vacuous: false,
            failed_on_revert: 0,
            targeted_tests,
            failing_tests: head_pass.failing.clone(),
            skipped_tests: skipped,
            manifest_path: resolved_manifest,
        });
    }

    // Phase (c) — baseline-main sanity. We can't easily run the test
    // runner against a different commit in-place without disturbing the
    // working tree, so we use `git worktree add --detach` to materialize
    // `base_ref` in a temp dir, run the targeted tests there, and clean
    // up. This catches the "test was already broken before the PR" case
    // where the red-on-revert finding would otherwise be a false
    // positive.
    let baseline_pass = match backend {
        Backend::Cargo => run_baseline_check(
            repo_root,
            base_ref,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        )?,
        Backend::Pytest => run_pytest_baseline_check(
            repo_root,
            base_ref,
            &pytest_targets,
            &targeted_tests,
            resolved_manifest.as_deref(),
            pytest_loc.clone(),
        )?,
    };
    if !baseline_pass.all_passed() {
        return Ok(RedGreenReport {
            verdict: Verdict::BaselineFailed,
            vacuous: false,
            failed_on_revert: 0,
            targeted_tests,
            failing_tests: baseline_pass.failing.clone(),
            skipped_tests: skipped,
            manifest_path: resolved_manifest,
        });
    }

    // Phase (b) — red-on-revert. Stash the production diff, revert it,
    // run the test runner against the reverted tree, restore. The diff
    // capture + restore are best-effort wrappers; we panic-on-restore-fail
    // at the call site (`restore_diff`) so a partial revert never leaves
    // the tree dirty.
    let full_diff = capture_production_diff(repo_root, base_ref)?;
    let production_diff = filter_diff_for_paths(
        &full_diff,
        &changed
            .iter()
            .filter(|(_, k)| *k == FileClass::Production)
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>(),
    )?;

    apply_revert(repo_root, &production_diff)?;

    let revert_outcome = match backend {
        Backend::Cargo => run_cargo_tests(
            repo_root,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        ),
        Backend::Pytest => run_pytest_tests(
            repo_root,
            &pytest_targets,
            &targeted_tests,
            resolved_manifest.as_deref(),
            pytest_loc.clone(),
        ),
    };

    if let Err(e) = restore_diff(repo_root, &production_diff) {
        return Err(RedGreenError::RestoreFailed(format!(
            "{e}; original outcome suppressed to protect working tree"
        )));
    }

    let revert_outcome = revert_outcome?;
    let failing_on_revert = revert_outcome.failing.clone();

    let outcome = RunOutcome {
        green_on_head_ok: true,
        baseline_ok: true,
        failing_on_revert,
    };
    let verdict = outcome.verdict();
    Ok(RedGreenReport {
        verdict,
        vacuous: verdict == Verdict::Vacuous,
        failed_on_revert: revert_outcome.failing.len(),
        targeted_tests,
        failing_tests: revert_outcome.failing,
        skipped_tests: skipped,
        manifest_path: resolved_manifest,
    })
}

/// Build a human-readable reason for the gate when neither backend
/// could find a manifest. The string is what
/// `verifier::VacuousRedGreenStatus::ManifestMissing` shows operators
/// in the PR evidence, so it should name both backend attempts.
fn no_manifest_reason(repo_root: &Path) -> String {
    format!(
        "no Cargo.toml or pyproject.toml/pytest.ini reachable from {} (walk-up + recursive depth-4 both failed on both backends)",
        repo_root.display()
    )
}

/// Scan a Rust source file for `#[test] fn <name>(` declarations.
/// Multi-attribute test fns (e.g. `#[tokio::test]`) are supported by
/// looking one line above the `fn` for a `#[` line containing `test`.
fn discover_test_fns(source: &str) -> Vec<String> {
    discover_test_fns_with_skip(source)
        .into_iter()
        .map(|i| i.name)
        .collect()
}

/// Scan a Rust source file for `#[test] fn <name>(` declarations and
/// record whether the test is `#[ignore]` (cargo will skip it on
/// default runs). Returns one `TestFnInfo` per discovered fn — the
/// `skip_reason` is `Some(reason)` when cargo would skip this test
/// because of `#[ignore]` / `#[ignore = "..."]`, `None` otherwise.
///
/// r5 finding 4: silently counting an `#[ignore]`-marked test as
/// "all green on revert" was the bug — the test never ran, so the
/// detector was producing vacuous=true for free. We still surface
/// these tests (so operators can see why) but mark them as
/// `skip_reason != None`.
///
/// Public so the integration test suite under `tests/` can exercise
/// the fn-level scoping contract without paying for a `cargo test`
/// round-trip on every CI run.
pub fn discover_test_fns_with_skip(source: &str) -> Vec<TestFnInfo> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if !(trimmed.starts_with("#[test]") || trimmed.starts_with("#[tokio::test]") || trimmed.starts_with("#[rstest]")) {
            i += 1;
            continue;
        }
        // Walk forward collecting every `#[...]` attribute line until we
        // see the `fn`. Any `#[ignore]` / `#[ignore = "..."]` in this
        // run attaches to the test fn below it.
        let mut j = i + 1;
        let mut ignore_reason: Option<String> = None;
        while j < lines.len() {
            let t = lines[j].trim_start();
            if t.starts_with('#') {
                if t.starts_with("#[ignore") {
                    ignore_reason = parse_ignore_reason(t);
                }
                j += 1;
                continue;
            }
            if t.is_empty() || t.starts_with("//") {
                j += 1;
                continue;
            }
            if let Some(name) = parse_test_fn_name(t) {
                out.push(TestFnInfo {
                    name,
                    skip_reason: ignore_reason.clone(),
                });
            }
            break;
        }
        i = j.max(i + 1);
    }
    out
}

/// Parse `#[ignore]` / `#[ignore = "reason"]` into a `Some(reason)`.
/// Default reason (`#[ignore]` without `=`) is the literal string
/// `"#[ignore]"` so the report shows the skip was unannotated.
fn parse_ignore_reason(line: &str) -> Option<String> {
    let after = line
        .trim_start_matches("#[ignore")
        .trim_start();
    if after.is_empty() || after.starts_with(']') {
        return Some("#[ignore]".to_string());
    }
    // Expect ` = "reason"`.
    let after_eq = after.trim_start_matches('=').trim_start();
    let quoted = after_eq.strip_prefix('"')?;
    let end = quoted.find('"')?;
    Some(quoted[..end].to_string())
}

fn parse_test_fn_name(line: &str) -> Option<String> {
    let idx = line.find("fn ")?;
    let rest = &line[idx + 3..];
    let after_fn = rest.trim_start();
    let name_end = after_fn
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
        .last()
        .map(|(i, _)| i + 1)
        .unwrap_or(0);
    if name_end == 0 {
        return None;
    }
    Some(after_fn[..name_end].to_string())
}

/// Strip the daemon cwd prefix from an absolute path used in `git show
/// <base_ref>:<path>`. Returns `None` when `path` is not under
/// `repo_root` (the caller will then skip the base-blob fetch and fall
/// back to "treat every head fn as added").
fn relative_repo_path(repo_root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(repo_root)
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}

/// Fetch the test-file blob at `base_ref:<rel_path>` from git. Returns
/// `None` when the file does not exist on the base (a brand-new test
/// file is purely "added", which the diff-aware scoping handles by
/// emitting every head-side fn). A failed `git show` for any other
/// reason (corrupt repo, revoked ref) is propagated as `Some("")` so
/// the downstream parse simply sees an empty base file — every head fn
/// still classifies as "added".
fn read_base_blob(repo_root: &Path, base_ref: &str, rel_path: &str) -> Option<String> {
    let spec = format!("{base_ref}:{rel_path}");
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(["show", &spec])
        .output()
        .ok()?;
    if !out.status.success() {
        // File did not exist on base, or ref is unresolvable. Treat as
        // "no base content" — every head fn will look new.
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Diff-aware scoping (issue #387 r6 P1 #5): given the parsed HEAD-side
/// `#[test]` fns (with skip_reasons) and the optional base-side source
/// for the same file, return `(targeted_names, skipped_records)` where
/// `targeted_names` contains exactly those head-side fns that were
/// added OR modified by this PR. Pre-existing fns whose body bytes are
/// identical to base are excluded. A `None` base_src (file did not
/// exist before the PR) classifies every head fn as added.
///
/// The "added" detection is by-name: a fn is added iff its name is not
/// present in the base parser. The "modified" detection is by body: a
/// fn with the same name in both is modified iff the byte slice
/// between `fn <name>(` and its matching `}` differs between base and
/// head. The matching brace is found by counting nested `{`/`}` inside
/// the fn body — sufficient for the test-fn shapes cargo recognises.
pub fn compute_targeted_test_fns(
    base_src: Option<&str>,
    head_src: &str,
) -> (Vec<String>, Vec<TestFnInfo>) {
    let head_fns = discover_test_fns_with_skip(head_src);
    if head_fns.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let base_fns = match base_src {
        None => Vec::new(),
        Some(src) => discover_test_fns_with_skip(src),
    };
    let base_names: BTreeSet<&str> = base_fns.iter().map(|i| i.name.as_str()).collect();
    // For fns present in both, we need the body bytes to detect "modified".
    // Index base-side bodies once by name so the loop below is O(head).
    let mut base_body_by_name: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    if let Some(src) = base_src {
        for (start, end, name) in fn_bodies_iter(src) {
            base_body_by_name.insert(name, &src[start..end]);
        }
    }
    let mut head_body_by_name: std::collections::HashMap<&str, (usize, usize)> =
        std::collections::HashMap::new();
    for (start, end, name) in fn_bodies_iter(head_src) {
        head_body_by_name.insert(name, (start, end));
    }

    let mut targeted: Vec<String> = Vec::new();
    let mut skipped: Vec<TestFnInfo> = Vec::new();

    for info in head_fns {
        if base_src.is_none() {
            // Brand-new file: every head fn is added.
            targeted.push(info.name.clone());
            if let Some(reason) = info.skip_reason {
                skipped.push(TestFnInfo {
                    name: info.name,
                    skip_reason: Some(reason),
                });
            }
            continue;
        }
        if !base_names.contains(info.name.as_str()) {
            // Added: name not in base.
            targeted.push(info.name.clone());
            if let Some(reason) = info.skip_reason {
                skipped.push(TestFnInfo {
                    name: info.name,
                    skip_reason: Some(reason),
                });
            }
            continue;
        }
        // Existing fn in both — compare bodies.
        let head_body = head_body_by_name
            .get(info.name.as_str())
            .copied()
            .map(|(s, e)| &head_src[s..e]);
        let base_body = base_body_by_name.get(info.name.as_str()).copied();
        if head_body != base_body {
            targeted.push(info.name.clone());
            if let Some(reason) = info.skip_reason {
                skipped.push(TestFnInfo {
                    name: info.name,
                    skip_reason: Some(reason),
                });
            }
        }
        // Unchanged fns are intentionally dropped — issue #387 r6 P1 #5.
    }

    (targeted, skipped)
}

/// Iterate `(body_start, body_end, name)` for every `fn <name>(...)`
/// in `source`, where body bytes run from immediately AFTER the opening
/// `{` of the fn body to the matching closing `}` (exclusive). Used by
/// `compute_targeted_test_fns` to compare per-fn bodies between base
/// and head. Stops at EOF if the matching `}` is missing.
fn fn_bodies_iter(source: &str) -> Vec<(usize, usize, &str)> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < len {
        // Look for the literal sequence `fn <name>(`. The simplest
        // search is over byte offsets — Rust source is ASCII for our
        // purposes (test files don't embed multi-byte identifiers).
        if let Some(rel) = find_subslice(&source[i..], b"fn ") {
            let fn_kw = i + rel;
            let after = fn_kw + 3;
            // Parse the name.
            let mut name_end = after;
            while name_end < len {
                let c = bytes[name_end];
                if c.is_ascii_alphanumeric() || c == b'_' {
                    name_end += 1;
                } else {
                    break;
                }
            }
            if name_end == after {
                i = after;
                continue;
            }
            let name = &source[after..name_end];
            // Skip past the parameter list `(...)`.
            let mut p = name_end;
            if p < len && bytes[p] == b'(' {
                let mut depth = 1;
                p += 1;
                while p < len && depth > 0 {
                    match bytes[p] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    p += 1;
                }
            }
            // Skip return type + where clauses until we hit `{`.
            let mut brace = p;
            while brace < len && bytes[brace] != b'{' {
                brace += 1;
            }
            if brace >= len {
                break;
            }
            // Walk to matching `}`.
            let body_start = brace + 1;
            let mut depth = 1;
            let mut j = body_start;
            while j < len && depth > 0 {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if j > body_start {
                out.push((body_start, j.saturating_sub(1), name));
            }
            i = j;
        } else {
            break;
        }
    }
    out
}

fn find_subslice(haystack: &str, needle: &[u8]) -> Option<usize> {
    haystack.as_bytes().windows(needle.len()).position(|w| w == needle)
}

fn capture_production_diff(
    repo_root: &Path,
    base_ref: &str,
) -> Result<Vec<u8>, RedGreenError> {
    // Capture the full diff between `base_ref` and the current working
    // tree (working tree includes any uncommitted edits). This is what
    // we will revert in step 3. Note: `git diff <commit>` already covers
    // both staged and unstaged changes against the working tree, so this
    // works whether or not the caller has committed.
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(["diff", "--binary", "--no-color", base_ref])
        .output()
        .map_err(|e| RedGreenError::Git(format!("spawn git diff: {e}")))?;
    if !out.status.success() {
        return Err(RedGreenError::Git(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

fn apply_revert(repo_root: &Path, diff: &[u8]) -> Result<(), RedGreenError> {
    if diff.is_empty() {
        return Ok(());
    }
    // git apply -R reverses the patch in-memory.
    let mut child = Command::new("git")
        .current_dir(repo_root)
        .args(["apply", "--reverse", "--binary"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RedGreenError::Git(format!("spawn git apply: {e}")))?;
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(diff)
        .map_err(|e| RedGreenError::Git(format!("write to git apply stdin: {e}")))?;
    let out = child
        .wait_with_output()
        .map_err(|e| RedGreenError::Git(format!("wait git apply: {e}")))?;
    if !out.status.success() {
        return Err(RedGreenError::RevertFailed(format!(
            "{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Filter a `git diff` blob to keep only the hunks whose `diff --git`
/// header references one of `keep_paths`. Used to drop test-file hunks
/// from a revert patch so the test target stays discoverable by cargo.
///
/// Implementation note: a git diff is a sequence of `diff --git a/<p>
/// b/<p>` headers followed by `--` / `++` / hunk lines. We split on the
/// header, take the path (second column, stripping `b/` prefix), and
/// emit hunks whose path is in `keep_paths`. Path matching uses
/// `ends_with` on the suffix so callers can pass either absolute or
/// repo-relative paths.
fn filter_diff_for_paths(
    diff: &[u8],
    keep_paths: &[PathBuf],
) -> Result<Vec<u8>, RedGreenError> {
    if diff.is_empty() {
        return Ok(Vec::new());
    }
    let text = std::str::from_utf8(diff)
        .map_err(|e| RedGreenError::Git(format!("diff is not UTF-8: {e}")))?;
    let mut out = String::with_capacity(text.len());
    let mut current_block = String::new();
    let mut keep_block = false;
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            // Decide whether to keep the prior block.
            if keep_block && !current_block.is_empty() {
                out.push_str(&current_block);
            }
            current_block.clear();
            // Header line: `diff --git a/<p> b/<p>` — extract the path.
            let parts: Vec<&str> = line.split_whitespace().collect();
            // parts: ["diff", "--git", "a/<p>", "b/<p>"]
            let path_b = parts.get(3).copied().unwrap_or("");
            let path = path_b.trim_start_matches("b/").trim_end();
            keep_block = keep_paths.iter().any(|kp| {
                let kp_str = kp.to_string_lossy();
                let kp_norm = kp_str.trim_start_matches('/').trim_start_matches("./");
                kp_str == path
                    || kp_norm == path
                    || path.ends_with(kp_norm)
                    || kp_str.ends_with(path)
            });
            current_block.push_str(line);
        } else {
            current_block.push_str(line);
        }
    }
    if keep_block {
        out.push_str(&current_block);
    }
    Ok(out.into_bytes())
}

fn restore_diff(repo_root: &Path, diff: &[u8]) -> Result<(), RedGreenError> {
    if diff.is_empty() {
        return Ok(());
    }
    // Re-applying the original diff brings the production files back.
    let mut child = Command::new("git")
        .current_dir(repo_root)
        .args(["apply", "--binary"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RedGreenError::Git(format!("spawn git apply: {e}")))?;
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(diff)
        .map_err(|e| RedGreenError::Git(format!("write to git apply stdin: {e}")))?;
    let out = child
        .wait_with_output()
        .map_err(|e| RedGreenError::Git(format!("wait git apply: {e}")))?;
    if !out.status.success() {
        return Err(RedGreenError::RestoreFailed(format!(
            "{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Outcome of a single `cargo test` invocation. `failing` lists test
/// names that FAILED OR NEVER RAN — issue #387 r5 finding 3: NEVER_RAN
/// used to be counted as a real pass on the dark-factory layout, which
/// was the root cause of the silent vacuous-pass bug.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CargoOutcome {
    failing: Vec<String>,
    compile_errored: bool,
}

/// A pytest node keeps its file association. Names alone are insufficient:
/// two changed files may both define `test_parse`, and executing a Cartesian
/// product can report one passing file while silently missing the other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PytestTarget {
    path: PathBuf,
    name: String,
}

impl CargoOutcome {
    fn all_passed(&self) -> bool {
        // A compile failure on the reverted tree is the strongest
        // "test exercises real production code" signal — the test must
        // have referenced a symbol that disappeared with the revert. We
        // synthesize a failure entry for it so callers don't miss the
        // signal even though cargo never got to the assertion phase.
        if self.compile_errored && self.failing.is_empty() {
            return false;
        }
        self.failing.is_empty()
    }
}

/// Walk up from `repo_root` looking for a `Cargo.toml`. Returns the
/// first one found, or `None` if none exist. Used by the legacy
/// `check_red_green` shim and as a sanity check before invoking
/// `cargo test` (issue #387 r5 finding 3: a bare `cargo test` from
/// the repo root silently ran unrelated tests on dark-factory's
/// nested-crate layout; without a manifest, every test "passed"
/// because cargo found nothing to run, and the gate approved the
/// vacuous PR).
///
/// Public so the production tick path (`tick.rs::vacuous_red_green_for_pr`)
/// can sanity-check the daemon's CWD before invoking the detector and
/// surface a `ManifestMissing` status rather than letting `cargo test`
/// silently run against the wrong crate.
pub fn find_cargo_manifest(repo_root: &Path) -> Option<PathBuf> {
    let mut cur = repo_root.to_path_buf();
    loop {
        let candidate = cur.join("Cargo.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Bounded recursive manifest search (jleechan-ni1k / issue #437 bonus).
/// The walk-up `find_cargo_manifest` cannot find a manifest when the
/// crate lives at a path BELOW the daemon's CWD (dark-factory's
/// `daemon/Cargo.toml` is the canonical example: the repo root has no
/// `Cargo.toml`, so the walk-up returns `None` and the detector surfaces
/// `ManifestMissing: no Cargo.toml reachable from /home/jleechan/projects/
/// dark-factory`). This helper walks the directory tree to a fixed
/// `max_depth` and returns the FIRST `Cargo.toml` it finds, skipping
/// `target`, `node_modules`, `.git`, and `Cargo.lock`-only directories
/// (a `Cargo.lock` without a sibling `Cargo.toml` is not a crate).
///
/// Why bounded: a malicious or unusually deep tree (e.g. an attacker
/// uploads a 1000-entry `node_modules`-shaped payload) cannot cost real
/// seconds per tick. The cap at 4 covers dark-factory's layout
/// (`<repo_root>/daemon/Cargo.toml`) with headroom.
///
/// Order matters: the root `Cargo.toml` is checked first (preserving the
/// legacy walk-up fast path for single-crate repos), then a depth-first
/// walk of immediate children, then depth-2, etc. We return on the first
/// match — the daemon only needs ONE manifest to gate the PR.
pub fn find_cargo_manifest_recursive(repo_root: &Path, max_depth: usize) -> Option<PathBuf> {
    // Skip noisy subtrees — these almost never host the crate manifest
    // and add entries to every tick's traversal cost. `target` is rustc
    // build output, `node_modules` is JS, `.git` is the VCS database.
    const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git"];

    // Depth 0: the root itself (fast path; preserves legacy walk-up
    // behavior for single-crate repos).
    let root_candidate = repo_root.join("Cargo.toml");
    if root_candidate.exists() {
        return Some(root_candidate);
    }

    // Depth 1..=max_depth: BFS so the first match is the shallowest one.
    let mut frontier: Vec<PathBuf> = std::iter::once(repo_root.to_path_buf()).collect();
    for _depth in 1..=max_depth {
        let mut next_frontier: Vec<PathBuf> = Vec::with_capacity(frontier.len() * 4);
        for dir in &frontier {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue, // unreadable dir (perms, vanished) — skip silently
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if SKIP_DIRS.iter().any(|s| *s == name_str) {
                    continue;
                }
                let candidate = p.join("Cargo.toml");
                if candidate.exists() {
                    return Some(candidate);
                }
                next_frontier.push(p);
            }
        }
        frontier = next_frontier;
    }
    None
}

/// Run cargo test against the working tree (or a worktree under
/// `baseline_root` for the baseline-main phase) using the resolved
/// manifest. Issue #387 r5 finding 3: `--manifest-path` is required
/// so cargo executes the PR's crate regardless of cwd — without it,
/// `NEVER_RAN` is treated as a real pass on multi-crate layouts.
///
/// Bead jleechan-sb4b: `cargo` is the binary chosen to run the tests;
/// when PATH lacks cargo (the systemd-unit daemon context that surfaced
/// this bead), the caller must supply a `CargoLocation` resolved via
/// `resolve_cargo` so the detector can fall back to
/// `$HOME/.cargo/bin/cargo` or `rustup which cargo`. When `cargo_loc`
/// is `NotFound`, the detector returns `RedGreenError::CargoNotFound`
/// instead of a misleading `git error: spawn cargo test: No such file or
/// directory` (the previous failure mode).
fn run_cargo_tests(
    repo_root: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
    manifest: Option<&Path>,
    cargo_loc: CargoLocation,
) -> Result<CargoOutcome, RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok(CargoOutcome {
            failing: vec![],
            compile_errored: false,
        });
    }

    let cargo_bin = match &cargo_loc {
        CargoLocation::OnPath => PathBuf::from("cargo"),
        CargoLocation::Found(p) => p.clone(),
        CargoLocation::NotFound => {
            return Err(RedGreenError::CargoNotFound(
                "cargo was not found on PATH, in $HOME/.cargo/bin/cargo, \
                 or via `rustup which cargo`; install the toolchain or \
                 set CARGO_HOME so the runtime detector can run."
                    .to_string(),
            ));
        }
    };

    let mut failing: Vec<String> = Vec::new();
    let mut compile_errored = false;

    for tf in test_files {
        let basename = tf
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| RedGreenError::Git(format!("bad test file path: {}", tf.display())))?;

        let mut args: Vec<String> = vec![
            "test".to_string(),
            "--quiet".to_string(),
            "--test".to_string(),
            basename.to_string(),
        ];
        if let Some(m) = manifest {
            args.push("--manifest-path".to_string());
            args.push(m.to_string_lossy().into_owned());
        }
        for name in targeted_tests {
            args.push("--".to_string());
            args.push(name.clone());
            args.push("--exact".to_string());
        }

        let out = Command::new(&cargo_bin)
            .current_dir(repo_root)
            .args(&args)
            .output()
            .map_err(|e| {
                RedGreenError::CargoNotFound(format!(
                    "spawn {}: {e}; cargo binary not usable from this environment",
                    cargo_bin.display()
                ))
            })?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        // Cargo surfaces compile errors as `error[E0...]:` on stderr or
        // stdout. If we see one AND exit was non-zero AND no per-test PASS
        // lines were emitted, the test never compiled — which is the
        // strongest possible "production code is being exercised" signal.
        if !out.status.success()
            && (stderr.contains("error[E") || stdout.contains("error[E"))
            && !stdout.contains(" ... ok")
        {
            compile_errored = true;
        }

        // Parse cargo test's per-test PASS/FAIL summary lines.
        for name in targeted_tests {
            let passed_marker = format!("test {name} ... ok");
            let failed_marker = format!("test {name} ... FAILED");
            let ignored_marker = format!("test {name} ... ignored");
            if stdout.contains(&failed_marker) || stderr.contains(&failed_marker) {
                failing.push(name.clone());
            } else if !(stdout.contains(&passed_marker) || stdout.contains(&ignored_marker)) {
                // If neither PASS nor FAIL nor IGNORED is present, the test
                // didn't run at all — treat that as a hard fail signal so
                // the gate doesn't accidentally approve a test that was
                // skipped. Issue #387 r5 finding 3: this used to be
                // treated as a real pass on the dark-factory layout when
                // --manifest-path was omitted.
                failing.push(format!("{name}:NEVER_RAN"));
            }
        }
    }

    Ok(CargoOutcome {
        failing,
        compile_errored,
    })
}

/// Run phase (c) — baseline-main sanity check — by materializing
/// `base_ref` in a temporary git worktree, running the targeted tests
/// there against the resolved manifest, and cleaning up. Returns
/// `BaselineFailed` (as a hard error) when the worktree setup itself
/// fails, so the caller can surface it rather than silently reporting
/// `Vacuous`.
fn run_baseline_check(
    repo_root: &Path,
    base_ref: &str,
    test_files: &[PathBuf],
    targeted_tests: &[String],
    manifest: Option<&Path>,
    cargo_loc: CargoLocation,
) -> Result<CargoOutcome, RedGreenError> {
    // Build a temp worktree directory for the pristine base. We use
    // `git worktree add --detach` so the original working tree is
    // untouched and the temp dir is removed on cleanup.
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_baseline_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let add = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "add",
            "--detach",
            "--quiet",
            tmp.to_string_lossy().as_ref(),
            base_ref,
        ])
        .output()
        .map_err(|e| RedGreenError::Git(format!("spawn git worktree: {e}")))?;
    if !add.status.success() {
        return Err(RedGreenError::BaselineFailed(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }

    // Resolve the manifest path relative to the worktree root if the
    // caller passed a relative manifest — manifests passed in are
    // typically repo-relative (e.g. "daemon/Cargo.toml"), and the
    // worktree uses the same relative layout.
    let baseline_manifest = manifest.map(|m| {
        if m.is_absolute() {
            m.to_path_buf()
        } else {
            tmp.join(m)
        }
    });

    let result = run_cargo_tests(&tmp, test_files, targeted_tests, baseline_manifest.as_deref(), cargo_loc);

    // Always clean up the worktree, even on error. We swallow cleanup
    // errors — the test outcome is the primary signal; a stale /tmp
    // worktree is a leak, not a defect.
    let _ = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "remove",
            "--force",
            tmp.to_string_lossy().as_ref(),
        ])
        .output();
    let _ = std::fs::remove_dir_all(&tmp);

    result
}

/// Pytest analogue of `run_cargo_tests`. Runs the targeted tests
/// against the working tree (or against a baseline worktree when
/// invoked from `run_pytest_baseline_check`). Returns a
/// `CargoOutcome` so the orchestrator can keep using a single
/// per-phase shape — `failing` lists test names that failed or
/// never ran, `compile_errored` is repurposed to mean "collection
/// failed" (pytest's equivalent of a Rust compile error).
fn run_pytest_tests(
    repo_root: &Path,
    pytest_targets: &[PytestTarget],
    targeted_tests: &[String],
    manifest: Option<&Path>,
    pytest_loc: PytestLocation,
) -> Result<CargoOutcome, RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok(CargoOutcome {
            failing: vec![],
            compile_errored: false,
        });
    }

    let pytest_bin = match &pytest_loc {
        PytestLocation::OnPath => PathBuf::from("pytest"),
        PytestLocation::Found(p) => p.clone(),
        PytestLocation::NotFound => {
            return Err(RedGreenError::PytestNotFound(
                "pytest was not found on PATH or in a venv adjacent to the supplied python; \
                 install pytest (`pip install pytest` / `uv pip install pytest`) or set \
                 PYTHON so the runtime detector can locate it."
                    .to_string(),
            ));
        }
    };

    let mut failing: Vec<String> = Vec::new();
    let mut compile_errored = false;

    // Build the per-test selector list once. pytest's targeted
    // invocation accepts `--collect-only -q` to enumerate the
    // would-be-run set, but for the runtime check we just pass
    // `<file>::<test_name>` nodes directly. Multiple files in
    // a single pytest invocation is fine; pytest collects them
    // all.
    // Pytest reports node IDs relative to the effective project root. For a
    // nested pyproject, that root is the manifest's parent rather than the
    // daemon target-worktree root; selectors and parsing must use the same
    // coordinate system.
    let pytest_root = manifest
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.eq_ignore_ascii_case("pyproject.toml"))
                .unwrap_or(false)
        })
        .and_then(Path::parent)
        .unwrap_or(repo_root);
    let mut selector_args: Vec<String> = Vec::new();
    for target in pytest_targets {
        // Selectors and pytest's verbose node IDs share the manifest-root
        // coordinate system.  Passing a nested-project path such as
        // `mvp_site/tests/test_value.py` while running from the repository
        // root makes pytest report `tests/test_value.py::...`, so the
        // parser would classify a passing target as NEVER_RAN.  Construct
        // the selector relative to the same root used for output parsing.
        let rel = relative_repo_path(pytest_root, &target.path).unwrap_or_else(|| {
            target.path.to_string_lossy().into_owned()
        });
        selector_args.push(format!("{rel}::{}", target.name));
    }

    let mut args: Vec<String> = vec![
        // `-v` (verbose) is required so pytest emits per-test
        // `::test_x PASSED` lines on stdout. The detector's
        // pass/fail parser keys off those lines; with `-q`
        // pytest only emits a one-line summary, which would mark
        // every targeted test as NEVER_RAN and trip the r5
        // finding-3 fail-closed contract.
        "-v".to_string(),
        "--no-header".to_string(),
        "--tb=line".to_string(),
        "-p".to_string(),
        "no:cacheprovider".to_string(),
    ];

    // When the manifest is given AND it is a real `pyproject.toml`,
    // we pass it as the rootdir so pytest picks up the right
    // `[tool.pytest.ini_options]` for the project. We deliberately
    // do NOT pass `--rootdir` for `pytest.ini`/`setup.cfg` —
    // pytest's own discovery handles those natively when the
    // working dir is the project root.
    if let Some(m) = manifest {
        let name = m
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name == "pyproject.toml" {
            args.push("--rootdir".to_string());
            args.push(
                m.parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| m.to_string_lossy().into_owned()),
            );
        }
    }

    args.extend(selector_args);

    let out = Command::new(&pytest_bin)
        // A nested pyproject is a standalone pytest project.  Running from
        // its manifest parent makes the repo-relative selectors above valid
        // across pytest versions while preserving root-level behavior.
        .current_dir(pytest_root)
        .args(&args)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTEST_DISABLE_PLUGIN_AUTOLOAD", "1")
        .output()
        .map_err(|e| {
            RedGreenError::PytestNotFound(format!(
                "spawn {pytest_bin:?}: {e}; pytest binary not usable from this environment",
                pytest_bin = pytest_bin
            ))
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Any non-zero pytest exit is a hard failure, even when one target's
    // stdout line says PASSED. This catches collection/import errors in a
    // sibling target and prevents a same-name test in another file from
    // masking it.
    if !out.status.success() {
        compile_errored = true;
        failing.push(format!(
            "pytest process failed (rc={}): {}",
            out.status.code().unwrap_or(-1),
            stderr.lines().next().unwrap_or("unknown pytest error")
        ));
    }

    // Parse pytest's per-test PASS/FAIL summary lines. pytest's
    // short summary format prints one line per test in the form:
    //   `tests/test_scenario.py::test_classify_high PASSED`
    //   `tests/test_scenario.py::test_classify_high FAILED`
    //   `tests/test_scenario.py::test_classify_high SKIPPED`
    for target in pytest_targets {
        let rel = relative_repo_path(pytest_root, &target.path).unwrap_or_else(|| {
            target.path.to_string_lossy().into_owned()
        });
        let node = format!("{rel}::{}", target.name);
        let passed = pytest_output_has_status(&stdout, &node, "PASSED");
        let failed = pytest_output_has_status(&stdout, &node, "FAILED");
        let skipped = pytest_output_has_status(&stdout, &node, "SKIPPED");
        let errored = pytest_output_has_status(&stdout, &node, "ERROR");
        if failed || errored {
            failing.push(format!("{node}:FAILED"));
        } else if !passed && !skipped {
            // If neither PASS nor FAIL nor SKIP is recorded, the
            // test was not collected by pytest (e.g. the file
            // failed to import, the test name was mistyped, or the
            // selector didn't match). Treat that as a hard fail
            // signal — issue #387 r5 finding 3 (cargo analogue):
            // NEVER_RAN must NOT be silently counted as a real
            // pass.
            failing.push(format!("{node}:NEVER_RAN"));
        }
    }

    Ok(CargoOutcome {
        failing,
        compile_errored,
    })
}

/// Match a pytest verbose result for one exact selected function.  Pytest
/// appends parameter IDs (`node[param] STATUS`) for parametrized tests; the
/// suffix must be accepted without relaxing file/function association (so
/// `test_x` cannot match `test_xyz`).
fn pytest_output_has_status(stdout: &str, node: &str, status: &str) -> bool {
    stdout.lines().any(|line| {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(node) else {
            return false;
        };

        let has_status = |candidate: &str| {
            let candidate = candidate.trim_start();
            let Some(tail) = candidate.strip_prefix(status) else {
                return false;
            };
            tail.is_empty()
                || tail
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_whitespace())
        };

        if rest.starts_with('[') {
            // The progress suffix (`[ 33%]`) can contain a later closing
            // bracket than the parameter ID.  Accept the bracket whose
            // following token is the requested terminal status instead of
            // blindly taking the last `]`.
            return rest
                .char_indices()
                .filter(|(_, character)| *character == ']')
                .any(|(close, _)| has_status(&rest[close + 1..]));
        }
        has_status(rest)
    })
}

/// Pytest analogue of `run_baseline_check`. Materializes `base_ref`
/// in a temp worktree, runs the targeted pytest tests there, and
/// cleans up. Mirrors the cargo version's contract: the worktree is
/// always removed even on error.
fn run_pytest_baseline_check(
    repo_root: &Path,
    base_ref: &str,
    pytest_targets: &[PytestTarget],
    targeted_tests: &[String],
    manifest: Option<&Path>,
    pytest_loc: PytestLocation,
) -> Result<CargoOutcome, RedGreenError> {
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_pytest_baseline_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let add = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "add",
            "--detach",
            "--quiet",
            tmp.to_string_lossy().as_ref(),
            base_ref,
        ])
        .output()
        .map_err(|e| RedGreenError::Git(format!("spawn git worktree: {e}")))?;
    if !add.status.success() {
        return Err(RedGreenError::BaselineFailed(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }

    // `test_files` and `manifest` are resolved against the PR target
    // worktree. Rebase them into the detached baseline before invoking
    // pytest; retaining absolute head paths here would silently execute the
    // PR checkout during the baseline phase and invalidate red/green's
    // baseline contract.
    let baseline_targets: Vec<PytestTarget> = pytest_targets
        .iter()
        .map(|target| PytestTarget {
            path: rebase_worktree_path(repo_root, &tmp, &target.path),
            name: target.name.clone(),
        })
        .collect();
    for (target, baseline_target) in pytest_targets.iter().zip(&baseline_targets) {
        // Newly added test files do not exist in the detached base. Copy only
        // that HEAD test source into the baseline so the phase can execute it
        // against base production/configuration; existing test files remain
        // the base revision, preserving genuine red/green semantics.
        if !baseline_target.path.exists() && target.path.is_file() {
            if let Some(parent) = baseline_target.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    RedGreenError::BaselineFailed(format!(
                        "create baseline test parent {}: {e}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&target.path, &baseline_target.path).map_err(|e| {
                RedGreenError::BaselineFailed(format!(
                    "copy new head test {} into baseline {}: {e}",
                    target.path.display(),
                    baseline_target.path.display()
                ))
            })?;
        } else if baseline_target.path.is_file() && target.path.is_file() {
            // A newly added test function in an existing module is absent from
            // the detached base file even though the file itself exists.  If
            // we leave it untouched, pytest reports "not found" and the
            // baseline phase becomes BaselineFailed -> Green, bypassing gate
            // 8.  Materialize only the newly-added function(s), preserving
            // the base module's production-facing imports and existing tests.
            let rel = relative_repo_path(repo_root, &target.path);
            let base_src = rel.and_then(|path| read_base_blob(repo_root, base_ref, &path));
            let head_src = std::fs::read_to_string(&target.path).map_err(|e| {
                RedGreenError::BaselineFailed(format!(
                    "read head pytest module {}: {e}",
                    target.path.display()
                ))
            })?;
            let base_names: BTreeSet<String> = base_src
                .as_deref()
                .map(discover_python_test_fns)
                .unwrap_or_default()
                .into_iter()
                .collect();
            if !base_names.contains(&target.name) {
                let snippet = extract_python_test_fn(&head_src, &target.name).ok_or_else(|| {
                    RedGreenError::BaselineFailed(format!(
                        "cannot materialize added pytest function {} from {}",
                        target.name,
                        target.path.display()
                    ))
                })?;
                let mut baseline_src = std::fs::read_to_string(&baseline_target.path).map_err(|e| {
                    RedGreenError::BaselineFailed(format!(
                        "read baseline pytest module {}: {e}",
                        baseline_target.path.display()
                    ))
                })?;
                if !baseline_src.ends_with('\n') {
                    baseline_src.push('\n');
                }
                baseline_src.push('\n');
                baseline_src.push_str(&snippet);
                if !baseline_src.ends_with('\n') {
                    baseline_src.push('\n');
                }
                std::fs::write(&baseline_target.path, baseline_src).map_err(|e| {
                    RedGreenError::BaselineFailed(format!(
                        "materialize added pytest function {} into {}: {e}",
                        target.name,
                        baseline_target.path.display()
                    ))
                })?;
            }
        }
    }
    let baseline_manifest = manifest
        .map(|path| rebase_worktree_path(repo_root, &tmp, path))
        .filter(|path| path.is_file());

    let result = run_pytest_tests(
        &tmp,
        &baseline_targets,
        targeted_tests,
        baseline_manifest.as_deref(),
        pytest_loc,
    );

    // Always clean up the worktree, even on error. We swallow cleanup
    // errors — the test outcome is the primary signal; a stale /tmp
    // worktree is a leak, not a defect.
    let _ = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "remove",
            "--force",
            tmp.to_string_lossy().as_ref(),
        ])
        .output();
    let _ = std::fs::remove_dir_all(&tmp);

    result
}

/// Extract one top-level pytest function, including immediately preceding
/// decorators, from a module. This deliberately stays line-oriented like the
/// discovery scanner above; pytest itself remains the parser of record.
fn extract_python_test_fn(source: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut fn_index = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(declaration) = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "))
        else {
            continue;
        };
        let candidate = declaration
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next()
            .unwrap_or_default();
        if candidate == name && line.len() == trimmed.len() {
            fn_index = Some(index);
            break;
        }
    }
    let fn_index = fn_index?;
    let mut start = fn_index;
    while start > 0 {
        let previous = lines[start - 1].trim_start();
        if previous.starts_with('@') {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = fn_index + 1;
    while end < lines.len() {
        let line = lines[end];
        let trimmed = line.trim_start();
        if !trimmed.is_empty() && line.len() == trimmed.len() {
            break;
        }
        end += 1;
    }
    Some(lines[start..end].join("\n"))
}

/// Rebase a path resolved in `repo_root` into a detached worktree. Relative
/// paths are interpreted from the repository root; absolute paths outside the
/// repository are preserved because callers may intentionally pass an
/// external interpreter/configuration path.
fn rebase_worktree_path(repo_root: &Path, worktree: &Path, path: &Path) -> PathBuf {
    if let Ok(relative) = path.strip_prefix(repo_root) {
        worktree.join(relative)
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    }
}

// Tiny smoke test — the integration suite under `tests/` is the real proof.
#[cfg(test)]
mod unit_tests {
    use super::*;

    // Serializes tests that mutate the process-wide PATH/CARGO_HOME env
    // vars. Rust runs tests in parallel by default, so without this lock
    // a sibling test adding `.cargo/bin` to PATH races with a test
    // trying to assert PATH is empty — the resolution result is then
    // a coin-flip. The NOTIFY_ENV_LOCK pattern in main.rs uses the same
    // idea.
    static PATH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn discovers_plain_test_fn() {
        let src = r#"
#[test]
fn classify_high() {
    assert_eq!(1, 1);
}
"#;
        let names = discover_test_fns(src);
        assert_eq!(names, vec!["classify_high".to_string()]);
    }

    #[test]
    fn discovers_tokio_test_fn() {
        let src = r#"
#[tokio::test]
async fn classify_async() {
    assert!(true);
}
"#;
        let names = discover_test_fns(src);
        assert!(names.contains(&"classify_async".to_string()));
    }

    #[test]
    fn ignores_non_test_fns() {
        let src = r#"
fn helper() { 1 }

#[test]
fn real_test() { assert!(true); }
"#;
        let names = discover_test_fns(src);
        assert_eq!(names, vec!["real_test".to_string()]);
    }

    // ---- r5: skip-reason discovery + #[ignore] classification ----

    #[test]
    fn discovers_test_fn_with_ignore_attached_records_skip_reason() {
        let src = r#"
#[test]
#[ignore = "needs fixture repo"]
fn needs_network() {
    assert!(true);
}
"#;
        let infos = discover_test_fns_with_skip(src);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "needs_network");
        assert_eq!(
            infos[0].skip_reason.as_deref(),
            Some("needs fixture repo"),
            "#[ignore = \"...\"] must populate skip_reason",
        );
    }

    #[test]
    fn discovers_test_fn_with_bare_ignore_records_default_skip_reason() {
        let src = r#"
#[test]
#[ignore]
fn slow_path() {
    assert!(true);
}
"#;
        let infos = discover_test_fns_with_skip(src);
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "slow_path");
        assert!(
            infos[0].skip_reason.is_some(),
            "bare #[ignore] must still record a skip_reason (default)",
        );
    }

    #[test]
    fn plain_test_fn_has_no_skip_reason() {
        let src = r#"
#[test]
fn ordinary() { assert!(true); }
"#;
        let infos = discover_test_fns_with_skip(src);
        assert_eq!(infos.len(), 1);
        assert!(infos[0].skip_reason.is_none());
    }

    // ---- r5: verdict derivation from raw outcome fields ----

    #[test]
    fn verdict_genuine_when_revert_red_and_head_green_and_baseline_green() {
        let outcome = RunOutcome {
            green_on_head_ok: true,
            baseline_ok: true,
            failing_on_revert: vec!["classify_high".to_string()],
        };
        assert_eq!(outcome.verdict(), Verdict::Genuine);
    }

    #[test]
    fn verdict_vacuous_when_revert_all_green_but_other_phases_pass() {
        let outcome = RunOutcome {
            green_on_head_ok: true,
            baseline_ok: true,
            failing_on_revert: vec![],
        };
        assert_eq!(outcome.verdict(), Verdict::Vacuous);
    }

    #[test]
    fn verdict_green_failed_when_head_fails() {
        let outcome = RunOutcome {
            green_on_head_ok: false,
            baseline_ok: true,
            failing_on_revert: vec!["classify_high".to_string()],
        };
        assert_eq!(outcome.verdict(), Verdict::GreenFailed);
    }

    #[test]
    fn verdict_baseline_failed_when_pristine_main_fails() {
        let outcome = RunOutcome {
            green_on_head_ok: true,
            baseline_ok: false,
            failing_on_revert: vec!["classify_high".to_string()],
        };
        assert_eq!(outcome.verdict(), Verdict::BaselineFailed);
    }

    // ---- r6: diff-aware fn-level scoping (issue #387 r6 P1 #5) ----

    #[test]
    fn diff_aware_targeting_only_emits_added_fn_when_file_is_new() {
        // base_src == None simulates a brand-new test file added by the
        // PR. Every head fn must classify as added.
        let head = r#"
#[test]
fn new_test() { assert!(true); }
"#;
        let (targeted, skipped) = compute_targeted_test_fns(None, head);
        assert_eq!(targeted, vec!["new_test".to_string()]);
        assert!(skipped.is_empty());
    }

    #[test]
    fn diff_aware_targeting_excludes_fn_with_unchanged_body() {
        // Base has both fns. Head re-declares them with the same body —
        // issue #387 r6 P1 #5: unchanged fns must NOT be re-run.
        let base = r#"
#[test]
fn a() { assert!(true); }

#[test]
fn b() { assert!(false); }
"#;
        let head = r#"
#[test]
fn a() { assert!(true); }

#[test]
fn b() { assert!(false); }
"#;
        let (targeted, _skipped) = compute_targeted_test_fns(Some(base), head);
        assert!(
            targeted.is_empty(),
            "no fn changed, expected empty targeted list; got {targeted:?}"
        );
    }

    #[test]
    fn diff_aware_targeting_emits_modified_fn_when_body_changed() {
        let base = r#"
#[test]
fn a() { assert!(true); }
"#;
        let head = r#"
#[test]
fn a() { assert!(false); }
"#;
        let (targeted, _) = compute_targeted_test_fns(Some(base), head);
        assert_eq!(targeted, vec!["a".to_string()]);
    }

    #[test]
    fn diff_aware_targeting_mixes_added_and_unchanged() {
        // Base has fn `a` only. Head adds `b` and leaves `a` alone.
        let base = r#"
#[test]
fn a() { assert!(true); }
"#;
        let head = r#"
#[test]
fn a() { assert!(true); }

#[test]
fn b() { assert!(true); }
"#;
        let (targeted, _) = compute_targeted_test_fns(Some(base), head);
        assert_eq!(targeted, vec!["b".to_string()]);
    }

    #[test]
    fn diff_aware_targeting_preserves_skip_reason_for_targeted_fns() {
        let base = r#"
#[test]
fn a() { assert!(true); }
"#;
        let head = r#"
#[test]
#[ignore = "needs fixture"]
fn b() { assert!(true); }
"#;
        let (targeted, skipped) = compute_targeted_test_fns(Some(base), head);
        assert_eq!(targeted, vec!["b".to_string()]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].name, "b");
        assert_eq!(skipped[0].skip_reason.as_deref(), Some("needs fixture"));
    }

    #[test]
    fn fn_bodies_iter_extracts_per_fn_body() {
        let src = r#"
#[test]
fn a() { assert!(true); }

#[test]
fn b() {
    let x = 1;
    assert_eq!(x, 1);
}
"#;
        let bodies: Vec<&str> = fn_bodies_iter(src)
            .into_iter()
            .map(|(_, _, _name)| _name)
            .collect();
        assert_eq!(bodies, vec!["a", "b"]);

        // Body for `b` should include "let x = 1".
        let bodies_full = fn_bodies_iter(src);
        let body_b = bodies_full.iter().find(|(_, _, n)| *n == "b").unwrap();
        let body_text = &src[body_b.0..body_b.1];
        assert!(body_text.contains("let x = 1"));
    }

    // ---- jleechan-ni1k / issue #437 bonus: nested-crate manifest
    // discovery. dark-factory is a nested-crate layout (`daemon/Cargo.toml`
    // at the crate root, NOT the repo root). The legacy walk-up
    // `find_cargo_manifest` cannot find a manifest on this layout — the
    // detector surfaced `ManifestMissing: no Cargo.toml reachable from
    // /home/jleechan/projects/dark-factory` during PR #435's assessment.
    // The fix is a bounded recursive downward search (`find_cargo_
    // manifest_recursive`) that walks up to `max_depth` levels looking
    // for the first `Cargo.toml`. These tests pin that contract: it
    // finds a top-level manifest (fast-path), finds a nested manifest
    // (the dark-factory layout), skips `target`/`node_modules`/`.git`,
    // and respects the depth bound so a malicious tree can't burn
    // wall-clock per tick.

    #[test]
    fn find_cargo_manifest_recursive_finds_root_level_manifest() {
        let dir = tempdir_unique("vacuous-root");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let found = find_cargo_manifest_recursive(&dir, 4).unwrap();
        assert!(found.ends_with("Cargo.toml"));
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(dir.join("Cargo.toml")).unwrap()
        );
    }

    #[test]
    fn find_cargo_manifest_recursive_finds_nested_manifest() {
        // Reproduce the dark-factory layout: repo_root has NO Cargo.toml,
        // but repo_root/daemon/Cargo.toml exists.
        let dir = tempdir_unique("vacuous-nested");
        std::fs::create_dir(dir.join("daemon")).unwrap();
        std::fs::write(
            dir.join("daemon").join("Cargo.toml"),
            "[package]\nname=\"daemon\"\n",
        )
        .unwrap();
        // Sanity check: the walk-up helper returns None here, proving the
        // recursive helper is doing real work.
        assert!(
            find_cargo_manifest(&dir).is_none(),
            "walk-up must not find a nested manifest; the recursive helper \
             exists exactly because the walk-up fails on nested crates"
        );
        let found = find_cargo_manifest_recursive(&dir, 4).unwrap();
        let found_canon = std::fs::canonicalize(found).unwrap();
        let expected = std::fs::canonicalize(dir.join("daemon").join("Cargo.toml")).unwrap();
        assert_eq!(found_canon, expected);
    }

    #[test]
    fn find_cargo_manifest_recursive_skips_target_node_modules_git() {
        // A tree where the only Cargo.toml lives inside `target/...`
        // must NOT be picked up — `target/` is build output, not a crate.
        let dir = tempdir_unique("vacuous-skip");
        std::fs::create_dir_all(dir.join("target").join("build")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join("target").join("Cargo.toml"),
            "[package]\nname=\"build-output\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("node_modules").join("Cargo.toml"),
            "[package]\nname=\"node-dep\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".git").join("Cargo.toml"),
            "[package]\nname=\"git-internal\"\n",
        )
        .unwrap();
        let found = find_cargo_manifest_recursive(&dir, 4);
        assert!(
            found.is_none(),
            "must not surface a manifest from target/node_modules/.git; got {found:?}"
        );
    }

    #[test]
    fn find_cargo_manifest_recursive_respects_max_depth() {
        // A manifest 5 levels deep with max_depth=3 must NOT be returned.
        let dir = tempdir_unique("vacuous-depth");
        let deep = dir.join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Cargo.toml"), "[package]\nname=\"deep\"\n").unwrap();
        assert!(
            find_cargo_manifest_recursive(&dir, 3).is_none(),
            "depth=3 must not reach a/b/c/d/e/Cargo.toml"
        );
        let found = find_cargo_manifest_recursive(&dir, 6).unwrap();
        assert!(found.ends_with("Cargo.toml"));
    }

    #[test]
    fn pytest_baseline_rebases_head_paths_into_detached_worktree() {
        // This fixture intentionally has different head imports/configuration
        // from base. Only the detached base worktree can collect and pass the
        // test; using the original absolute head paths must fail.
        let dir = tempdir_unique("pytest-baseline-rebase");
        std::fs::create_dir_all(dir.join("src/pkg")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname='baseline-rebase'\n\n[tool.pytest.ini_options]\npythonpath=['src']\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/pkg/__init__.py"), "").unwrap();
        std::fs::write(dir.join("src/pkg/value.py"), "def value():\n    return 'base'\n").unwrap();
        std::fs::write(
            dir.join("tests/test_value.py"),
            "from pkg.value import value\n\ndef test_value():\n    assert value() == 'base'\n",
        )
        .unwrap();
        let run = |args: &[&str]| {
            let out = Command::new(args[0])
                .current_dir(&dir)
                .args(&args[1..])
                .output()
                .expect("spawn fixture command");
            assert!(
                out.status.success(),
                "fixture command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["git", "init", "-q", "-b", "main"]);
        run(&["git", "config", "user.email", "pytest@example.com"]);
        run(&["git", "config", "user.name", "pytest"]);
        run(&["git", "add", "."]);
        run(&["git", "commit", "-q", "-m", "base"]);
        let base = String::from_utf8(
            Command::new("git")
                .current_dir(&dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        // Head deliberately changes both pytest configuration and the test
        // import/assertion. It is not expected to pass; the regression calls
        // only the baseline phase and must use the detached base files.
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname='baseline-rebase-head'\n\n[tool.pytest.ini_options]\npythonpath=['.']\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tests/test_value.py"),
            "from pkg.value import value\n\ndef test_value():\n    assert value() == 'head'\n",
        )
        .unwrap();
        run(&["git", "add", "."]);
        run(&["git", "commit", "-q", "-m", "head"]);

        let pytest_targets = vec![PytestTarget {
            path: dir.join("tests/test_value.py"),
            name: "test_value".to_owned(),
        }];
        let manifest = dir.join("pyproject.toml");
        let result = run_pytest_baseline_check(
            &dir,
            &base,
            &pytest_targets,
            &["test_value".to_owned()],
            Some(&manifest),
            PytestLocation::OnPath,
        )
        .expect("detached baseline pytest should run");
        assert!(
            result.all_passed(),
            "detached base test must pass after path rebasing: {result:?}"
        );
    }

    #[test]
    fn pytest_baseline_materializes_added_fn_in_existing_module() {
        // The base already contains this test module, while the PR adds a
        // second function to it.  The added function must be copied into the
        // detached baseline; otherwise pytest reports "not found" and the
        // verifier would map BaselineFailed to Green.
        let dir = tempdir_unique("pytest-baseline-added-fn");
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname='added-fn'\n\n[tool.pytest.ini_options]\npythonpath=['.']\n",
        )
        .unwrap();
        std::fs::write(dir.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(dir.join("pkg/value.py"), "def value():\n    return 'base'\n").unwrap();
        std::fs::write(
            dir.join("tests/test_value.py"),
            "import pytest\nfrom pkg.value import value\n\ndef test_existing():\n    assert value() == 'base'\n",
        )
        .unwrap();
        let run = |args: &[&str]| {
            let out = Command::new(args[0])
                .current_dir(&dir)
                .args(&args[1..])
                .output()
                .expect("spawn fixture command");
            assert!(
                out.status.success(),
                "fixture command {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["git", "init", "-q", "-b", "main"]);
        run(&["git", "config", "user.email", "pytest@example.com"]);
        run(&["git", "config", "user.name", "pytest"]);
        run(&["git", "add", "."]);
        run(&["git", "commit", "-q", "-m", "base"]);
        let base = String::from_utf8(
            Command::new("git")
                .current_dir(&dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();

        std::fs::write(dir.join("pkg/value.py"), "def value():\n    return 'head'\n").unwrap();
        std::fs::write(
            dir.join("tests/test_value.py"),
            "import pytest\nfrom pkg.value import value\n\ndef test_existing():\n    assert value() == 'base'\n\n@pytest.mark.skipif(False, reason='regression')\n@pytest.mark.regression\ndef test_added_vacuous():\n    assert 2 + 2 == 4\n",
        )
        .unwrap();
        run(&["git", "add", "."]);
        run(&["git", "commit", "-q", "-m", "head"]);

        let changed = vec![
            (dir.join("pkg/value.py"), FileClass::Production),
            (dir.join("tests/test_value.py"), FileClass::Test),
        ];
        let report = check_red_green_with_manifest(
            &dir,
            &base,
            &changed,
            Some(&dir.join("pyproject.toml")),
        )
        .expect("pytest detector should execute");
        assert_eq!(
            report.verdict,
            Verdict::Vacuous,
            "added function must run on baseline, not become BaselineFailed: {report:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pytest_targets_keep_file_association_and_fail_on_sibling_collection_error() {
        let dir = tempdir_unique("pytest-target-association");
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[project]\nname='association'\n").unwrap();
        std::fs::write(
            dir.join("tests/test_valid.py"),
            "def test_same():\n    assert True\n",
        )
        .unwrap();
        // Same test name, but invalid syntax. A name-only Cartesian matcher
        // would see the valid file's PASSED line and incorrectly accept both.
        std::fs::write(
            dir.join("tests/test_invalid.py"),
            "def test_same(:\n    assert True\n",
        )
        .unwrap();
        let targets = vec![
            PytestTarget {
                path: dir.join("tests/test_valid.py"),
                name: "test_same".to_owned(),
            },
            PytestTarget {
                path: dir.join("tests/test_invalid.py"),
                name: "test_same".to_owned(),
            },
        ];
        let outcome = run_pytest_tests(
            &dir,
            &targets,
            &["test_same".to_owned()],
            Some(&dir.join("pyproject.toml")),
            PytestLocation::OnPath,
        )
        .expect("pytest should spawn for association fixture");
        assert!(
            !outcome.all_passed(),
            "sibling collection failure must fail closed: {outcome:?}"
        );
        assert!(
            outcome
                .failing
                .iter()
                .any(|failure| failure.contains("pytest process failed")
                    || failure.contains("test_invalid.py::test_same")),
            "failure must identify process/invalid target, not be masked by same-name pass: {outcome:?}"
        );
    }

    /// Create a unique temp directory under `std::env::temp_dir()`. The
    /// directory is NOT auto-cleaned (Rust tests don't share a global
    /// fixture lifetime), but the name encodes the test pid + nanos so
    /// concurrent test runs don't collide. Lives inside `unit_tests`
    /// to avoid `clippy::items_after_test_module` (the original trailing
    /// `_silence_unused` was removed for the same reason).
    fn tempdir_unique(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("vacuous_{label}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- jleechan-sb4b: cargo binary resolution ----
    //
    // The systemd-unit daemon environment that surfaced bead
    // jleechan-sb4b had no `cargo` on PATH, so the runtime detector
    // surfaced `GreenFailed: git error: spawn cargo test: No such file or
    // directory` on every assessment — a misleading "git error" that hid
    // the real cause. The fix is twofold:
    //
    //   1. `resolve_cargo()` falls back to `$HOME/.cargo/bin/cargo` and
    //      `rustup which cargo` so the detector can run when cargo is
    //      installed via rustup but not symlinked onto PATH.
    //   2. When the resolver returns `NotFound`, the detector returns
    //      `RedGreenError::CargoNotFound` so the gate can surface a
    //      structured "toolchain missing" signal rather than a generic
    //      `GreenFailed`.
    //
    // These tests pin both behaviors. The PATH-stripped test reproduces
    // the systemd-unit failure mode (PATH that doesn't contain cargo) and
    // asserts the resolver finds a fake shim under a fake `cargo_home`.

    // Test helper: returns the host PATH stripped of any directory that
    // contains a `cargo` binary. This isolates the resolver from the
    // host's cargo (which would otherwise mask the "missing toolchain"
    // signal). The PATH_ENV_LOCK guard ensures no sibling test adds a
    // cargo directory between the snapshot and the assertion.
    fn path_without_cargo() -> std::ffi::OsString {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let filtered: Vec<std::path::PathBuf> = std::env::split_paths(&path)
            .filter(|entry| !entry.join("cargo").is_file())
            .collect();
        std::env::join_paths(&filtered).unwrap_or_default()
    }

    #[test]
    fn resolve_cargo_returns_not_found_when_path_and_cargo_home_both_empty() {
        let _guard = PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // cargo_home pointing at an empty dir + a PATH that has no
        // cargo binary anywhere. Resolver must surface NotFound without
        // requiring PATH mutation (the test pins the resolver, not the
        // environment).
        let fake_home = tempdir_unique("cargo-empty");
        // The host PATH almost certainly contains cargo, so strip cargo
        // out for the duration of the resolver call.
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", path_without_cargo());
        let res = resolve_cargo(Some(&fake_home));
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        assert_eq!(
            res,
            CargoLocation::NotFound,
            "PATH-stripped + empty cargo_home must surface NotFound"
        );
    }

    #[test]
    fn resolve_cargo_finds_cargo_in_cargo_home_bin_when_path_is_empty() {
        let _guard = PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Reproduce the systemd-unit failure mode: PATH without cargo,
        // but `~/.cargo/bin/cargo` exists. The resolver must find it.
        let fake_home = tempdir_unique("cargo-fallback");
        let bin = fake_home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("cargo");
        std::fs::write(&shim, "#!/bin/sh\necho ok\n").unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", path_without_cargo());
        let res = resolve_cargo(Some(&fake_home));
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        match res {
            CargoLocation::Found(p) => {
                let canonical = std::fs::canonicalize(&p).unwrap();
                let expected = std::fs::canonicalize(&shim).unwrap();
                assert_eq!(
                    canonical, expected,
                    "resolver must find the shim under cargo_home/bin"
                );
            }
            other => panic!("expected CargoLocation::Found, got {other:?}"),
        }
    }

    #[test]
    fn resolve_cargo_prefers_path_over_cargo_home() {
        let _guard = PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // When cargo is on PATH AND the cargo_home has a shim, the
        // resolver must prefer PATH (no network/disk dependency for the
        // common case). We strip the cargo from PATH first, then add a
        // private dir containing a fake cargo, and assert OnPath is the
        // chosen carrier.
        let dir = tempdir_unique("cargo-prefer");
        let bin = dir.join("path_bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("cargo"), "#!/bin/sh\nexit 0\n").unwrap();
        // cargo_home also has a shim but it must NOT be chosen.
        let fake_home = dir.join("cargo_home");
        std::fs::create_dir_all(fake_home.join("bin")).unwrap();
        std::fs::write(
            fake_home.join("bin").join("cargo"),
            "#!/bin/sh\nexit 99\n",
        )
        .unwrap();
        let stripped = path_without_cargo();
        let prev_path = std::env::var_os("PATH");
        let new_path = std::env::join_paths(
            std::iter::once(bin.clone()).chain(std::env::split_paths(&stripped)),
        )
        .unwrap();
        std::env::set_var("PATH", &new_path);
        let res = resolve_cargo(Some(&fake_home));
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        assert_eq!(
            res,
            CargoLocation::OnPath,
            "PATH hit must win over cargo_home fallback"
        );
    }

    #[test]
    fn resolve_cargo_surfaces_not_found_when_path_is_empty() {
        let _guard = PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Reproduce the systemd-unit failure mode: PATH-stripped (no
        // cargo directories), cargo_home doesn't exist. The resolver
        // must surface NotFound (or Found via rustup which cargo, when
        // rustup is on the stripped PATH — extremely unlikely).
        let fake_home = tempdir_unique("cargo-none");
        let stripped = path_without_cargo();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &stripped);
        let res = resolve_cargo(Some(&fake_home));
        match prev_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        // When neither PATH nor cargo_home yields cargo, the resolver
        // must surface NotFound (rustup is on the stripped PATH, so it
        // can't help either).
        assert_eq!(
            res,
            CargoLocation::NotFound,
            "PATH-stripped + valid-but-empty cargo_home must surface NotFound"
        );
    }

    #[test]
    fn red_green_error_cargo_not_found_display_is_actionable() {
        // The error message must NOT be the misleading "git error: spawn
        // cargo test: No such file or directory" string that surfaced in
        // the bead — it must name the toolchain and the fix.
        let e = RedGreenError::CargoNotFound("test reason".to_string());
        let msg = format!("{e}");
        assert!(msg.contains("cargo"), "error must name cargo: {msg}");
        assert!(
            msg.contains("test reason"),
            "error must include the embedded reason: {msg}"
        );
    }

    // ---- jleechan-6xje: pytest backend parity tests ----
    //
    // The cargo backend carries the r5 contract. These tests pin the
    // pytest backend's mirror: a manifest-discovery helper that mirrors
    // `find_cargo_manifest_recursive`, a Python test-fn scanner that
    // mirrors `discover_test_fns_with_skip`, a backend chooser that
    // mirrors `find_cargo_manifest`'s role, and a resolver that mirrors
    // `resolve_cargo`. Each test is the smallest unit of new behavior
    // the detector needs to gain Python coverage.

    #[test]
    fn find_pytest_manifest_recursive_finds_root_level_pyproject_toml() {
        let dir = tempdir_unique("py-root");
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname=\"x\"\n",
        )
        .unwrap();
        let found = find_pytest_manifest_recursive(&dir, 4).unwrap();
        assert!(found.ends_with("pyproject.toml"));
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(dir.join("pyproject.toml")).unwrap()
        );
    }

    #[test]
    fn find_pytest_manifest_recursive_finds_nested_pyproject_toml() {
        // Reproduce the worldarchitect.ai layout: repo root has no
        // pyproject.toml, but a nested package carries one. The
        // walk-up helper cannot find a Python manifest; the recursive
        // helper must.
        let dir = tempdir_unique("py-nested");
        std::fs::create_dir(dir.join("mvp_site")).unwrap();
        std::fs::write(
            dir.join("mvp_site").join("pyproject.toml"),
            "[project]\nname=\"mvp_site\"\n",
        )
        .unwrap();
        assert!(
            find_pytest_manifest_recursive(&dir, 4).is_some(),
            "nested pyproject.toml must be reachable by the recursive helper"
        );
        let found = find_pytest_manifest_recursive(&dir, 4).unwrap();
        assert!(found.ends_with("pyproject.toml"));
    }

    #[test]
    fn find_pytest_manifest_recursive_accepts_pytest_ini_as_fallback() {
        // A repo without pyproject.toml but with pytest.ini is still a
        // pytest project. The recursive helper must surface pytest.ini
        // when pyproject.toml is absent.
        let dir = tempdir_unique("py-ini");
        std::fs::write(
            dir.join("pytest.ini"),
            "[pytest]\ntestpaths = tests\n",
        )
        .unwrap();
        let found = find_pytest_manifest_recursive(&dir, 4).unwrap();
        assert!(found.ends_with("pytest.ini"));
    }

    #[test]
    fn find_pytest_manifest_recursive_skips_venv_node_modules_git() {
        // Subtrees that almost never carry the manifest must be excluded
        // to avoid the "found a huge dependency lockfile" false positive.
        let dir = tempdir_unique("py-skip");
        std::fs::create_dir_all(dir.join(".venv").join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".venv").join("pyproject.toml"),
            "[project]\nname=\"venv\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("node_modules").join("pyproject.toml"),
            "[project]\nname=\"npm\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".git").join("pyproject.toml"),
            "[project]\nname=\"git-internal\"\n",
        )
        .unwrap();
        let found = find_pytest_manifest_recursive(&dir, 4);
        assert!(
            found.is_none(),
            "must not surface a manifest from .venv/node_modules/.git; got {found:?}"
        );
    }

    #[test]
    fn find_pytest_manifest_recursive_respects_max_depth() {
        let dir = tempdir_unique("py-depth");
        let deep = dir.join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("pyproject.toml"), "[project]\nname=\"deep\"\n").unwrap();
        assert!(
            find_pytest_manifest_recursive(&dir, 3).is_none(),
            "depth=3 must not reach a/b/c/d/e/pyproject.toml"
        );
        let found = find_pytest_manifest_recursive(&dir, 6).unwrap();
        assert!(found.ends_with("pyproject.toml"));
    }

    #[test]
    fn discover_python_test_fns_extracts_sync_and_async_defs() {
        let src = r#"
def test_classify_high():
    assert classify_score(95) == "high"


async def test_async_thing():
    assert True


def helper():
    return 1


def test_classify_low():
    assert classify_score(10) == "low"
"#;
        let names = discover_python_test_fns(src);
        assert_eq!(
            names,
            vec![
                "test_classify_high".to_string(),
                "test_async_thing".to_string(),
                "test_classify_low".to_string(),
            ],
            "sync and async `test_*` defs at module scope must be discovered"
        );
    }

    #[test]
    fn discover_python_test_fns_ignores_class_methods_and_private_defs() {
        // pytest's default discovery rule is `test_*` at module scope.
        // Methods inside a class are still discovered by pytest, but
        // this fast scanner operates at file level; the r6 contract
        // says "added/modified at the file level" for the Rust backend,
        // and the Python backend mirrors that with the same broadness.
        // What we MUST exclude: private helpers (`_test_*`), non-test
        // defs, and defs that aren't named `test_*`.
        let src = r#"
def _test_helper():
    return 1


def regular_function():
    return 1


def test_real():
    assert True
"#;
        let names = discover_python_test_fns(src);
        assert_eq!(names, vec!["test_real".to_string()]);
    }

    #[test]
    fn compute_targeted_python_test_fns_emits_added_when_file_new() {
        let head = r#"
def test_new():
    assert True
"#;
        let (targeted, skipped) = compute_targeted_python_test_fns(None, head);
        assert_eq!(targeted, vec!["test_new".to_string()]);
        assert!(skipped.is_empty());
    }

    #[test]
    fn compute_targeted_python_test_fns_emits_modified_when_body_changed() {
        let base = r#"
def test_a():
    assert True
"#;
        let head = r#"
def test_a():
    assert False
"#;
        let (targeted, _) = compute_targeted_python_test_fns(Some(base), head);
        assert_eq!(targeted, vec!["test_a".to_string()]);
    }

    #[test]
    fn compute_targeted_python_test_fns_excludes_unchanged_fns() {
        let base = r#"
def test_a():
    assert True

def test_b():
    assert False
"#;
        let head = r#"
def test_a():
    assert True

def test_b():
    assert False
"#;
        let (targeted, _) = compute_targeted_python_test_fns(Some(base), head);
        assert!(
            targeted.is_empty(),
            "no fn changed, expected empty targeted list; got {targeted:?}"
        );
    }

    #[test]
    fn compute_targeted_python_test_fns_does_not_attach_new_decorators_to_predecessor() {
        let base = "def test_existing():\n    assert True\n";
        let head = "def test_existing():\n    assert True\n\n@pytest.mark.parametrize('value', [1])\n@pytest.mark.skipif(False, reason='regression')\nasync def test_added(value):\n    assert value == 1\n";
        let (targeted, _) = compute_targeted_python_test_fns(Some(base), head);
        assert_eq!(
            targeted,
            vec!["test_added".to_string()],
            "decorators on a new async test must not retarget its unchanged predecessor"
        );
    }

    #[test]
    fn pytest_output_status_accepts_params_and_all_terminal_states_without_prefix_collisions() {
        let stdout = "tests/test_mod.py::test_x[case] PASSED\n"
            .to_owned()
            + "tests/test_mod.py::test_failed[param] FAILED\n"
            + "tests/test_mod.py::test_skipped SKIPPED\n"
            + "tests/test_mod.py::test_error[repr] ERROR\n"
            + "tests/test_mod.py::test_xyz PASSED\n";
        assert!(pytest_output_has_status(&stdout, "tests/test_mod.py::test_x", "PASSED"));
        assert!(pytest_output_has_status(&stdout, "tests/test_mod.py::test_failed", "FAILED"));
        assert!(pytest_output_has_status(&stdout, "tests/test_mod.py::test_skipped", "SKIPPED"));
        assert!(pytest_output_has_status(&stdout, "tests/test_mod.py::test_error", "ERROR"));
        assert!(!pytest_output_has_status(&stdout, "tests/test_mod.py::test", "PASSED"));
    }

    #[test]
    fn backend_detect_picks_cargo_when_both_manifests_present() {
        let dir = tempdir_unique("backend-both");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        // Pick the backend from the directory layout. Both manifest
        // kinds co-exist in mixed-stack repos; the cargo backend wins
        // because the existing detector was cargo-only and operators
        // likely invoked it specifically for the Rust side.
        assert_eq!(Backend::detect(&dir), Some(Backend::Cargo));
    }

    #[test]
    fn backend_detect_picks_pytest_when_only_pyproject_present() {
        let dir = tempdir_unique("backend-pyonly");
        std::fs::write(dir.join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        assert_eq!(Backend::detect(&dir), Some(Backend::Pytest));
    }

    #[test]
    fn backend_detect_picks_cargo_when_only_cargo_manifest_present() {
        let dir = tempdir_unique("backend-cargoonly");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(Backend::detect(&dir), Some(Backend::Cargo));
    }

    #[test]
    fn backend_detect_returns_none_when_no_manifest_found() {
        let dir = tempdir_unique("backend-none");
        assert_eq!(Backend::detect(&dir), None);
    }

    #[test]
    fn red_green_error_pytest_not_found_display_is_actionable() {
        // The pytest analogue of the cargo display test: the error must
        // name pytest (not "git") so operators immediately see the
        // toolchain is missing.
        let e = RedGreenError::PytestNotFound("test reason".to_string());
        let msg = format!("{e}");
        assert!(msg.contains("pytest"), "error must name pytest: {msg}");
        assert!(
            msg.contains("test reason"),
            "error must include the embedded reason: {msg}"
        );
        assert!(
            !msg.contains("git"),
            "PytestNotFound must NOT mention git: {msg}"
        );
    }

    #[test]
    fn pytest_location_resolves_to_found_when_pytest_on_path() {
        // The bare-on-PATH path: the helper returns `OnPath` so the
        // detector can use `pytest` directly without an absolute path.
        if std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .find(|entry| entry.join("pytest").is_file())
            })
            .is_none()
        {
            eprintln!("pytest not on PATH; skipping pytest resolver positive test");
            return;
        }
        let loc = resolve_pytest(None);
        assert_eq!(loc, PytestLocation::OnPath);
    }

    #[cfg(unix)]
    #[test]
    fn pytest_location_rejects_non_executable_path_entry() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir_unique("pytest-non-executable");
        let candidate = dir.join("pytest");
        std::fs::write(&candidate, "#!/bin/sh\nexit 0\n").unwrap();
        let mut mode = std::fs::metadata(&candidate).unwrap().permissions();
        mode.set_mode(0o644);
        std::fs::set_permissions(&candidate, mode).unwrap();
        assert!(!is_executable_file(&candidate));
        let mut executable_mode = std::fs::metadata(&candidate).unwrap().permissions();
        executable_mode.set_mode(0o755);
        std::fs::set_permissions(&candidate, executable_mode).unwrap();
        assert!(is_executable_file(&candidate));
    }
}

// (No trailing helper — the original `_silence_unused` was removed: it
// triggered `clippy::items_after_test_module` and served no purpose.)
