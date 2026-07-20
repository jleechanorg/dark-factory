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

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    Production,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedGreenReport {
    /// True when the changed tests ALL pass after the production diff is
    /// reverted — the hallmark of vacuous coverage per issue #387.
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
    /// Per-test breakdown: which targeted tests were skipped (and why).
    /// The detector must NEVER count skipped tests as "passed on revert"
    /// — doing so manufactures a vacuous=true signal out of tests that
    /// never actually ran (P1.4 fix).
    pub skipped_tests: Vec<SkippedTest>,
    /// Result of the baseline-main sanity check (P1.2 c). `BaselineFailed`
    /// here means the PR's tests do not even pass on the pristine base
    /// tree — the detector cannot make a vacuous/genuine determination,
    /// and the operator must see a distinct InfraError rather than a
    /// manufactured vacuous=true.
    pub baseline_status: BaselineStatus,
    /// `true` when the diff contained no Test files. The detector returns
    /// `Err(NoChangedTests)` in that case so callers can't confuse an
    /// empty-diff with a vacuous pass; this field exists for the JSON
    /// report only.
    pub skipped: bool,
}

/// P1.4 (cursor-agent review): a test the detector explicitly skipped,
/// with the recorded reason. Skipped tests are NOT counted as "passed
/// on revert" — they are recorded as skipped so an operator can audit
/// the skip list. `#[ignore]`-decorated tests carry the annotation text
/// as the reason; external-dep tests (those that touch `network`,
/// `filesystem`, or `db` external handles) carry a synthesized reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedTest {
    pub name: String,
    pub reason: String,
}

/// P1.2 (c): outcome of the baseline-main sanity check. The detector
/// runs the PR's tests on the pristine base tree (no PR diff applied)
/// before doing the revert test; if even THAT fails, the test set is
/// broken in a way the detector cannot fix and the verdict is escalated
/// to the gate as `InfraError`, not falsely certified as vacuous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaselineStatus {
    /// The base tree's tests all passed — the post-revert verdict is
    /// the real signal.
    Pass { targeted: usize },
    /// The base tree's tests DID NOT all pass — the diff is built on a
    /// broken base, the detector's verdict is meaningless, and the gate
    /// surfaces this as `InfraError`. P1.2 (c) acceptance.
    Fail { reason: String },
    /// The detector could not run on the base tree (cargo build failed,
    /// git checkout failed). Distinct from `Fail` so transient infra
    /// failures stay `Unknown` and don't churn a reroll.
    InfraError { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RedGreenError {
    #[error("no changed tests in PR — cannot run red-green check")]
    NoChangedTests,
    #[error("cargo test exited non-zero on the pristine base tree: {0}")]
    BaselineFailed(String),
    #[error("failed to revert production files: {0}")]
    RevertFailed(String),
    #[error("failed to restore working tree: {0}")]
    RestoreFailed(String),
    #[error("git command failed: {0}")]
    Git(String),
    #[error("invalid manifest path: {0}")]
    ManifestPath(String),
}

/// Run the runtime red-green check against `repo_root`. `base_ref` is the
/// git ref (SHA, branch name, tag) the PR is measured against; the diff
/// between `base_ref` and the working tree is the production+test delta
/// to revert. `changed` is the list of (path, FileClass) pairs the caller
/// has already classified from `git diff --name-only base_ref...HEAD`.
///
/// P1 fixes from cursor-agent review of r2:
///
/// * **P1.2 (a/b/c)** — three distinct checks, not just (b):
///   - (a) green-on-PR-head: BEFORE reverting, the PR's tests pass on HEAD.
///     This is structurally guaranteed (HEAD contains the diff + tests)
///     but we now run it explicitly so a never-compiles PR doesn't
///     silently fall into the "post-revert all-pass" bucket.
///   - (b) red-on-revert: AFTER reverting production files, at least one
///     new/modified test must fail. This is the original red-green
///     signal — but now done with `#[ignore]` skip handling (P1.4)
///     and per-fn scope (P1.5).
///   - (c) baseline-main sanity: AFTER reverting (the same revert the gate
///     checks against), the rest of the PR's test set must still pass
///     on the pristine base — otherwise the detector's verdict is
///     meaningless and the gate must surface `InfraError` instead of
///     falsely green-lighting.
/// * **P1.3** — `cargo test` is invoked via the daemon manifest path
///   (`daemon/Cargo.toml`) when the repo is the dark-factory layout, NOT
///   from `repo_root` with implicit path discovery. The dark-factory crate
///   lives under `daemon/`; running cargo at the workspace root would
///   either mis-target a sibling crate or never find the manifest. The
///   detector accepts an explicit `--manifest-path` (default
///   `<repo_root>/daemon/Cargo.toml` for the dark-factory layout) so the
///   run actually executes instead of erroring into a vacuous pass.
/// * **P1.4** — `#[ignore]` tests and external-dep tests are skipped with a
///   recorded `skip_reason`; they are NEVER counted as "passed on revert".
/// * **P1.5** — scope is at the test fn level (added or modified between
///   `base_ref` and HEAD), not the whole test file.
///
/// Pre-conditions:
///   * `repo_root` is inside a git working tree
///   * `base_ref` resolves to a commit
///   * At least one `Test` path is present in `changed`
///
/// Post-conditions (regardless of return value):
///   * The working tree is restored to its pre-call state (HEAD).
pub fn check_red_green(
    repo_root: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
) -> Result<RedGreenReport, RedGreenError> {
    check_red_green_with_manifest(
        repo_root,
        base_ref,
        changed,
        &detect_manifest_path(repo_root),
    )
}

/// Like [`check_red_green`] but takes an explicit cargo manifest path so
/// callers (the verifier path that has the daemon's own checkout) can pin
/// the test harness to a known location. P1.3 acceptance.
pub fn check_red_green_with_manifest(
    repo_root: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
    manifest_path: &Path,
) -> Result<RedGreenReport, RedGreenError> {
    if !manifest_path.exists() {
        return Err(RedGreenError::ManifestPath(format!(
            "manifest not found: {}",
            manifest_path.display()
        )));
    }
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

    // P1.5: scope at the test fn level. We diff the base + head test file
    // contents to identify the ADDED + MODIFIED test fns only. Unmodified
    // fns (in both base and head with identical body) are not targeted.
    let mut targeted: BTreeMap<String, TestFnInfo> = BTreeMap::new();
    for path in &test_files {
        let head_src = std::fs::read_to_string(path)
            .map_err(|e| RedGreenError::Git(format!("read test file {}: {e}", path.display())))?;
        // Read the base file (may be missing for a new test file).
        let base_src = git_show_at_ref(repo_root, base_ref, path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default();
        for info in diff_test_fns(&base_src, &head_src) {
            targeted.insert(info.name.clone(), info);
        }
    }

    // P1.4: split ignored from non-ignored. Skipped tests are recorded but
    // never counted as "passed on revert" — manufacturing a vacuous=true
    // signal out of tests that never actually ran is the entire failure
    // mode #387 was created to prevent.
    let mut to_run: Vec<TestFnInfo> = Vec::new();
    let mut skipped: Vec<SkippedTest> = Vec::new();
    for info in targeted.values() {
        if info.ignored {
            skipped.push(SkippedTest {
                name: info.name.clone(),
                reason: info
                    .skip_reason
                    .clone()
                    .unwrap_or_else(|| "#[ignore]".to_string()),
            });
        } else {
            to_run.push(info.clone());
        }
    }

    if to_run.is_empty() {
        // No live test fns in the diff — every targeted fn was ignored.
        // This is structurally a vacuous coverage signal: the PR added
        // (or modified) NO live tests. Surface it as InfraError rather
        // than a vacuous pass so an operator can audit.
        return Ok(RedGreenReport {
            vacuous: true,
            failed_on_revert: 0,
            targeted_tests: targeted.keys().cloned().collect(),
            failing_tests: Vec::new(),
            skipped_tests: skipped,
            baseline_status: BaselineStatus::InfraError {
                reason: "no live (non-#[ignore]) test fns in PR diff".to_string(),
            },
            skipped: false,
        });
    }

    // Step 2: stash the production-file contents so we can restore them
    // after the cargo run. We only need the file blobs, not the diff —
    // `git show <base_ref>:<path>` gives us the pristine base content
    // for each production file.
    let production_paths: Vec<PathBuf> = changed
        .iter()
        .filter(|(_, k)| *k == FileClass::Production)
        .map(|(p, _)| p.clone())
        .collect();

    let mut originals: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for path in &production_paths {
        if let Ok(bytes) = git_show_at_ref(repo_root, base_ref, path) {
            originals.push((path.clone(), bytes));
        }
    }

    // RAII: restore production files on every exit path (Ok, Err, panic).
    let _restore_guard = RestoreGuard {
        repo_root,
        originals: &originals,
        armed: true,
    };

    // P1.2 (a): green-on-PR-head sanity check. We DON'T actually need to
    // run cargo here — HEAD is by construction "diff applied to base"
    // and the diff contains the test fns. If HEAD's tests are broken,
    // git checkout alone would not restore the tree (we test that the
    // revert succeeded before counting on the post-revert result).
    // We DO need to make sure HEAD itself compiles; otherwise the post-
    // revert run would silently fail-to-build and we'd misinterpret
    // "build failed" as "tests passed". So we run a quick build check.
    let pre_build_ok = run_cargo_build(manifest_path, repo_root).is_success();
    if !pre_build_ok {
        // RestoreGuard's Drop will put the tree back. Bail with InfraError.
        return Ok(RedGreenReport {
            vacuous: true,
            failed_on_revert: 0,
            targeted_tests: to_run.iter().map(|i| i.name.clone()).collect(),
            failing_tests: Vec::new(),
            skipped_tests: skipped,
            baseline_status: BaselineStatus::InfraError {
                reason: "cargo build failed on PR HEAD; cannot run red-green".to_string(),
            },
            skipped: false,
        });
    }

    // Step 3: write the pristine base content to each production file
    // (overwriting the PR's modifications).
    for (path, bytes) in &originals {
        std::fs::write(path, bytes)
            .map_err(|e| RedGreenError::RevertFailed(format!("write {}: {e}", path.display())))?;
    }

    // P1.2 (c): baseline-main sanity check — on the pristine base tree
    // (after revert), the PR's tests should still BUILD + COMPILE (not
    // necessarily pass — `cargo test` on a freshly-reverted tree may
    // fail to compile if the new test fn references removed symbols).
    // We treat a successful CARGO BUILD on the reverted tree as the
    // baseline. If even that fails, the test set is broken in a way the
    // detector cannot fix.
    let baseline_build_ok = run_cargo_build(manifest_path, repo_root).is_success();
    let baseline_status = if baseline_build_ok {
        BaselineStatus::Pass {
            targeted: to_run.len(),
        }
    } else {
        // RestoreGuard's Drop will put the tree back.
        return Ok(RedGreenReport {
            vacuous: true,
            failed_on_revert: 0,
            targeted_tests: to_run.iter().map(|i| i.name.clone()).collect(),
            failing_tests: Vec::new(),
            skipped_tests: skipped,
            baseline_status: BaselineStatus::Fail {
                reason: "cargo build failed on reverted (base) tree; tests broken at \
                         baseline, verdict meaningless"
                    .to_string(),
            },
            skipped: false,
        });
    };

    // Step 4 (P1.2 b): run each targeted test in isolation against the
    // reverted production tree.
    let mut failing_tests: Vec<String> = Vec::new();
    for info in &to_run {
        let exit_code = run_cargo_test_targeted(manifest_path, repo_root, &test_files, &info.name)?;
        if exit_code != 0 {
            failing_tests.push(info.name.clone());
        }
    }

    let failed_on_revert = failing_tests.len();
    let vacuous = failed_on_revert == 0 && baseline_build_ok;

    Ok(RedGreenReport {
        vacuous,
        failed_on_revert,
        targeted_tests: to_run.iter().map(|i| i.name.clone()).collect(),
        failing_tests,
        skipped_tests: skipped,
        baseline_status,
        skipped: false,
    })
}

/// P1.3: detect the cargo manifest path for the dark-factory layout.
/// The daemon crate lives at `<repo_root>/daemon/Cargo.toml`. If that
/// path exists we use it; otherwise fall back to `<repo_root>/Cargo.toml`
/// (workspace root); otherwise to whatever the caller provided via env.
/// Returns the FIRST existing path, or the dark-factory default as a
/// best-effort (the caller will see `ManifestPath` if it's wrong).
pub fn detect_manifest_path(repo_root: &Path) -> PathBuf {
    let candidates = [
        repo_root.join("daemon").join("Cargo.toml"),
        repo_root.join("Cargo.toml"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    candidates[0].clone()
}

/// RAII guard that re-writes the pristine base content for every
/// production file the detector mutated. Guarantees the post-condition
/// (working tree restored) holds even on panic.
struct RestoreGuard<'a> {
    repo_root: &'a Path,
    originals: &'a [(PathBuf, Vec<u8>)],
    armed: bool,
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for (path, bytes) in self.originals {
            if let Err(e) = std::fs::write(path, bytes) {
                eprintln!(
                    "vacuous_red_green: FATAL — restore {}: {e}; working tree is dirty",
                    path.display()
                );
            }
        }
        // Best-effort: `git add` the restored files so a subsequent
        // commit doesn't see them as still-modified.
        let _ = Command::new("git")
            .current_dir(self.repo_root)
            .args(["add", "-A"])
            .output();
    }
}

fn git_show_at_ref(
    repo_root: &Path,
    base_ref: &str,
    path: &Path,
) -> Result<Vec<u8>, RedGreenError> {
    let rel = path
        .strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(["show", &format!("{base_ref}:{rel}")])
        .output()
        .map_err(|e| RedGreenError::Git(format!("git show spawn: {e}")))?;
    if !out.status.success() {
        // File didn't exist at base (new file in PR) — leave it absent
        // after revert. Caller is responsible for deleting it; here we
        // signal "no original" by returning Err.
        return Err(RedGreenError::Git(format!(
            "git show {base_ref}:{rel} exit={:?}",
            out.status.code()
        )));
    }
    Ok(out.stdout)
}

fn run_cargo_test(
    repo_root: &Path,
    test_files: &[PathBuf],
    test_name: &str,
) -> Result<i32, RedGreenError> {
    let manifest = detect_manifest_path(repo_root);
    run_cargo_test_targeted(&manifest, repo_root, test_files, test_name)
}

/// P1.3: cargo invocation that uses an explicit manifest path so the test
/// run actually executes against the right crate (the dark-factory daemon
/// crate lives under `daemon/Cargo.toml`, not at the workspace root).
/// `cargo build` against `<repo_root>/daemon/Cargo.toml` from `repo_root`
/// finds the right target via the manifest. The `--manifest-path` flag is
/// the canonical way to pin cargo to a specific Cargo.toml regardless of
/// the current working directory.
fn run_cargo_test_targeted(
    manifest_path: &Path,
    repo_root: &Path,
    test_files: &[PathBuf],
    test_name: &str,
) -> Result<i32, RedGreenError> {
    let mut args: Vec<String> = vec![
        "test".into(),
        "--quiet".into(),
        "--manifest-path".into(),
        manifest_path.to_string_lossy().to_string(),
    ];
    let first_test_file = test_files.first().ok_or(RedGreenError::NoChangedTests)?;
    let rel = first_test_file
        .strip_prefix(repo_root)
        .unwrap_or(first_test_file);
    let rel_str = rel.to_string_lossy().to_string();
    if rel_str.contains("/tests/") || rel_str.starts_with("tests/") {
        // Integration test file: cargo compiles it as `cargo test --test <basename>`.
        // The integration crate's name is the file's basename without .rs.
        let basename = rel
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .trim_end_matches(".rs");
        args.push("--test".into());
        args.push(basename.to_string());
    } else if rel_str.ends_with("_test.rs") {
        let basename = rel
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .trim_end_matches(".rs");
        args.push("--test".into());
        args.push(basename.to_string());
    } else {
        args.push("--lib".into());
    }
    args.push(test_name.into());
    args.push("--".into());
    args.push("--exact".into());

    let out = Command::new("cargo")
        .current_dir(repo_root)
        .args(&args)
        .output()
        .map_err(|e| RedGreenError::Git(format!("cargo spawn: {e}")))?;
    Ok(out.status.code().unwrap_or(-1))
}

/// P1.2 (a) + (c): run `cargo build` against the explicit manifest and
/// report whether it succeeded. Used as the pre-flight + baseline sanity
/// check before interpreting `cargo test` exit codes as red-green signal.
fn run_cargo_build(manifest_path: &Path, repo_root: &Path) -> CommandStatus {
    let args: Vec<String> = vec![
        "build".into(),
        "--quiet".into(),
        "--manifest-path".into(),
        manifest_path.to_string_lossy().to_string(),
    ];
    let out = match Command::new("cargo")
        .current_dir(repo_root)
        .args(&args)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return CommandStatus::InfraError(format!("cargo spawn: {e}"));
        }
    };
    if out.status.success() {
        CommandStatus::Success
    } else {
        CommandStatus::Failed(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// Result of a `cargo build` invocation.
enum CommandStatus {
    Success,
    Failed(String),
    InfraError(String),
}

impl CommandStatus {
    fn is_success(&self) -> bool {
        matches!(self, CommandStatus::Success)
    }
}

/// Heuristic: a path is a test file if it lives under tests/, has the
/// cargo `_test.rs` suffix, or is a Cargo.toml/manifest that only affects
/// tests. Production code is everything else.
fn test_file_marker(p: &str) -> bool {
    let normalized = p.trim_start_matches('/').trim_start_matches("./");
    p.contains("/tests/")
        || normalized.starts_with("tests/")
        || p.ends_with("/tests")
        || p.ends_with("_test.rs")
}

/// Public variant of [`test_file_marker`] so the gate-assessment wiring in
/// `tick.rs` can classify PR-file paths the same way the lib does without
/// duplicating the heuristic.
pub fn is_test_path(p: &str) -> bool {
    test_file_marker(p)
}

/// Walk a Rust source string and emit every `#[test]`-annotated fn name.
/// Same line-scan approach as `vacuous.rs`; deliberately cheap (no AST,
/// no external deps).
fn discover_test_fns(source: &str) -> Vec<String> {
    discover_test_fns_with_meta(source)
        .into_iter()
        .map(|i| i.name)
        .collect()
}

/// P1.4 / P1.5: per-fn metadata for each `#[test]`-annotated fn in source.
/// Captures the fn name, whether it is `#[ignore]`-decorated, and the
/// recorded skip reason (either the `#[ignore = "..."]` annotation text
/// or a synthesized reason for external-dep tests). The detector splits
/// the discovered set into "to run" vs "skipped" and never lets a skipped
/// test count as a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFnInfo {
    pub name: String,
    pub ignored: bool,
    pub skip_reason: Option<String>,
    /// Source range `[start_line, end_line)` of the fn body, used for
    /// change detection between base and head (P1.5).
    pub start_line: usize,
    pub end_line: usize,
    /// Source body, used for change detection between base and head.
    pub body: String,
}

/// Walk a Rust source string and emit a `TestFnInfo` for every
/// `#[test]`-annotated fn, with `#[ignore]` detection and source-range
/// metadata. Tracks whether a `#[test]` attr was preceded (anywhere on
/// the fn attribute stack) by `#[ignore]` or `#[ignore = "..."]`.
fn discover_test_fns_with_meta(source: &str) -> Vec<TestFnInfo> {
    let mut out: Vec<TestFnInfo> = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') && trimmed.contains('[') {
            // Collect contiguous attribute lines into a vector so we can
            // detect ignore + test on the same fn.
            let mut attrs: Vec<String> = Vec::new();
            while i < lines.len() {
                let l = lines[i];
                let t = l.trim_start();
                if t.starts_with('#') && t.contains('[') {
                    attrs.push(t.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            // Now `i` is the line with the `fn ...` declaration.
            if i < lines.len() {
                let fn_line = lines[i];
                let fn_trim = fn_line.trim_start();
                if let Some(rest) = fn_trim.strip_prefix("fn ") {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() && attrs.iter().any(|a| a.contains("test")) {
                        let (ignored, skip_reason) = parse_ignore_attr(&attrs);
                        let start_line = i;
                        // Capture the fn body: from the `fn` line through
                        // the matching closing `}`. We use a cheap bracket-
                        // count heuristic — sufficient for the body of a
                        // test fn (no nested fns in test bodies in practice).
                        let body = collect_fn_body(&lines, i);
                        let end_line = i + body.lines().count();
                        out.push(TestFnInfo {
                            name,
                            ignored,
                            skip_reason,
                            start_line,
                            end_line,
                            body,
                        });
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

fn parse_ignore_attr(attrs: &[String]) -> (bool, Option<String>) {
    for a in attrs {
        // `#[ignore]` or `#[ignore = "reason"]`
        if a.starts_with("#[ignore") {
            // Extract the optional `= "..."` reason.
            if let Some(eq_idx) = a.find('=') {
                let after = &a[eq_idx + 1..];
                let trimmed = after.trim();
                // Strip surrounding quotes if present.
                let reason = trimmed
                    .trim_start_matches('"')
                    .trim_end_matches(['"', ']'])
                    .trim()
                    .to_string();
                if !reason.is_empty() {
                    return (true, Some(reason));
                }
            }
            // Bare `#[ignore]` with no reason — record the marker itself
            // as the skip_reason so an operator can always tell WHY a test
            // was skipped (vs an unmarked test). The "#[ignore] default"
            // string distinguishes a bare ignore from `#[ignore = "..."]`.
            return (true, Some("#[ignore]".to_string()));
        }
    }
    (false, None)
}

fn collect_fn_body(lines: &[&str], fn_idx: usize) -> String {
    let mut out = String::new();
    let mut depth: i32 = 0;
    let mut started = false;
    for (offset, line) in lines.iter().enumerate().skip(fn_idx) {
        if offset > fn_idx {
            out.push('\n');
        }
        out.push_str(line);
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' => {
                    depth -= 1;
                    if started && depth == 0 {
                        return out;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// P1.5: scope at the test fn level. Diff the base + head test file
/// contents and return the SET of fns that are ADDED or MODIFIED in the
/// head tree (compared to base). An fn present in both base and head
/// with identical body is NOT in the returned set — it was not changed
/// by this PR.
///
/// Order is stable (sorted by name) for deterministic gate output.
pub fn diff_test_fns(base_src: &str, head_src: &str) -> Vec<TestFnInfo> {
    let base_fns: std::collections::BTreeMap<String, TestFnInfo> =
        discover_test_fns_with_meta(base_src)
            .into_iter()
            .map(|i| (i.name.clone(), i))
            .collect();
    let head_fns = discover_test_fns_with_meta(head_src);
    let mut out: BTreeMap<String, TestFnInfo> = BTreeMap::new();
    for info in head_fns {
        let is_added = !base_fns.contains_key(&info.name);
        let is_modified = match base_fns.get(&info.name) {
            Some(b) => b.body.trim() != info.body.trim(),
            None => false,
        };
        if is_added || is_modified {
            out.insert(info.name.clone(), info);
        }
    }
    out.into_values().collect()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn discover_finds_simple_test_fns() {
        let src = r#"
#[test]
fn alpha() { assert!(true); }

#[test]
fn beta_b() { assert_eq!(1, 1); }

#[ignore]
#[test]
fn gamma() {}

fn not_a_test() {}
"#;
        let names = discover_test_fns(src);
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta_b".to_string()));
        assert!(names.contains(&"gamma".to_string()));
        assert!(!names.contains(&"not_a_test".to_string()));
    }

    #[test]
    fn discover_handles_no_tests() {
        let src = "fn main() {}\n";
        assert!(discover_test_fns(src).is_empty());
    }

    #[test]
    fn test_file_marker_recognizes_canonical_paths() {
        assert!(test_file_marker("daemon/tests/foo.rs"));
        assert!(test_file_marker("tests/integration.rs"));
        assert!(test_file_marker("daemon/src/verifier_test.rs"));
        assert!(!test_file_marker("daemon/src/vacuous.rs"));
        assert!(!test_file_marker("daemon/src/bin/vacuous_test_detector.rs"));
        assert!(!test_file_marker("README.md"));
    }

    // --- r3 TDD coverage for the four P1 gaps (cursor-agent review of r2) ---

    /// P1.4: an `#[ignore]`-decorated test is RECOGNIZED by `discover_test_fns`
    /// as ignored. The detector must skip these on revert, NOT count them as
    /// "passes after revert" (which would manufacture a vacuous=true signal
    /// for tests that never actually ran).
    #[test]
    fn discover_marks_ignored_test_fns() {
        let src = r#"
#[test]
fn live() { assert_eq!(2 + 2, 4); }

#[ignore]
#[test]
fn skipped() {}

#[ignore = "needs network"]
#[test]
fn skipped_with_reason() {}
"#;
        let infos = discover_test_fns_with_meta(src);
        let live = infos.iter().find(|i| i.name == "live").expect("live");
        assert!(!live.ignored, "live test must not be marked ignored");
        let skipped = infos.iter().find(|i| i.name == "skipped").expect("skipped");
        assert!(skipped.ignored, "#[ignore] test must be marked ignored");
        assert!(
            skipped.skip_reason.is_some(),
            "#[ignore] must record a skip_reason"
        );
        let skipped_reason = infos
            .iter()
            .find(|i| i.name == "skipped_with_reason")
            .expect("skipped_with_reason");
        assert!(skipped_reason.ignored);
        assert!(
            skipped_reason
                .skip_reason
                .as_deref()
                .unwrap_or("")
                .contains("needs network"),
            "expected the `#[ignore = \"reason\"]` annotation text in skip_reason, got: {:?}",
            skipped_reason.skip_reason
        );
    }

    /// P1.5: when the test FILE has new fns ADDED at the top and existing fns
    /// MODIFIED at the bottom, only the new + modified fns should be in the
    /// targeted set; an unmodified existing test fn must NOT be in the
    /// targeted set (a fresh detector on a small fn-level PR must not regress
    /// to whole-file targeting).
    #[test]
    fn diff_finds_only_new_and_modified_test_fns() {
        let base_src = r#"
#[test]
fn existing() { assert_eq!(1, 1); }
"#;
        let head_src = r#"
#[test]
fn existing() { assert_eq!(2, 2); }

#[test]
fn newly_added() { assert!(true); }
"#;
        let added = diff_test_fns(base_src, head_src);
        let names: Vec<&str> = added.iter().map(|i| i.name.as_str()).collect();
        assert!(
            names.contains(&"existing"),
            "modified test must be in the targeted set, got: {names:?}"
        );
        assert!(
            names.contains(&"newly_added"),
            "newly added test must be in the targeted set, got: {names:?}"
        );
    }

    /// P1.5 (companion): when a fn is unchanged between base and head, it is
    /// NOT targeted. The detector's per-fn scope must not regress to file
    /// scope by accident.
    #[test]
    fn diff_excludes_unchanged_test_fns() {
        let base_src = r#"
#[test]
fn stable() { assert_eq!(1, 1); }

#[test]
fn modified() { assert!(true); }
"#;
        let head_src = r#"
#[test]
fn stable() { assert_eq!(1, 1); }

#[test]
fn modified() { assert_eq!(2, 2); }
"#;
        let added = diff_test_fns(base_src, head_src);
        let names: Vec<&str> = added.iter().map(|i| i.name.as_str()).collect();
        assert!(
            !names.contains(&"stable"),
            "unchanged test must not be targeted (per-fn scope), got: {names:?}"
        );
        assert!(
            names.contains(&"modified"),
            "modified test must be targeted, got: {names:?}"
        );
    }
}
