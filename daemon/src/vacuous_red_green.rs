// Runtime red-green vacuous-test detector — issue #387 / bead jleechan-ijod.
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

/// Test runner backend the red-green detector dispatches to.
///
/// Bead jleechan-6xje: the r5 detector was hardcoded to cargo and the gate
/// was structurally inert on Python repos (93 of the 124 GATE_ASSESSMENT
/// `vacuous_red_green: unknown` events in the factory telemetry were
/// worldarchitect.ai Python PRs). The dispatch abstraction lets the
/// detector choose between `Cargo` (default; preserves all r5 behavior) and
/// `Pytest` (new; covers the Python backlog). New runners (e.g. `BunTest`)
/// slot in by adding a variant + matching arm in `detect_runner_kind`,
/// `discover_*_test_fns`, and `run_*_tests`.
///
/// The enum is `Copy` so it can sit on the stack in the hot path
/// (`check_red_green_with_kind`) without per-call allocation; serde
/// rename_all keeps the JSON wire format stable (`"cargo"`, `"pytest"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestRunnerKind {
    /// Rust: cargo test with `--manifest-path` so the runner executes the
    /// PR's crate regardless of cwd. Existing r5 contract.
    Cargo,
    /// Python: pytest invoked with `--collect-only -q` for discovery and
    /// the plain stdout `PASSED/FAILED` lines for verdict parsing. Used
    /// for the Python repos the factory reviews (worldarchitect.ai et al).
    Pytest,
}

impl TestRunnerKind {
    /// Canonical snake_case label for telemetry / structured logging. The
    /// daemon already reports gate outcomes by string key; this keeps
    /// `runner=cargo` / `runner=pytest` consistent across surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            TestRunnerKind::Cargo => "cargo",
            TestRunnerKind::Pytest => "pytest",
        }
    }
}

/// Detect the test runner a repo should use. The factory's primary target
/// is the dark-factory Rust workspace, so when BOTH a `Cargo.toml` and a
/// Python config are present the detector prefers `Cargo`. The detection
/// order is:
///
///   1. `Cargo.toml` reachable from `repo_root` (walk-up + recursive
///      fallback, max-depth 4 — issue #437 bonus).
///   2. Python config: `pyproject.toml` with a `[tool.pytest.ini_options]`
///      table, `pytest.ini`, `setup.cfg` with `[tool:pytest]`, or
///      `tox.ini`. Any one is sufficient — pytest discovers from any of
///      these at the repo root by default.
///   3. Neither → `Cargo` (the legacy default; preserves r5 behavior for
///      repos where neither lookup succeeded but the caller still wants a
///      best-effort dispatch).
///
/// This function is the single source of truth for backend selection —
/// `check_red_green_with_kind` (production caller) consults it, and the
/// integration tests pin each branch so the dispatch cannot silently flip.
pub fn detect_runner_kind(repo_root: &Path) -> TestRunnerKind {
    if find_cargo_manifest(repo_root).is_some()
        || find_cargo_manifest_recursive(repo_root, 4).is_some()
    {
        return TestRunnerKind::Cargo;
    }
    if find_python_pytest_config(repo_root).is_some() {
        return TestRunnerKind::Pytest;
    }
    // Default to Cargo — preserves the r5 contract for repos that have
    // neither config but still want the detector to attempt a run.
    TestRunnerKind::Cargo
}

/// Find a Python pytest config reachable from `repo_root`. Accepts the
/// canonical config filenames pytest checks at the project root:
///
///   * `pyproject.toml` with a `[tool.pytest.ini_options]` table
///   * `pytest.ini`
///   * `setup.cfg` with a `[tool:pytest]` section
///   * `tox.ini` with a `[pytest]` section
///
/// Walk-up only — pytest does not search nested directories for its
/// config, so a recursive search here would yield false positives.
/// Returns the FIRST config file found, matching the order above.
pub fn find_python_pytest_config(repo_root: &Path) -> Option<PathBuf> {
    for name in [
        "pyproject.toml",
        "pytest.ini",
        "setup.cfg",
        "tox.ini",
    ] {
        let candidate = repo_root.join(name);
        if candidate.is_file() {
            // For `pyproject.toml` only, confirm it actually has a
            // `[tool.pytest.ini_options]` table — some repos ship a
            // pyproject.toml without pytest configured (e.g. ruff-only).
            if name == "pyproject.toml" {
                if let Ok(src) = std::fs::read_to_string(&candidate) {
                    if !src.contains("[tool.pytest.ini_options]") {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
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
/// **Backward-compat shim**: equivalent to
/// `check_red_green_with_kind(repo_root, base_ref, changed,
/// TestRunnerKind::Cargo)`. New callers should prefer
/// `check_red_green_with_kind` so the detector can dispatch to pytest
/// when the target repo is Python.
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
    check_red_green_with_kind(repo_root, base_ref, changed, TestRunnerKind::Cargo, manifest_path)
}

/// Run the red-green check with an explicit backend. This is the dispatch
/// entry point for the bead jleechan-6xje change: callers that have
/// already decided which runner to use (e.g. after `detect_runner_kind`)
/// pass it explicitly; the cargo-only `check_red_green_with_manifest`
/// shim remains for callers that pre-date the abstraction.
///
/// The three-phase contract (head/baseline/revert) is identical to the
/// cargo-only path; only the test discovery + test runner dispatch on
/// `kind`. The `manifest_path` argument is honored for `Cargo` (used as
/// `--manifest-path`) and ignored for `Pytest` (pytest discovers from
/// the project root's `pyproject.toml` / `pytest.ini` automatically; the
/// field is preserved in the report for telemetry parity).
pub fn check_red_green_with_kind(
    repo_root: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
    kind: TestRunnerKind,
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

    // Resolve the runner-specific config path. For Cargo this is the
    // walk-up + recursive Cargo.toml lookup; for Pytest this is the
    // walk-up search for pyproject.toml / pytest.ini / setup.cfg / tox.ini.
    // The result is preserved on `RedGreenReport.manifest_path` for
    // telemetry parity so operators can see which config was used.
    let resolved_manifest: Option<PathBuf> = match kind {
        TestRunnerKind::Cargo => match manifest_path {
            Some(p) => Some(p.to_path_buf()),
            None => find_cargo_manifest(repo_root)
                .or_else(|| find_cargo_manifest_recursive(repo_root, 4)),
        },
        TestRunnerKind::Pytest => match manifest_path {
            Some(p) => Some(p.to_path_buf()),
            None => find_python_pytest_config(repo_root),
        },
    };

    // Bead jleechan-sb4b: resolve the cargo binary via the same fallback
    // chain the startup check uses. Only consulted when `kind == Cargo`;
    // pytest resolves its own interpreter via `which` at run time.
    let cargo_loc = resolve_cargo(cargo_home_from_env().as_deref());

    // Step 1: discover the diff-aware added/modified test fns + their
    // per-fn skip reasons across the changed test files. r6 contract
    // (issue #387 r6 P1 #5): scope at the fn level AND only emit fns that
    // were actually added or modified by this PR — not every `#[test]`
    // living in a changed test file. The base blob for each path is
    // fetched via `git show <base_ref>:<rel>` so we can compare fn bodies
    // (added fns = name missing from base; modified fns = same name in
    // both, different body). `#[ignore]` / `#[ignore = "..."]` populate
    // `skip_reason` (issue #387 r5 finding 4) on the cargo side; pytest
    // has no static skip marker, so its `skip_reason` is always None.
    let mut targeted: BTreeSet<String> = BTreeSet::new();
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
        let (added_or_modified, skipped_local) = match kind {
            TestRunnerKind::Cargo => {
                compute_targeted_test_fns(base_src.as_deref(), &head_src)
            }
            TestRunnerKind::Pytest => compute_targeted_test_fns_pytest(base_src.as_deref(), &head_src),
        };
        for info in skipped_local {
            skipped.push(info);
        }
        for name in added_or_modified {
            targeted.insert(name);
        }
    }
    let targeted_tests: Vec<String> = targeted.iter().cloned().collect();

    // Phase (a) — green-on-PR-head. If the targeted tests don't pass
    // before any revert, the gate reports `GreenFailed` immediately.
    let head_pass = match kind {
        TestRunnerKind::Cargo => run_cargo_tests(
            repo_root,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        )?,
        TestRunnerKind::Pytest => run_pytest_tests(
            repo_root,
            &test_files,
            &targeted_tests,
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
    let baseline_pass = match kind {
        TestRunnerKind::Cargo => run_baseline_check(
            repo_root,
            base_ref,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        )?,
        TestRunnerKind::Pytest => run_pytest_baseline_check(
            repo_root,
            base_ref,
            &test_files,
            &targeted_tests,
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

    let revert_outcome = match kind {
        TestRunnerKind::Cargo => run_cargo_tests(
            repo_root,
            &test_files,
            &targeted_tests,
            resolved_manifest.as_deref(),
            cargo_loc.clone(),
        ),
        TestRunnerKind::Pytest => run_pytest_tests(
            repo_root,
            &test_files,
            &targeted_tests,
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

/// Diff-aware scoping for pytest: given the parsed HEAD-side `test_*`
/// fns and the optional base-side source for the same file, return
/// `(targeted_names, skipped_records)` where `targeted_names` contains
/// exactly those head-side fns that were added OR modified by this PR.
///
/// Mirrors `compute_targeted_test_fns` for Rust. The fn body extraction
/// is line-based (Python's indentation is significant — a function body
/// is everything from the line AFTER `def <name>(` until the next
/// top-level `def` / `class` / end-of-file), which is sufficient for
/// r6's "added or modified" contract: a body that changed at any point
/// inside the function flags the whole fn as modified.
///
/// `skipped_records` is always empty for pytest (Python has no static
/// skip marker comparable to cargo's `#[ignore]`); runtime skip via
/// `pytest.mark.skip` / `pytest.skip()` is observed at execution time
/// by the verdict parser's `PASSED` / `FAILED` parsing.
fn compute_targeted_test_fns_pytest(
    base_src: Option<&str>,
    head_src: &str,
) -> (Vec<String>, Vec<TestFnInfo>) {
    let head_fns = discover_pytest_test_fns(head_src);
    if head_fns.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Build a base-side name -> body map by line-scanning for `def ` at
    // column 0. Python convention: top-level `def`s start at column 0;
    // methods indented inside `class` are NOT collected by pytest by
    // default (`python_functions = ["test_*"]` matches method names but
    // their nodeids are `TestClass::test_xxx`, which the verdict parser
    // strips down via `split("::").last()`).
    let base_bodies: std::collections::HashMap<String, String> = match base_src {
        None => std::collections::HashMap::new(),
        Some(src) => {
            let mut map = std::collections::HashMap::new();
            for (name, body) in py_def_bodies(src) {
                map.insert(name, body);
            }
            map
        }
    };
    let head_bodies: std::collections::HashMap<String, String> = py_def_bodies(head_src)
        .into_iter()
        .collect();

    let mut targeted: Vec<String> = Vec::new();
    for info in head_fns {
        let head_body = head_bodies.get(&info.name);
        let base_body = base_bodies.get(&info.name);
        match (head_body, base_body) {
            // Brand-new file: every head fn is added.
            (Some(_), None) if base_src.is_none() => targeted.push(info.name),
            // Added: name not in base.
            (Some(_), None) => targeted.push(info.name),
            // Modified: same name in both, body differs.
            (Some(h), Some(b)) if h != b => targeted.push(info.name),
            // Unchanged: same body — exclude from targeting (r6 P1 #5).
            (Some(_), Some(_)) => {}
            // Defensive: head fn has no body. Should not happen if the
            // parser is consistent, but treat as added to be safe.
            (None, _) => targeted.push(info.name),
        }
    }
    (targeted, Vec::new())
}

/// Iterate `(name, body)` for every top-level `def <name>(` in `source`,
/// where the body runs from the line AFTER the `def` line until the next
/// top-level `def` / `class` / end-of-file (Python uses indentation for
/// scope, so this line-based extraction is a heuristic — sufficient for
/// "added vs modified" detection in the runtime detector because the
/// diff at the function-body level is byte-comparable).
fn py_def_bodies(source: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Top-level defs start at column 0.
        let after_def = match line.strip_prefix("def ") {
            Some(rest) => rest,
            None => {
                i += 1;
                continue;
            }
        };
        // Name: identifier up to `(`. Allow trailing whitespace.
        let name_end = after_def
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .last()
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        if name_end == 0 {
            i += 1;
            continue;
        }
        let name = after_def[..name_end].to_string();
        // Body: every subsequent line until the next top-level `def ` /
        // `class ` / EOF. Indented continuations belong to this body.
        let mut body_lines: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let next = lines[j];
            let next_starts_top = next.starts_with("def ") || next.starts_with("class ");
            // A top-level non-blank, non-comment line terminates the body.
            if next_starts_top && !next.is_empty() {
                break;
            }
            body_lines.push(next);
            j += 1;
        }
        let body = body_lines.join("\n");
        out.push((name, body));
        i = j;
    }
    out
}

/// Run pytest against the working tree (or a worktree under `baseline_root`
/// for the baseline-main phase) using the resolved `pytest` interpreter.
///
/// Bead jleechan-6xje: pytest output is parsed from the `PASSED` /
/// `FAILED` / `ERROR` lines emitted in pytest's default text mode
/// (`-q --no-header`). For each `targeted_test`, we look for either a
/// `... PASSED` or `... FAILED` line — anything else (no PASSED line for
/// a targeted test) is treated as a hard fail signal so the gate doesn't
/// accidentally approve a test that was skipped, errored at collection,
/// or never reached. This mirrors the cargo `NEVER_RAN` rule the r5
/// contract defined.
///
/// We invoke pytest with `-q --no-header` so the output is one line per
/// test (`tests/test_scenario.py::test_alpha PASSED [100%]`). We then
/// `--deselect` every test that is NOT in the targeted list so only the
/// PR's added/modified tests run — matches the r6 P1 #5 contract.
///
/// Failure modes:
///   * `pytest` not on PATH -> `RedGreenError::CargoNotFound` (re-uses the
///     cargo error variant so the tick-side translate_error path stays
///     untouched; the message names pytest explicitly).
fn run_pytest_tests(
    repo_root: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
) -> Result<CargoOutcome, RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok(CargoOutcome {
            failing: vec![],
            compile_errored: false,
        });
    }

    let pytest_bin = which_pytest().ok_or_else(|| {
        RedGreenError::CargoNotFound(
            "pytest was not found on PATH or via `python3 -m pytest`; \
             install pytest (pip install pytest) so the runtime detector \
             can run against Python repos."
                .to_string(),
        )
    })?;

    let mut failing: Vec<String> = Vec::new();
    let mut compile_errored = false;

    // Build nodeids like `tests/test_scenario.py::test_alpha` and use
    // pytest's `-k` selector (substring match) to limit execution to the
    // PR's added/modified tests. Pytest's `-k` is a NAME selector and
    // matches any node whose name CONTAINS the substring; we then assert
    // exact-match per-test in the verdict parser to avoid a test like
    // `test_alpha_extra` accidentally satisfying `-k test_alpha`.
    let mut args: Vec<String> = vec![
        "-v".to_string(),
        "--no-header".to_string(),
        "-p".to_string(),
        "no:cacheprovider".to_string(),
    ];
    for tf in test_files {
        let path_str = tf.to_string_lossy().to_string();
        // Make pytest pick up tests under this file. Pytest resolves
        // nodeids from a path relative to the repo root.
        let rel = tf
            .strip_prefix(repo_root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path_str);
        args.push(rel);
    }
    // `-k` to keep only the targeted tests. Pytest's `-k` matches
    // nodeids by substring; we OR-combine the targeted names. For one
    // test this is just the name.
    let k_expr = targeted_tests.join(" or ");
    if !k_expr.is_empty() {
        args.push("-k".to_string());
        args.push(k_expr);
    }

    let out = Command::new(&pytest_bin)
        .current_dir(repo_root)
        .args(&args)
        .output()
        .map_err(|e| {
            RedGreenError::CargoNotFound(format!(
                "spawn {}: {e}; pytest binary not usable from this environment",
                pytest_bin.display()
            ))
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Collection errors (ImportError on the test module, syntax errors,
    // etc.) manifest as `ERROR collecting` on stderr — treat that as
    // compile_errored so the gate stops. Same contract as cargo's
    // `error[E...]` rule.
    if !out.status.success() && stderr.contains("ERROR collecting") {
        compile_errored = true;
    }

    // Parse pytest's per-test summary lines. With `-v` pytest emits
    // `<path>::<test_name> PASSED [100%]` per test. With `-q` it just
    // shows dots and a final summary, which is NOT enough granularity
    // to map a verdict back to a specific test name — so we use `-v`
    // here even though it's noisier. The format is:
    //   `tests/test_scenario.py::test_alpha PASSED [100%]`
    //   `tests/test_scenario.py::test_alpha FAILED`
    //   `tests/test_scenario.py::test_alpha SKIPPED`
    //   `tests/test_scenario.py::test_alpha ERROR`
    for name in targeted_tests {
        // Pytest's per-test line begins with `path::name`, so the
        // substring we look for is `::name` (the canonical pytest
        // nodeid separator). Tests run inside classes get a longer
        // nodeid (`tests/test_X.py::TestClass::test_y`) — for those we
        // strip down to the function name in the parser below.
        let passed_marker = format!("::{name} PASSED");
        let failed_marker = format!("::{name} FAILED");
        let skipped_marker = format!("::{name} SKIPPED");
        let errored_marker = format!("::{name} ERROR");
        if stdout.contains(&failed_marker) || stderr.contains(&failed_marker) {
            failing.push(name.clone());
        } else if stdout.contains(&passed_marker) {
            // Test ran + passed: nothing to record.
        } else if stdout.contains(&skipped_marker) {
            // Mirrors cargo's `#[ignore]` rule: a SKIPPED test never ran,
            // so we can't tell vacuous-vs-genuine from it. Record it as
            // a failing entry with the NEVER_RAN suffix so the gate
            // surfaces a structured "skipped test, no signal" verdict.
            failing.push(format!("{name}:NEVER_RAN"));
        } else if stderr.contains(&errored_marker) || stdout.contains(&errored_marker) {
            // ERROR indicates collection-time / fixture / unexpected
            // error; treat as a hard fail signal so the gate doesn't
            // approve an erroring test as "genuine".
            failing.push(name.clone());
        } else {
            // No PASSED / FAILED / SKIPPED / ERROR line for this test —
            // it never ran. Hard fail signal (matches r5 cargo rule).
            failing.push(format!("{name}:NEVER_RAN"));
        }
    }

    Ok(CargoOutcome {
        failing,
        compile_errored,
    })
}

/// Locate a usable `pytest` invocation. Order of preference:
///   1. bare `pytest` on PATH (most common case)
///   2. `python3 -m pytest` (when pytest is installed but not on PATH —
///      the systemd-unit failure mode for Python workers)
///
/// Returns `Some(PathBuf)` for a bare-name `pytest` hit, `None` when
/// nothing was found. The caller surfaces `RedGreenError::CargoNotFound`
/// (re-used error variant; the message names pytest) so the tick-side
/// translate_error path stays untouched.
fn which_pytest() -> Option<PathBuf> {
    // 1. bare `pytest` on PATH.
    if let Some(path_var) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&path_var) {
            let candidate = entry.join("pytest");
            if candidate.is_file() {
                return Some(PathBuf::from("pytest"));
            }
        }
    }
    // 2. `python3 -m pytest` — the canonical fallback when the bare
    // shim isn't on PATH but the interpreter is. We can't directly
    // invoke `-m pytest` through `which`; the caller (run_pytest_tests)
    // uses the returned path verbatim, so we fall back to returning
    // `python3` if it exists — run_pytest_tests knows to invoke it via
    // `-m pytest` when the path is the python interpreter.
    if let Some(path_var) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&path_var) {
            let candidate = entry.join("python3");
            if candidate.is_file() {
                return Some(PathBuf::from("python3"));
            }
        }
    }
    None
}

/// Returns the last line in `haystack` containing `needle`. Used by the
/// pytest verdict parser so per-test result lines win over collection
/// errors when both mention the test name.
fn last_line_containing(haystack: &str, needle: &str) -> String {
    haystack
        .lines()
        .rev()
        .find(|l| l.contains(needle))
        .unwrap_or("")
        .to_string()
}

/// Run phase (c) — baseline-main sanity check — for pytest by
/// materializing `base_ref` in a temporary git worktree, running pytest
/// there, and cleaning up. Mirrors `run_baseline_check` for cargo.
///
/// Bead jleechan-6xje: unlike the cargo baseline (which has a separate
/// manifest-path resolution bug that silently runs tests against HEAD's
/// crate instead of the pristine base — see `run_baseline_check`), the
/// pytest path operates on the WORKTREE'S TREE directly. Tests that
/// were ADDED by the PR cannot exist on the baseline worktree, so
/// `pytest -k <new_test_name>` will report them as NEVER_RAN — and the
/// detector correctly surfaces BaselineFailed.
///
/// The contract:
///   - Test files present on baseline work the same as cargo: pytest
///     runs them against the baseline's src tree and passes if they're
///     sound.
///   - Tests added by the PR (`base_src == None` for the test file, or
///     the test fn is missing from `base_src`) cannot run on baseline
///     by definition — they reference symbols that the PR introduces.
///     The detector filters these out of the baseline-targeted list so
///     the baseline check is only meaningful for tests that COULD have
///     run on baseline.
fn run_pytest_baseline_check(
    repo_root: &Path,
    base_ref: &str,
    test_files: &[PathBuf],
    targeted_tests: &[String],
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

    // Compute the baseline-runnable subset of targeted tests: a test is
    // baseline-runnable iff (a) the test file exists on baseline AND (b)
    // the test fn is declared in the baseline test file. This filters
    // out PR-added tests that cannot exist on baseline by definition.
    let baseline_runnable = baseline_runnable_tests(repo_root, base_ref, test_files, targeted_tests);

    // Re-base the test file paths from the original repo root to the
    // worktree root. The detector passes paths under repo_root; the
    // baseline worktree has the same tree layout, so stripping the
    // repo_root prefix and re-rooting under tmp is correct.
    let rebased: Vec<PathBuf> = test_files
        .iter()
        .map(|p| {
            p.strip_prefix(repo_root)
                .ok()
                .map(|rel| tmp.join(rel))
                .unwrap_or_else(|| p.clone())
        })
        .collect();

    let result = if baseline_runnable.is_empty() {
        // Nothing to check on baseline — every targeted test was added by
        // the PR. The baseline-main sanity check is vacuously satisfied.
        Ok(CargoOutcome {
            failing: vec![],
            compile_errored: false,
        })
    } else {
        run_pytest_tests(&tmp, &rebased, &baseline_runnable)
    };

    // Cleanup is best-effort; stale worktrees are a leak, not a defect.
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

/// Filter the targeted test names to those that could plausibly run on
/// the baseline (`base_ref`) worktree. A test is baseline-runnable iff
/// (a) its test file exists on baseline AND (b) the test fn is declared
/// in the baseline test file. Tests added by the PR (no base_src, or fn
/// not in base_src) are excluded — they reference symbols the PR
/// introduces, so they cannot exist on baseline by definition.
fn baseline_runnable_tests(
    repo_root: &Path,
    base_ref: &str,
    test_files: &[PathBuf],
    targeted_tests: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for path in test_files {
        let rel = match relative_repo_path(repo_root, path) {
            Some(r) => r,
            None => continue,
        };
        let base_src = match read_base_blob(repo_root, base_ref, &rel) {
            Some(s) => s,
            None => continue, // file didn't exist on baseline
        };
        let base_fns = discover_pytest_test_fns(&base_src);
        let base_names: std::collections::HashSet<&str> =
            base_fns.iter().map(|i| i.name.as_str()).collect();
        for name in targeted_tests {
            if base_names.contains(name.as_str()) && !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    out
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

/// Scan a Python test file for top-level `test_*` function declarations.
///
/// Bead jleechan-6xje: this is the Python counterpart to
/// `discover_test_fns_with_skip`. Pytest's default `python_functions`
/// configuration (`test_*`) is the dominant convention in the factory's
/// Python repos (worldarchitect.ai uses `python_functions = ["test_*"]`),
/// so we mirror it: any top-level `def test_xxx(...)` counts as a test
/// fn. Decorator-prefixed tests (`@pytest.mark.parametrize` over
/// `test_xxx`, `@pytest.fixture` consumers, etc.) are not parsed at this
/// layer — pytest's collection already runs at execution time and the
/// verdict parser reads the resulting `nodeid`s.
///
/// Like the cargo variant, the parser is a pure function of the source
/// string: no I/O, no subprocess. The diff-aware scoping in
/// `compute_targeted_test_fns` operates on the returned names so the
/// targeting contract (issue #387 r6 P1 #5) is runner-agnostic.
///
/// Python does not have a native "skip" attribute equivalent to cargo's
/// `#[ignore]`; pytest uses `@pytest.mark.skip` / `@pytest.mark.skipif`
/// decorators AND runtime `pytest.skip()` calls. The runtime detector
/// treats ALL targeted tests as expected to run and records `NEVER_RAN`
/// when the verdict parser cannot find a `PASSED` line — mirroring the
/// cargo failure mode the r5 contract defined. We do NOT parse skip
/// decorators at this layer because the verdict parser already
/// distinguishes "ran + passed" from "did not run" via the pytest
/// stdout; decorating with a skip decorator that doesn't trigger at
/// collection time would still produce a `PASSED` line.
pub fn discover_pytest_test_fns(source: &str) -> Vec<TestFnInfo> {
    let mut out: Vec<TestFnInfo> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        // Match `def test_<name>(` — leading `def` (possibly preceded by
        // `@...` decorator lines) and a name starting with `test_`.
        // Indentation is preserved but the function must be top-level or
        // class-method-style; the prefix check `def test_` is the
        // authoritative one for pytest's collection convention.
        let after_def = match trimmed.strip_prefix("def ") {
            Some(rest) => rest,
            None => continue,
        };
        // First identifier char must be `t` and the name must start
        // with `test_` — pytest skips functions not matching this
        // pattern even if they're decorated.
        if !after_def.starts_with("test_") {
            continue;
        }
        // Walk the identifier until `(`. The name is everything before
        // the opening paren (or end-of-line if absent, which is malformed
        // and gets ignored).
        let name_end = after_def
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .last()
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        if name_end == 0 {
            continue;
        }
        let name = &after_def[..name_end];
        // Optional sanity: ensure the next char is `(` so we don't pick
        // up trailing references like `def test_alpha` (no parens, would
        // be a syntax error in Python anyway, but be defensive).
        let rest = &after_def[name_end..];
        if !rest.starts_with('(') {
            continue;
        }
        out.push(TestFnInfo {
            name: name.to_string(),
            skip_reason: None,
        });
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
}

// (No trailing helper — the original `_silence_unused` was removed: it
// triggered `clippy::items_after_test_module` and served no purpose.)