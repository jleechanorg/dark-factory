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
    let resolved_manifest: Option<PathBuf> = match manifest_path {
        Some(p) => Some(p.to_path_buf()),
        None => find_cargo_manifest(repo_root),
    };

    // Step 1: discover the test fn names + per-fn skip reasons in each
    // changed test file. `#[ignore]` and `#[ignore = "..."]` populate
    // `skip_reason` so the report can record them instead of silently
    // counting them as "all green on revert" (issue #387 r5 finding 4).
    let mut targeted: BTreeSet<String> = BTreeSet::new();
    let mut skipped: Vec<TestFnInfo> = Vec::new();
    for path in &test_files {
        let src = std::fs::read_to_string(path).map_err(|e| {
            RedGreenError::Git(format!("read test file {}: {e}", path.display()))
        })?;
        for info in discover_test_fns_with_skip(&src) {
            if targeted.insert(info.name.clone()) {
                if let Some(reason) = info.skip_reason {
                    skipped.push(TestFnInfo {
                        name: info.name,
                        skip_reason: Some(reason),
                    });
                }
            }
        }
    }
    let targeted_tests: Vec<String> = targeted.iter().cloned().collect();

    // Phase (a) — green-on-PR-head. If the targeted tests don't pass
    // before any revert, the gate reports `GreenFailed` immediately.
    let head_pass = run_cargo_tests(
        repo_root,
        &test_files,
        &targeted_tests,
        resolved_manifest.as_deref(),
    )?;
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

    // Phase (c) — baseline-main sanity. We can't easily run cargo against
    // a different commit in-place without disturbing the working tree, so
    // we use `git worktree add --detach` to materialize `base_ref` in a
    // temp dir, run the targeted tests there, and clean up. This catches
    // the "test was already broken before the PR" case where the
    // red-on-revert finding would otherwise be a false positive.
    let baseline_pass = run_baseline_check(
        repo_root,
        base_ref,
        &test_files,
        &targeted_tests,
        resolved_manifest.as_deref(),
    )?;
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
    // run cargo against the reverted tree, restore. The diff capture +
    // restore are best-effort wrappers; we panic-on-restore-fail at the
    // call site (`restore_diff`) so a partial revert never leaves the
    // tree dirty.
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

    let revert_outcome = run_cargo_tests(
        repo_root,
        &test_files,
        &targeted_tests,
        resolved_manifest.as_deref(),
    );

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

/// Run cargo test against the working tree (or a worktree under
/// `baseline_root` for the baseline-main phase) using the resolved
/// manifest. Issue #387 r5 finding 3: `--manifest-path` is required
/// so cargo executes the PR's crate regardless of cwd — without it,
/// `NEVER_RAN` is treated as a real pass on multi-crate layouts.
fn run_cargo_tests(
    repo_root: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
    manifest: Option<&Path>,
) -> Result<CargoOutcome, RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok(CargoOutcome {
            failing: vec![],
            compile_errored: false,
        });
    }

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

        let out = Command::new("cargo")
            .current_dir(repo_root)
            .args(&args)
            .output()
            .map_err(|e| RedGreenError::Git(format!("spawn cargo test: {e}")))?;
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

    let result = run_cargo_tests(&tmp, test_files, targeted_tests, baseline_manifest.as_deref());

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
}

// (No trailing helper — the original `_silence_unused` was removed: it
// triggered `clippy::items_after_test_module` and served no purpose.)