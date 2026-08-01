// Runtime red-green vacuous-test detector — integration test.
//
// Issue #387 acceptance criteria:
//   * revert the non-test diff
//   * run the new tests
//   * require at least one to FAIL
//   * fixture PR with a vacuous test is flagged
//   * fixture with a genuine red-green test passes
//   * runtime bounded (only affected tests run)
//
// These tests build two mini-projects under a tempdir, synthesize the
// `git diff` set the way the wrapper would, and assert the runtime
// detector's verdict on each. Each scenario runs `cargo test` in a
// throwaway directory so the host's daemon crate is untouched.

use std::path::PathBuf;
use std::process::Command;

use daemon::vacuous_red_green::{
    check_red_green, check_red_green_with_manifest, FileClass, RedGreenError, Verdict,
};

/// Build a tiny Rust project that exposes one production function and one
/// test that exercises it. Returned `Production` path is the .rs file that
/// holds the production code; `Tests` holds the test-only code. The test
/// names are deterministic so the runtime detector can target them with
/// `cargo test <name>`.
struct MiniProject {
    root: PathBuf,
    /// The git "base" commit the diff is measured against. After this
    /// commit the project contains only an empty lib.rs; the test + prod
    /// changes are committed on top so the detector sees a real diff.
    base_sha: String,
}

#[derive(Clone, Copy)]
enum ProjectKind {
    /// Test asserts a real property of production output. Reverting the
    /// production function to a stub makes the test fail. The runtime
    /// detector should report `vacuous = false` (genuine red-green).
    GenuineRedGreen,
    /// Test passes regardless of production correctness (always-green pin).
    /// Reverting the production function does NOT make the test fail. The
    /// runtime detector should report `vacuous = true`.
    VacuousAlwaysGreen,
}

fn build_mini_project(kind: ProjectKind) -> MiniProject {
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_{}_{}",
        std::process::id(),
        // Unique-per-test suffix so concurrent runs don't clobber each other.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = tmp.clone();
    std::fs::create_dir_all(&root).unwrap();

    // The "base" tree contains ONLY Cargo.toml + an empty src/lib.rs
    // skeleton. The PR's changes (lib.rs body + tests/) are committed on
    // top, so the diff captures exactly the files the detector should
    // classify as production vs test.
    let manifest = r#"[package]
name = "mini"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#;
    std::fs::write(root.join("Cargo.toml"), manifest).unwrap();

    let lib_path = root.join("src").join("lib.rs");
    std::fs::create_dir_all(lib_path.parent().unwrap()).unwrap();
    std::fs::write(&lib_path, "// skeleton lib\n").unwrap();

    run(Command::new("git").current_dir(&root).args(["init", "-q", "-b", "main"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "user.email", "jleechan2015@users.noreply.github.com"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "user.name", "test"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "commit.gpgsign", "false"]));
    run(Command::new("git").current_dir(&root).args(["add", "-A"]));
    run(Command::new("git").current_dir(&root).args(["commit", "-q", "-m", "base"]));
    let base_sha = String::from_utf8(
        run(Command::new("git").current_dir(&root).args(["rev-parse", "HEAD"])),
    )
    .unwrap()
    .trim()
    .to_string();

    // Now layer the PR's changes (lib body + test file) on top of the
    // base. The test will commit this as the head SHA.
    let lib_src = match kind {
        ProjectKind::GenuineRedGreen => GENUINE_LIB,
        ProjectKind::VacuousAlwaysGreen => VACUOUS_LIB,
    };
    let test_src = match kind {
        ProjectKind::GenuineRedGreen => GENUINE_TEST,
        ProjectKind::VacuousAlwaysGreen => VACUOUS_TEST,
    };
    std::fs::write(&lib_path, lib_src).unwrap();

    let tests_dir = root.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let test_path = tests_dir.join("scenario.rs");
    std::fs::write(&test_path, test_src).unwrap();

    MiniProject {
        root,
        base_sha,
    }
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git failed: status={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out.stdout
}

/// Mark the current working tree as the new "head" commit. Returns the new
/// head SHA and stashes the tree's diff against `base_sha` so the test can
/// reason about what changed.
fn commit_current_tree(proj: &MiniProject, message: &str) -> String {
    run(Command::new("git")
        .current_dir(&proj.root)
        .args(["add", "-A"]));
    run(Command::new("git")
        .current_dir(&proj.root)
        .args(["commit", "-q", "-m", message]));
    String::from_utf8(
        run(Command::new("git").current_dir(&proj.root).args(["rev-parse", "HEAD"])),
    )
    .unwrap()
    .trim()
    .to_string()
}

fn classify_changed_files(proj: &MiniProject) -> Vec<(PathBuf, FileClass)> {
    let diff = String::from_utf8(
        run(Command::new("git").current_dir(&proj.root).args([
            "diff",
            "--name-only",
            &format!("{}...HEAD", proj.base_sha),
        ])),
    )
    .unwrap();
    diff.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let p = proj.root.join(l);
            let kind = if l.contains("/tests/") || l.ends_with("_test.rs") || l == "tests/scenario.rs" {
                FileClass::Test
            } else {
                FileClass::Production
            };
            (p, kind)
        })
        .collect()
}

const GENUINE_LIB: &str = r#"
pub fn classify_score(n: i32) -> &'static str {
    if n >= 90 { "high" } else if n >= 60 { "medium" } else { "low" }
}
"#;

const GENUINE_TEST: &str = r#"
use mini::classify_score;

#[test]
fn classify_high() {
    assert_eq!(classify_score(95), "high");
}

#[test]
fn classify_medium() {
    assert_eq!(classify_score(70), "medium");
}

#[test]
fn classify_low() {
    assert_eq!(classify_score(10), "low");
}
"#;

const VACUOUS_LIB: &str = r#"
pub fn classify_score(n: i32) -> &'static str {
    if n >= 90 { "high" } else if n >= 60 { "medium" } else { "low" }
}
"#;

const VACUOUS_TEST: &str = r#"
// Vacuous test: this fixture does NOT reference any production symbol.
// The assertions hold for arbitrary input — reverting `classify_score`
// cannot break them because the tests never call it. This is the
// canonical "test pins a green bar without exercising prod" failure
// mode described in #387's body: a PR adds an always-green pin and the
// factory's reviewer can't tell.

#[test]
fn vacuous_constant_truth() {
    assert_eq!(2 + 2, 4);
}

#[test]
fn vacuous_string_literal() {
    let s = String::from("hello");
    assert_eq!(s.len(), 5);
}

#[test]
fn vacuous_range_check() {
    let x: i32 = 50;
    assert!(0 <= x && x <= 200);
}
"#;

#[test]
fn genuine_red_green_passes_check() {
    let proj = build_mini_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("Cargo.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");
    assert!(
        !report.vacuous,
        "expected genuine red-green test to NOT be flagged vacuous; report={report:?}"
    );
    assert_eq!(report.verdict, Verdict::Genuine);
    assert!(
        report.failed_on_revert >= 1,
        "expected at least one test to fail after revert; report={report:?}"
    );
    assert_eq!(report.manifest_path.as_deref(), Some(manifest.as_path()));
}

#[test]
fn vacuous_test_is_flagged() {
    let proj = build_mini_project(ProjectKind::VacuousAlwaysGreen);
    let _ = commit_current_tree(&proj, "feat: vacuous test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("Cargo.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");
    assert_eq!(report.verdict, Verdict::Vacuous);
    assert!(
        report.vacuous,
        "expected vacuous test to be flagged; report={report:?}"
    );
    assert_eq!(
        report.failed_on_revert, 0,
        "vacuous fixture must have 0 failing tests after revert; report={report:?}"
    );
}

#[test]
fn errors_propagate_for_unrunnable_cargo() {
    // An empty diff (no changed files) should produce a deterministic error
    // — the detector cannot prove anything about a PR with no tests.
    let proj = build_mini_project(ProjectKind::GenuineRedGreen);
    let empty: Vec<(PathBuf, FileClass)> = vec![];
    let err = check_red_green(&proj.root, &proj.base_sha, &empty).unwrap_err();
    assert!(matches!(err, RedGreenError::NoChangedTests), "got {err:?}");
}

#[test]
fn target_test_names_are_only_changed_tests() {
    // Bounds guarantee: only the tests added/changed by the PR are run.
    // This is the "runtime bounded (only affected tests run)" acceptance
    // criterion from #387. We verify by building a project where the
    // changed-test set is exactly {classify_high}, and inspecting the
    // report's `targeted_tests` field.
    let proj = build_mini_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("Cargo.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");
    let names: Vec<&str> = report.targeted_tests.iter().map(|s| s.as_str()).collect();
    assert!(
        names.iter().all(|n| ["classify_high", "classify_medium", "classify_low"].contains(n)),
        "unexpected targeted_tests: {names:?}"
    );
    // And nothing outside the PR's touched test file.
    assert!(
        names.iter().all(|n| !n.contains("other_test")),
        "must not run tests outside the PR diff: {names:?}"
    );
}

// ---- r5: ignore-skipped tests are recorded, not counted as vacuous-pass ----

/// Pure unit-style test exercising the fn-level `discover_test_fns_with_skip`
/// path (no cargo invocation) so the gate can rely on the fn-level scoping
/// contract without paying for a `cargo test` round-trip in CI.
#[test]
fn ignored_tests_are_recorded_with_skip_reason_not_silently_passed() {
    let src = r#"
#[test]
fn ordinary() { assert!(true); }

#[test]
#[ignore = "needs fixture repo"]
fn needs_network() { assert!(true); }

#[test]
#[ignore]
fn slow_path() { assert!(true); }
"#;
    let infos = daemon::vacuous_red_green::discover_test_fns_with_skip(src);
    let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"ordinary"), "ordinary must be discovered: {names:?}");
    assert!(names.contains(&"needs_network"), "needs_network must be discovered: {names:?}");
    assert!(names.contains(&"slow_path"), "slow_path must be discovered: {names:?}");

    let ordinary = infos.iter().find(|i| i.name == "ordinary").unwrap();
    assert!(ordinary.skip_reason.is_none(), "ordinary has no #[ignore]");

    let needs_network = infos.iter().find(|i| i.name == "needs_network").unwrap();
    assert_eq!(
        needs_network.skip_reason.as_deref(),
        Some("needs fixture repo"),
        "#[ignore = \"...\"] must populate skip_reason verbatim",
    );

    let slow_path = infos.iter().find(|i| i.name == "slow_path").unwrap();
    assert!(
        slow_path.skip_reason.is_some(),
        "bare #[ignore] must still record a skip_reason",
    );
}

/// Verdict precedence contract: when green-on-head fails, the verdict is
/// `GreenFailed` even if the revert run would have been red — the
/// green-on-head phase is the precondition that makes the revert signal
/// meaningful.
#[test]
fn verdict_precedence_green_failed_beats_genuine() {
    let outcome = daemon::vacuous_red_green::RunOutcome {
        green_on_head_ok: false,
        baseline_ok: true,
        failing_on_revert: vec!["a".to_string()],
    };
    assert_eq!(outcome.verdict(), Verdict::GreenFailed);
}

#[test]
fn verdict_precedence_baseline_failed_beats_vacuous() {
    let outcome = daemon::vacuous_red_green::RunOutcome {
        green_on_head_ok: true,
        baseline_ok: false,
        failing_on_revert: vec![],
    };
    assert_eq!(outcome.verdict(), Verdict::BaselineFailed);
}

/// Bead jleechan-sb4b: the `CargoNotFound` error variant must NOT be
/// classified as the misleading `GreenFailed` that the previous
/// `Git("spawn cargo test: No such file or directory")` produced. The
/// `Display` impl must name `cargo` so operators can immediately see
/// the toolchain is missing (not git).
#[test]
fn cargo_not_found_error_message_is_not_misleading_git_error() {
    use daemon::vacuous_red_green::RedGreenError;
    let e = RedGreenError::CargoNotFound("not on PATH".to_string());
    let msg = format!("{e}");
    assert!(
        msg.contains("cargo"),
        "CargoNotFound must name cargo (got: {msg:?})"
    );
    assert!(
        !msg.contains("git"),
        "CargoNotFound must NOT mention git — that's the misleading \
         message this bead replaces (got: {msg:?})"
    );
}
