// Runtime red-green vacuous-test detector — issue #387 / bead jleechan-ijod
// (r3 — issue #408).
//
// r2 acceptance (issue #387):
//   1. Revert the non-test diff for a PR.
//   2. Run the PR's new/changed tests against the reverted source.
//   3. Require at least one test to FAIL after revert.
//   4. All-green-on-revert == vacuous coverage (coder-fixable red).
//   5. Fixture PR with a vacuous test is flagged.
//   6. Fixture with a genuine red-green test passes the gate.
//   7. Runtime bounded: only tests added/modified by the PR are run.
//
// r3 acceptance (issue #408 — bead jleechan-1a5e):
//   P1-1  three-check coverage on the report: (a) green_on_head,
//         (b) failed_on_revert, (c) baseline_passed. `BaselineFailed` is no
//         longer dead code; the gate requires ALL THREE.
//   P1-3  cargo test invoked with `--manifest-path <repo>/daemon/Cargo.toml`
//         so on dark-factory layout the detector does NOT return a false
//         pass via NEVER_RAN. `check_red_green` takes an explicit
//         `manifest_path` argument.
//   P1-4  `#[ignore]`-attributed tests are SKIPPED (not counted as coverage
//         proof). Tests with `#[ignore]` but no `skip_reason` reason are
//         surfaced separately so the gate can reject them.
//   P1-5  scope is the ADDED or MODIFIED test fn within the file, not the
//         whole file's `#[test]` set. Pre-existing fns in a changed test
//         file are NOT re-run.
//
// The runtime check is wired through
// `check_red_green(repo_root, manifest_path, base_ref, changed)`:
//
//   * Pre-computes the test-fn set at `base_ref` and diffs against the
//     head's set to discover ONLY the added or modified fns (P1-5).
//   * Runs (a) green-on-head: targeted tests against head tree (must pass).
//   * Runs (b) red-on-revert: revert production files, run targeted tests
//     (at least one must FAIL).
//   * Runs (c) baseline-main sanity: cargo test on the pristine base tree
//     must compile and pass (BaselineFailed is now wired, not dead).
//   * Restores the production diff regardless of pass/fail.
//   * Returns `RedGreenReport { vacuous, failed_on_revert, green_on_head,
//     baseline_passed, targeted_tests, failing_tests, ignored_tests,
//     ignored_without_skip_reason, manifest_path_used }`.

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
    /// r3 (cursor-agent review of PR #410, fix (a)): True iff at least
    /// one `failing_tests` entry is a REAL test failure (NOT a synthetic
    /// `:NEVER_RAN` / `:COMPILE_FAILED_ON_REVERT` marker). When this is
    /// false but `failed_on_revert > 0`, every failure is a "never
    /// actually ran" outcome — the gate path maps this to Pending, not
    /// Verified, so a coder cannot smuggle a never-ran test past the
    /// gate by relying on the r1 vacuous-only signal.
    pub genuine_red_on_revert: bool,
    /// r3 (fix (a)): count of real failures (excludes NEVER_RAN).
    pub failed_without_never_ran: usize,
    /// P1-1 (a): True when the targeted tests PASS on the head tree (no
    /// revert). Required so a deliberately-broken-on-head test cannot
    /// satisfy the gate via revert-makes-it-pass.
    pub green_on_head: bool,
    /// P1-1 (c): True when cargo test on the pristine `base_ref` tree
    /// compiles and passes every test. If false, the red-green signal is
    /// meaningless because the baseline was already broken.
    pub baseline_passed: bool,
    /// Names of tests that were actually run (bounded: only the PR's
    /// added/modified test fns per P1-5).
    pub targeted_tests: Vec<String>,
    /// Names of tests that ran and FAILED on the reverted tree.
    pub failing_tests: Vec<String>,
    /// P1-4: names of `#[ignore]`-attributed test fns that were excluded
    /// from the coverage proof (cargo will not run them).
    pub ignored_tests: Vec<String>,
    /// P1-4: names of `#[ignore]`-attributed test fns that have NO
    /// accompanying `skip_reason` (i.e. `#[ignore]` without
    /// `#[ignore = "reason"]`). The gate should reject these so a coder
    /// cannot smuggle a known-broken test into the green bar by ignoring
    /// it silently.
    pub ignored_without_skip_reason: Vec<String>,
    /// P1-3: the manifest path the detector actually used for `cargo
    /// test`. Surfaced on the report so the gate path (and the on-disk
    /// telemetry) can verify it matches the operator's expectation
    /// (`daemon/Cargo.toml` on dark-factory).
    pub manifest_path_used: PathBuf,
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
    #[error("manifest path {0} does not exist — check the operator config (expected daemon/Cargo.toml)")]
    ManifestPathMissing(PathBuf),
}

/// Run the runtime red-green check against `repo_root`. `manifest_path` is
/// the Cargo manifest the detector MUST use for every `cargo` invocation
/// (P1-3 — on dark-factory layout `repo_root` has no top-level
/// `Cargo.toml`; the only one lives at `<repo>/daemon/Cargo.toml`).
/// `base_ref` is the git ref the PR is measured against; the diff between
/// `base_ref` and the working tree is the production+test delta to revert.
/// `changed` is the list of `(path, FileClass)` pairs the caller has
/// already classified from `git diff --name-only base_ref...HEAD`.
///
/// Pre-conditions:
///   * `repo_root` is inside a git working tree
///   * `base_ref` resolves to a commit
///   * At least one `Test` path is present in `changed`
///   * `manifest_path` exists on disk
///
/// Post-conditions (regardless of return value):
///   * The working tree is restored to its pre-call state.
pub fn check_red_green(
    repo_root: &Path,
    manifest_path: &Path,
    base_ref: &str,
    changed: &[(PathBuf, FileClass)],
) -> Result<RedGreenReport, RedGreenError> {
    if changed.is_empty() {
        return Err(RedGreenError::NoChangedTests);
    }
    if !manifest_path.exists() {
        return Err(RedGreenError::ManifestPathMissing(manifest_path.to_path_buf()));
    }

    let test_files: Vec<PathBuf> = changed
        .iter()
        .filter(|(_, k)| *k == FileClass::Test)
        .map(|(p, _)| p.clone())
        .collect();
    if test_files.is_empty() {
        return Err(RedGreenError::NoChangedTests);
    }

    // Step 1: discover the test fn names in each changed test file at HEAD.
    // Track which ones carry `#[ignore]` (P1-4) — these are EXCLUDED from
    // the coverage proof entirely. The set of HEAD fns minus the set of
    // pre-existing fns at `base_ref` = the added-or-modified set (P1-5).
    //
    // r3 fix (b) — modified-fn detection: a fn present in BOTH base and
    // head is only counted as a coverage proof when its BODY has actually
    // changed. r1 skipped such fns entirely, letting a coder leave a
    // pre-existing test fn untouched and still call it coverage.
    let mut targeted: BTreeSet<String> = BTreeSet::new();
    let mut ignored: BTreeSet<String> = BTreeSet::new();
    let mut ignored_no_reason: BTreeSet<String> = BTreeSet::new();
    for path in &test_files {
        let src = std::fs::read_to_string(path).map_err(|e| {
            RedGreenError::Git(format!("read test file {}: {e}", path.display()))
        })?;
        let head_fns = discover_test_fns(&src);
        let head_bodies = extract_fn_bodies(&src);
        let base_src = read_base_version(repo_root, base_ref, path)?;
        let base_bodies = extract_fn_bodies(&base_src);
        let (attr_index, skip_reasons) = build_ignore_index(&src);
        for name in head_fns {
            let is_in_base = base_bodies.contains_key(&name);
            let body_changed = match (head_bodies.get(&name), base_bodies.get(&name)) {
                (Some(h), Some(b)) => h != b,
                _ => true, // present in head but missing in base → "added"
            };
            if is_in_base && !body_changed {
                // Pre-existing fn at HEAD with IDENTICAL body: NOT a
                // coverage proof for this PR. The coder didn't change
                // it; the test isn't measuring anything new.
                continue;
            }
            // Added or modified: count as coverage proof unless `#[ignore]`.
            if attr_index.contains(&name) {
                ignored.insert(name.clone());
                if !skip_reasons.contains(&name) {
                    ignored_no_reason.insert(name.clone());
                }
                continue;
            }
            targeted.insert(name);
        }
    }
    let targeted_tests: Vec<String> = targeted.iter().cloned().collect();
    let ignored_tests: Vec<String> = ignored.iter().cloned().collect();
    let ignored_without_skip_reason: Vec<String> = ignored_no_reason.iter().cloned().collect();

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

    // Step 3 (P1-1 c): baseline-main sanity. On the pristine `base_ref`
    // tree, cargo test must compile and pass. If it doesn't, the red-green
    // signal is meaningless — BaselineFailed is now wired (no longer dead
    // code per the r2 review).
    let baseline_passed =
        run_cargo_baseline(repo_root, manifest_path, base_ref).is_ok();

    // Step 4: green-on-head (P1-1 a). Run the targeted tests against the
    // head tree (no revert) and require they pass. This catches a
    // deliberately-broken-on-head test that would otherwise pass the
    // gate via "revert makes it pass" (a broken test stays broken on
    // revert, so the revert-side pass would be vacuously true).
    let green_on_head = if targeted_tests.is_empty() {
        // No added/modified tests to run on head — vacuously green. This
        // is the only path where the green_on_head check is trivially
        // satisfied; the gate still requires `baseline_passed` and at
        // least one failing test on revert.
        true
    } else {
        run_targeted_tests(repo_root, manifest_path, &test_files, &targeted_tests).is_ok()
    };

    // Step 5: revert ONLY the production files. Test files remain
    // unchanged so cargo can still find the test target.
    apply_revert(repo_root, &production_diff)?;

    // Always restore on the way out — even if cargo panics or fails.
    let outcome = run_cargo_tests_against_reverted(
        repo_root,
        manifest_path,
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

    // r3 fix (a): distinguish real test failures from synthetic
    // `:NEVER_RAN` / `:COMPILE_FAILED_ON_REVERT` markers so the gate
    // path can return Pending (not Verified) when every observation is
    // "never actually ran".
    let real_failures: Vec<String> = failing_tests
        .iter()
        .filter(|t| {
            !t.ends_with(":NEVER_RAN")
                && t.as_str() != "__COMPILE_FAILED_ON_REVERT__"
        })
        .cloned()
        .collect();
    let failed_without_never_ran = real_failures.len();
    let genuine_red_on_revert = failed_without_never_ran > 0;

    Ok(RedGreenReport {
        vacuous,
        failed_on_revert,
        genuine_red_on_revert,
        failed_without_never_ran,
        green_on_head,
        baseline_passed,
        targeted_tests,
        failing_tests,
        ignored_tests,
        ignored_without_skip_reason,
        manifest_path_used: manifest_path.to_path_buf(),
    })
}

/// r3 (cursor-agent fix (a) + (c)): collapse a `RedGreenReport` into the
/// gate's `VacuousRedGreenStatus`. Mapping:
///
/// - `ignored_without_skip_reason` non-empty → `Failed` (caller cannot
///   silently smuggle a known-broken test into the green bar).
/// - `vacuous=false && genuine_red_on_revert=true` → `Verified` (real
///   red-green; gate passes).
/// - `vacuous=true && genuine_red_on_revert=false` → `Pending` (every
///   observation is NEVER_RAN / COMPILE-FAILED; transient signal —
///   wait and re-check, do NOT churn a reroll).
/// - `vacuous=true && genuine_red_on_revert=true` → `Failed` (at least
///   one real test failed on revert but something else in the report is
///   also vacuous — surface as Failed so the operator can investigate).
/// - `vacuous=true && !green_on_head` → `Failed` (head regression).
/// - `vacuous=true && !baseline_passed` → `Failed` (base broken).
/// - everything else (no failing_tests, no ignored-no-reason,
///   all three checks green) → `Verified`.
pub fn to_gate_status(report: &RedGreenReport) -> crate::verifier::VacuousRedGreenStatus {
    use crate::verifier::VacuousRedGreenStatus;
    // (c) ignored_without_skip_reason is fatal regardless of the other
    // signals — it means the PR has a known-broken test marked as
    // `#[ignore]` without an explanatory reason.
    if !report.ignored_without_skip_reason.is_empty() {
        return VacuousRedGreenStatus::Failed(format!(
            "{} test(s) marked #[ignore] without a skip_reason: {:?}",
            report.ignored_without_skip_reason.len(),
            report.ignored_without_skip_reason
        ));
    }
    if !report.baseline_passed {
        return VacuousRedGreenStatus::Failed(
            "baseline tree failed cargo test (P1-1c)".to_string(),
        );
    }
    if !report.green_on_head {
        return VacuousRedGreenStatus::Failed(format!(
            "targeted tests fail on head (P1-1a): {:?}",
            report.failing_tests
        ));
    }
    if report.vacuous {
        // vacuous=true with EMPTY failing_tests is true vacuity — every
        // targeted test passed on revert, so the test suite proves
        // nothing. r5 (cursor-agent + codex review of PR #420): this is
        // a real defect, the gate must Fail. The Pending branch below
        // is reserved for the synthetic-NEVER_RAN-only case (non-empty
        // failing_tests, but every entry is `:NEVER_RAN` /
        // `__COMPILE_FAILED_ON_REVERT__`).
        if !report.genuine_red_on_revert {
            if report.failing_tests.is_empty() {
                return VacuousRedGreenStatus::Failed(format!(
                    "vacuous red-green: targeted tests all pass on revert \
                     (no genuine red); targeted={:?}",
                    report.targeted_tests
                ));
            }
            return VacuousRedGreenStatus::Pending(format!(
                "no genuine red on revert: every observation is NEVER_RAN \
                 or COMPILE_FAILED; failing_tests={:?}",
                report.failing_tests
            ));
        }
        // vacuous=true with a real failure is unusual (the real failure
        // is also in failing_tests) — surface as Failed.
        return VacuousRedGreenStatus::Failed(
            "vacuous=true but a real failure was observed on revert".to_string(),
        );
    }
    // vacuous=false with synthetic-only failing_tests — Pending (transient,
    // never-ran noise, not Verified).
    if !report.failing_tests.is_empty() && !report.genuine_red_on_revert {
        return VacuousRedGreenStatus::Pending(format!(
            "failing_tests contains only synthetic entries \
             (NEVER_RAN / COMPILE_FAILED); failing_tests={:?}",
            report.failing_tests
        ));
    }
    VacuousRedGreenStatus::Verified
}

/// Read the version of `path` at `base_ref` from the git index. Used to
/// diff the test-fn set between base and head (P1-5). Returns the empty
/// string when the file did not exist at base (a brand-new test file —
/// every fn in head is "added").
fn read_base_version(
    repo_root: &Path,
    base_ref: &str,
    path: &Path,
) -> Result<String, RedGreenError> {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    let out = Command::new("git")
        .current_dir(repo_root)
        .args(["show", &format!("{base_ref}:{}", rel.display())])
        .output()
        .map_err(|e| RedGreenError::Git(format!("spawn git show: {e}")))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    // File didn't exist at base. Return empty source — every HEAD fn is
    // "added" relative to the empty baseline.
    Ok(String::new())
}

/// Extract `(fn_name → body_text)` pairs from a Rust source. The body is
/// the contiguous block of braces following the fn signature, dedented to
/// ignore surrounding whitespace. Used for body-level diff between base
/// and head (cursor-agent r2 fix (b) — modified-fn detection).
fn extract_fn_bodies(source: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_test_attr = trimmed.starts_with("#[test]")
            || trimmed.starts_with("#[tokio::test]")
            || trimmed.starts_with("#[rstest]");
        if !is_test_attr {
            i += 1;
            continue;
        }
        // Walk through attributes and capture the fn signature.
        let mut j = i;
        while j < lines.len() && lines[j].trim_start().starts_with("#[") {
            j += 1;
        }
        let sig_line = match lines.get(j) {
            Some(l) => l,
            None => break,
        };
        let name = match parse_test_fn_name(sig_line.trim_start()) {
            Some(n) => n,
            None => {
                i = j + 1;
                continue;
            }
        };
        // Find the opening `{` on the sig line OR on subsequent lines.
        let mut k = j;
        while k < lines.len() && !lines[k].contains('{') {
            k += 1;
        }
        if k >= lines.len() {
            i = j + 1;
            continue;
        }
        // Walk brace-balanced to capture the body.
        let mut depth: i32 = 0;
        let mut body_lines: Vec<&str> = Vec::new();
        let mut started = false;
        let mut idx = k;
        for line in &lines[k..] {
            body_lines.push(line);
            for ch in line.chars() {
                if ch == '{' {
                    depth += 1;
                    started = true;
                } else if ch == '}' {
                    depth -= 1;
                }
            }
            if started && depth == 0 {
                break;
            }
            idx += 1;
        }
        let _ = idx;
        let body = body_lines.join("\n");
        out.insert(name, body);
        i = j + 1;
    }
    out
}

/// Scan a Rust source file for `#[test] fn <name>(` declarations AND
/// record which of them carry `#[ignore]` (with or without an inline
/// `skip_reason` string). Returns `(ignored_set, skip_reason_set)`.
fn build_ignore_index(source: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let lines: Vec<&str> = source.lines().collect();
    let mut ignored = BTreeSet::new();
    let mut with_reason = BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_test_attr = trimmed.starts_with("#[test]")
            || trimmed.starts_with("#[tokio::test]")
            || trimmed.starts_with("#[rstest]");
        if !is_test_attr {
            i += 1;
            continue;
        }
        // Walk forward through attribute lines. Any `#[ignore]` or
        // `#[ignore = "reason"]` between here and the `fn` line counts as
        // ignore attribution. Multi-line `#[ignore = "..."]` is matched
        // by line because Rust commonly formats it on one line.
        let mut attr_start = i;
        let mut is_ignored = false;
        let mut has_skip_reason = false;
        while attr_start < lines.len() && lines[attr_start].trim_start().starts_with("#[") {
            let attr_text = lines[attr_start].trim_start();
            if attr_text.starts_with("#[ignore") {
                is_ignored = true;
                // `#[ignore = "reason"]` -> has_skip_reason = true.
                if attr_text.contains('=') {
                    has_skip_reason = true;
                }
            }
            attr_start += 1;
        }
        if let Some(name) = lines
            .get(attr_start)
            .and_then(|l| parse_test_fn_name(l.trim_start()))
        {
            if is_ignored {
                ignored.insert(name.clone());
                if has_skip_reason {
                    with_reason.insert(name);
                }
            }
        }
        i = attr_start + 1;
    }
    (ignored, with_reason)
}

/// Scan a Rust source file for `#[test] fn <name>(` declarations.
/// Multi-attribute test fns (e.g. `#[tokio::test]`) are supported by
/// looking one line above the `fn` for a `#[` line containing `test`.
pub fn discover_test_fns(source: &str) -> Vec<String> {
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

pub fn parse_test_fn_name(line: &str) -> Option<String> {
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

/// Run `cargo test` against the pristine base tree (no revert). Used by
/// P1-1 (c) baseline-main sanity. Returns `Ok(())` when every test passes
/// on base, `Err(reason)` otherwise.
///
/// jleechan-1a5e r5 (CodeRabbit review of PR #420): the previous
/// implementation only exported `GIT_BASE_REF` into the cargo env and
/// invoked cargo against the daemon's current worktree (which is the
/// PR head after the dispatch cycle). That meant the baseline sanity
/// check was actually testing the PR head tree, not the base tree, and
/// the `--quiet` flag masked the per-test output. The fix creates a
/// throw-away git worktree at `base_ref`, runs cargo there, and removes
/// the worktree on the way out (success or failure).
fn run_cargo_baseline(
    repo_root: &Path,
    manifest_path: &Path,
    base_ref: &str,
) -> Result<(), String> {
    // Materialize the pristine base_ref tree in a throw-away worktree
    // so cargo test runs against the actual baseline, not the daemon's
    // current worktree (which is the PR head).
    let worktree_path = std::env::temp_dir().join(format!(
        "vacuous_baseline_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let add_status = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "add",
            "--detach",
            worktree_path.to_string_lossy().as_ref(),
            base_ref,
        ])
        .output()
        .map_err(|e| format!("spawn git worktree add: {e}"))?;
    if !add_status.status.success() {
        return Err(format!(
            "git worktree add (base={base_ref}) failed: {}",
            String::from_utf8_lossy(&add_status.stderr)
        ));
    }

    // Resolve the manifest path inside the worktree. The CLI is invoked
    // with a repo-root-relative manifest (e.g. `daemon/Cargo.toml`); in
    // the worktree the same relative path resolves to the file's pristine
    // base version.
    let worktree_manifest = if manifest_path.is_absolute() {
        if let Ok(rel) = manifest_path.strip_prefix(repo_root) {
            worktree_path.join(rel)
        } else {
            manifest_path.to_path_buf()
        }
    } else {
        worktree_path.join(manifest_path)
    };

    let result = Command::new("cargo")
        .current_dir(&worktree_path)
        .args(["test", "--quiet", "--manifest-path"])
        .arg(&worktree_manifest)
        .output();

    // Always remove the worktree on the way out, even on cargo failure.
    let _ = Command::new("git")
        .current_dir(repo_root)
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_string_lossy().as_ref(),
        ])
        .output();

    let out = result.map_err(|e| format!("spawn cargo test (baseline): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rc={:?} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// Run the targeted tests against the head tree (no revert). Returns
/// `Ok(())` when every targeted test passes on head; `Err(reason)`
/// otherwise. Used by P1-1 (a).
fn run_targeted_tests(
    repo_root: &Path,
    manifest_path: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
) -> Result<(), String> {
    if targeted_tests.is_empty() {
        return Ok(());
    }
    let mut last_err: Option<String> = None;
    for tf in test_files {
        let basename = tf
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad test file path: {}", tf.display()))?;
        let mut args: Vec<String> = vec![
            "test".to_string(),
            "--quiet".to_string(),
            "--manifest-path".to_string(),
            manifest_path.to_string_lossy().to_string(),
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
            .map_err(|e| format!("spawn cargo test (head): {e}"))?;
        if !out.status.success() {
            last_err = Some(format!(
                "test file {} failed on head: rc={:?} stderr={}",
                basename,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    last_err.map_or(Ok(()), Err)
}

fn capture_production_diff(
    repo_root: &Path,
    base_ref: &str,
) -> Result<Vec<u8>, RedGreenError> {
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
            if keep_block && !current_block.is_empty() {
                out.push_str(&current_block);
            }
            current_block.clear();
            let parts: Vec<&str> = line.split_whitespace().collect();
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
    manifest_path: &Path,
    test_files: &[PathBuf],
    targeted_tests: &[String],
) -> Result<(usize, Vec<String>), RedGreenError> {
    if targeted_tests.is_empty() {
        return Ok((0, vec![]));
    }

    let mut failing: Vec<String> = Vec::new();
    let mut compile_errored = false;
    for tf in test_files {
        let basename = tf
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| RedGreenError::Git(format!("bad test file path: {}", tf.display())))?;
        // r3 (cursor-agent fix (a) regression): drop `--quiet` so the
        // per-test PASS/FAIL summary line is emitted. With `--quiet`
        // cargo prints a one-character-per-test progress line (`.`/`F`)
        // and only the final tally — neither contains `test <name>
        // ... ok` / `test <name> ... FAILED`, so the r1 parser
        // mis-classified every green run as NEVER_RAN. Without
        // `--quiet` the standard summary format is restored.
        let mut args: Vec<String> = vec![
            "test".to_string(),
            "--manifest-path".to_string(),
            manifest_path.to_string_lossy().to_string(),
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

        if !out.status.success() && (stderr.contains("error[E") || stdout.contains("error[E"))
            && !stdout.contains(" ... ok")
        {
            compile_errored = true;
        }

        for name in targeted_tests {
            let passed_marker = format!("test {name} ... ok");
            let failed_marker = format!("test {name} ... FAILED");
            if stdout.contains(&failed_marker) || stderr.contains(&failed_marker) {
                failing.push(name.clone());
            } else if !(stdout.contains(&passed_marker)
                || stdout.contains(&format!("test {name} ... ignored")))
            {
                failing.push(format!("{name}:NEVER_RAN"));
            }
        }
    }

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

    #[test]
    fn ignore_index_marks_ignored_test() {
        let src = r#"
#[test]
fn plain() { 1 }

#[test]
#[ignore]
fn ignored_plain() { 1 }

#[test]
#[ignore = "needs fixture"]
fn ignored_with_reason() { 1 }
"#;
        let (ignored, with_reason) = build_ignore_index(src);
        assert!(ignored.contains("ignored_plain"));
        assert!(ignored.contains("ignored_with_reason"));
        assert!(with_reason.contains("ignored_with_reason"));
        assert!(!with_reason.contains("ignored_plain"));
        assert!(!ignored.contains("plain"));
    }
}