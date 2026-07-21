// Issue #408 r3: red→green tests for the production-gate vacuous-test detector.
//
// Acceptance (per issue #408 body):
//   P1-1  three checks (a) green-on-head, (b) red-on-revert, (c) baseline-main
//         sanity; BaselineFailed is no longer dead code
//   P1-3  cargo test is invoked with --manifest-path <repo>/daemon/Cargo.toml
//         (NOT cwd-relative), so on dark-factory layout the detector does NOT
//         return false-pass via NEVER_RAN at ":396"
//   P1-4  #[ignore] tests are skipped (not counted as coverage); a test with
//         #[ignore] but no skip_reason is flagged separately
//   P1-5  scope is the ADDED or MODIFIED test fn within the file, not the
//         whole file's `#[test]` set — pre-existing fns are NOT re-run
//   P1-6  Config::vacuous_test_detection_enabled defaults to true; the
//         detector is wired through the gate path (NOT CLI-only) so beads
//         actually hit it
//   P1-7  NOT-ADDRESSED items from the reviewer are extracted as constraints
//         that flow into reroll.rs / constraints.rs

use std::path::{Path, PathBuf};
use std::process::Command;

use daemon::vacuous_red_green::{
    check_red_green, FileClass,
};

struct MiniProject {
    root: PathBuf,
    base_sha: String,
}

#[derive(Clone, Copy)]
enum ProjectKind {
    /// Test asserts a real property of production output. Reverting the
    /// production function to a stub makes the test fail.
    GenuineRedGreen,
    /// Test is `#[ignore]`-attributed without a skip_reason — must be flagged
    /// separately (P1-4). Coverage proof CANNOT count ignored tests.
    IgnoredNoReason,
}

fn build_mini_project(kind: ProjectKind) -> MiniProject {
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_r3_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = tmp.clone();
    std::fs::create_dir_all(&root).unwrap();

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
        .args(["config", "user.email", "test@example.com"]));
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

    let lib_src = match kind {
        ProjectKind::GenuineRedGreen | ProjectKind::IgnoredNoReason => GENUINE_LIB,
    };
    let test_src = match kind {
        ProjectKind::GenuineRedGreen => GENUINE_TEST,
        ProjectKind::IgnoredNoReason => IGNORED_TEST,
    };
    std::fs::write(&lib_path, lib_src).unwrap();

    let tests_dir = root.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let test_path = tests_dir.join("scenario.rs");
    std::fs::write(&test_path, test_src).unwrap();

    MiniProject { root, base_sha }
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

/// P1-4 fixture: a test that is `#[ignore]`-attributed without a skip_reason
/// must be flagged separately (not silently counted as coverage proof).
const IGNORED_TEST: &str = r#"
use mini::classify_score;

#[test]
fn classify_real() {
    assert_eq!(classify_score(95), "high");
}

#[test]
#[ignore]
fn ignored_no_reason() {
    assert_eq!(classify_score(10), "low");
}
"#;

// ---------------------------------------------------------------------------
// P1-1: three-check coverage on the report. The detector must surface
// `green_on_head`, `baseline_passed`, AND the existing `failed_on_revert` —
// BaselineFailed is no longer dead code.
// ---------------------------------------------------------------------------

#[test]
fn report_includes_three_check_fields() {
    let proj = build_mini_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);

    let manifest_path = proj.root.join("Cargo.toml");
    let report = check_red_green(&proj.root, &manifest_path, &proj.base_sha, &changed)
        .expect("report");
    // P1-1: green-on-head MUST be true (the targeted test passes on head).
    assert!(
        report.green_on_head,
        "green_on_head must be true for a real red-green PR; got {report:?}"
    );
    // P1-1: baseline-main sanity MUST be true (cargo test on the pristine
    // base tree compiles + runs every test green).
    assert!(
        report.baseline_passed,
        "baseline_passed must be true on a clean base; got {report:?}"
    );
    // P1-1: red-on-revert MUST still be the canonical "vacuous" signal.
    assert!(
        !report.vacuous,
        "genuine red-green must NOT be flagged vacuous; got {report:?}"
    );
    assert!(
        report.failed_on_revert >= 1,
        "expected at least one failing test on revert; got {report:?}"
    );
}

// ---------------------------------------------------------------------------
// P1-4: #[ignore] tests without a skip_reason are flagged separately, not
// silently counted as coverage proof.
// ---------------------------------------------------------------------------

#[test]
fn ignored_tests_are_listed_and_excluded_from_coverage() {
    let proj = build_mini_project(ProjectKind::IgnoredNoReason);
    let _ = commit_current_tree(&proj, "feat: ignored test");
    let changed = classify_changed_files(&proj);

    let manifest_path = proj.root.join("Cargo.toml");
    let report = check_red_green(&proj.root, &manifest_path, &proj.base_sha, &changed)
        .expect("report");
    assert!(
        report
            .ignored_tests
            .iter()
            .any(|t| t == "ignored_no_reason"),
        "ignored_no_reason must appear in ignored_tests; got {:?}",
        report.ignored_tests
    );
    assert!(
        report
            .ignored_without_skip_reason
            .iter()
            .any(|t| t == "ignored_no_reason"),
        "ignored_no_reason has no skip_reason and MUST be flagged; got {:?}",
        report.ignored_without_skip_reason
    );
    // The non-ignored test is the only coverage proof.
    assert!(
        report
            .targeted_tests
            .iter()
            .any(|t| t == "classify_real"),
        "classify_real must still be in targeted_tests; got {:?}",
        report.targeted_tests
    );
    assert!(
        !report
            .targeted_tests
            .iter()
            .any(|t| t == "ignored_no_reason"),
        "ignored tests must NOT count as coverage proof; got {:?}",
        report.targeted_tests
    );
}

// ---------------------------------------------------------------------------
// P1-5: scope to ADDED or MODIFIED test fns within a file. Pre-existing fns
// in the test file must NOT be in targeted_tests.
// ---------------------------------------------------------------------------

#[test]
fn targeted_tests_exclude_pre_existing_fns_in_changed_file() {
    // Build a base with a real test fn, then add ONE new fn. The new fn is
    // the only "added/modified" fn; the pre-existing one must NOT be in the
    // targeted_tests set.
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_r3_preex_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = tmp.clone();
    std::fs::create_dir_all(&root).unwrap();
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
    let pre_lib = r#"
pub fn classify_score(n: i32) -> &'static str {
    if n >= 90 { "high" } else if n >= 60 { "medium" } else { "low" }
}
"#;
    std::fs::write(&lib_path, pre_lib).unwrap();

    let tests_dir = root.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let test_path = tests_dir.join("scenario.rs");
    // Base test file already contains ONE pre-existing test fn.
    let pre_test = r#"
use mini::classify_score;

#[test]
fn pre_existing_test() {
    assert_eq!(classify_score(50), "low");
}
"#;
    std::fs::write(&test_path, pre_test).unwrap();

    run(Command::new("git").current_dir(&root).args(["init", "-q", "-b", "main"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "user.email", "test@example.com"]));
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

    // Now modify the test file: keep the pre-existing fn AND add a new one.
    let new_test = r#"
use mini::classify_score;

#[test]
fn pre_existing_test() {
    assert_eq!(classify_score(50), "low");
}

#[test]
fn brand_new_test() {
    assert_eq!(classify_score(95), "high");
}
"#;
    std::fs::write(&test_path, new_test).unwrap();
    run(Command::new("git")
        .current_dir(&root)
        .args(["add", "-A"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["commit", "-q", "-m", "feat: add test"]));

    let changed = vec![(test_path.clone(), FileClass::Test), (lib_path.clone(), FileClass::Production)];
    let manifest_path = root.join("Cargo.toml");
    let report = check_red_green(&root, &manifest_path, &base_sha, &changed).expect("report");
    assert!(
        report.targeted_tests.iter().any(|t| t == "brand_new_test"),
        "the added test fn must be in targeted_tests; got {:?}",
        report.targeted_tests
    );
    assert!(
        !report.targeted_tests.iter().any(|t| t == "pre_existing_test"),
        "pre-existing test fn must NOT be in targeted_tests (P1-5); got {:?}",
        report.targeted_tests
    );
}

// ---------------------------------------------------------------------------
// P1-3: --manifest-path is used so cargo can locate Cargo.toml. On
// dark-factory layout the daemon crate lives under daemon/Cargo.toml — the
// repo root has none. The CLI surfaces --manifest-path and check_red_green
// must accept it.
// ---------------------------------------------------------------------------

#[test]
fn check_red_green_accepts_explicit_manifest_path() {
    let proj = build_mini_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);

    // Pass the explicit manifest path even though the mini project also has
    // a root Cargo.toml — proves the API takes a manifest_path argument.
    let manifest_path = proj.root.join("Cargo.toml");
    let report = check_red_green(&proj.root, &manifest_path, &proj.base_sha, &changed)
        .expect("report with manifest path");
    assert!(report.green_on_head);
    assert!(!report.vacuous);
}

// ---------------------------------------------------------------------------
// P1-6: the vacuous_red_green gate is reachable from the gate path via
// VacuousRedGreenStatus on PrEvidence — beads do NOT have to invoke the CLI.
// ---------------------------------------------------------------------------

#[test]
fn pr_evidence_has_vacuous_red_green_status_default_not_run() {
    // Default-construct PrEvidence and confirm the new field defaults to
    // NotRun so the gate is wireable without breaking existing tests.
    use daemon::verifier::PrEvidence;
    let ev = PrEvidence::default();
    let status = ev.vacuous_red_green_status;
    assert!(
        matches!(status, daemon::verifier::VacuousRedGreenStatus::NotRun),
        "default VacuousRedGreenStatus must be NotRun; got {status:?}"
    );
}

#[allow(dead_code)]
fn _silence_path_unused(_: &Path) {}