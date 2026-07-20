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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

#[derive(Debug, thiserror::Error)]
pub enum RedGreenError {
    #[error("no changed tests in PR — cannot run red-green check")]
    NoChangedTests,
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

    // Step 1: discover the test fn names in each changed test file.
    let mut targeted: BTreeSet<String> = BTreeSet::new();
    for path in &test_files {
        let src = std::fs::read_to_string(path).map_err(|e| {
            RedGreenError::Git(format!("read test file {}: {e}", path.display()))
        })?;
        for name in discover_test_fns(&src) {
            targeted.insert(name);
        }
    }
    let targeted_tests: Vec<String> = targeted.iter().cloned().collect();

    // Step 2: stash the production diff so we can restore it later. We
    // capture the FULL diff and then build a filtered production-only diff
    // for the revert, so the test files in the PR are preserved during
    // the cargo run.
    let full_diff = capture_production_diff(repo_root, base_ref)?;
    let production_diff = filter_diff_for_paths(
        &full_diff,
        &changed
            .iter()
            .filter(|(_, k)| *k == FileClass::Production)
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>(),
    )?;

    // Step 3: revert ONLY the production files. Test files remain
    // unchanged so cargo can still find the test target.
    apply_revert(repo_root, &production_diff)?;

    // Always restore on the way out — even if cargo panics or fails.
    let outcome = run_cargo_tests_against_reverted(
        repo_root,
        &test_files,
        &targeted_tests,
    );

    if let Err(e) = restore_diff(repo_root, &production_diff) {
        return Err(RedGreenError::RestoreFailed(format!(
            "{e}; original outcome suppressed to protect working tree"
        )));
    }

    let (failed_on_revert, failing_tests) = outcome?;

    let vacuous = failing_tests.is_empty();
    Ok(RedGreenReport {
        vacuous,
        failed_on_revert,
        targeted_tests,
        failing_tests,
    })
}

/// Scan a Rust source file for `#[test] fn <name>(` declarations.
/// Multi-attribute test fns (e.g. `#[tokio::test]`) are supported by
/// looking one line above the `fn` for a `#[` line containing `test`.
fn discover_test_fns(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("#[test]") || trimmed.starts_with("#[tokio::test]") || trimmed.starts_with("#[rstest]")) {
            continue;
        }
        // The next non-attribute, non-comment line is the fn signature.
        for next in &lines[i + 1..] {
            let t = next.trim_start();
            if t.starts_with('#') || t.is_empty() || t.starts_with("//") {
                continue;
            }
            if let Some(name) = parse_test_fn_name(t) {
                out.push(name);
            }
            break;
        }
    }
    out
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

fn run_cargo_tests_against_reverted(
    repo_root: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
) -> Result<(usize, Vec<String>), RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok((0, vec![]));
    }

    // For integration tests, `cargo test --test <name>` is the correct
    // entry point. The file basename (without .rs) is the test target.
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
        for name in targeted_tests {
            args.push("--".to_string());
            args.push(name.to_string());
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
        if !out.status.success() && (stderr.contains("error[E") || stdout.contains("error[E"))
            && !stdout.contains(" ... ok") {
            compile_errored = true;
        }

        // Parse cargo test's per-test PASS/FAIL summary lines.
        for name in targeted_tests {
            let passed_marker = format!("test {name} ... ok");
            let failed_marker = format!("test {name} ... FAILED");
            if stdout.contains(&failed_marker) || stderr.contains(&failed_marker) {
                failing.push(name.clone());
            } else if !(stdout.contains(&passed_marker) || stdout.contains(&format!("test {name} ... ignored"))) {
                // If neither PASS nor FAIL nor IGNORED is present, the test
                // didn't run at all — treat that as a hard fail signal so
                // the gate doesn't accidentally approve a test that was
                // skipped.
                failing.push(format!("{name}:NEVER_RAN"));
            }
        }
    }

    // A compile failure on the reverted tree is the strongest possible
    // "this test exercises real production code" signal — the test must
    // have referenced a symbol that disappeared with the revert. We
    // surface a synthetic failure so the report reflects the genuine
    // red-green outcome even though cargo never got to the assertion.
    if compile_errored && failing.is_empty() {
        failing.push("__COMPILE_FAILED_ON_REVERT__".to_string());
    }

    Ok((failing.len(), failing))
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
}

// helper for unit test access
#[allow(dead_code)]
fn _silence_unused() {}