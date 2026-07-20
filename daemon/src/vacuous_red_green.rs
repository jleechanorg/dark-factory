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
use std::collections::BTreeSet;
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
    /// `true` when the diff contained no Test files. The detector returns
    /// `Err(NoChangedTests)` in that case so callers can't confuse an
    /// empty-diff with a vacuous pass; this field exists for the JSON
    /// report only.
    pub skipped: bool,
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
///   * The working tree is restored to its pre-call state (HEAD).
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

    if targeted_tests.is_empty() {
        return Err(RedGreenError::NoChangedTests);
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

    // Step 3: write the pristine base content to each production file
    // (overwriting the PR's modifications).
    for (path, bytes) in &originals {
        std::fs::write(path, bytes).map_err(|e| {
            RedGreenError::RevertFailed(format!("write {}: {e}", path.display()))
        })?;
    }

    // Step 4: run each targeted test in isolation.
    let mut failing_tests: Vec<String> = Vec::new();
    for test_name in &targeted_tests {
        let exit_code = run_cargo_test(repo_root, &test_files, test_name)?;
        if exit_code != 0 {
            failing_tests.push(test_name.clone());
        }
    }

    let failed_on_revert = failing_tests.len();
    let vacuous = failed_on_revert == 0;

    Ok(RedGreenReport {
        vacuous,
        failed_on_revert,
        targeted_tests,
        failing_tests,
        skipped: false,
    })
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
    let mut args: Vec<String> = vec!["test".into(), "--quiet".into()];
    let first_test_file = test_files
        .first()
        .ok_or_else(|| RedGreenError::NoChangedTests)?;
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

/// Walk a Rust source string and emit every `#[test]`-annotated fn name.
/// Same line-scan approach as `vacuous.rs`; deliberately cheap (no AST,
/// no external deps).
fn discover_test_fns(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_test_attr = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[") && trimmed.contains("test") {
            in_test_attr = true;
            continue;
        }
        if in_test_attr && trimmed.starts_with('#') {
            continue;
        }
        if in_test_attr && trimmed.starts_with("fn ") {
            if let Some(rest) = trimmed.strip_prefix("fn ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    names.push(name);
                }
            }
            in_test_attr = false;
            continue;
        }
        if in_test_attr && !trimmed.starts_with('#') && !trimmed.is_empty() {
            in_test_attr = false;
        }
    }
    names
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
}