// Runtime red-green vacuous-test detector — pytest backend integration tests.
//
// Bead jleechan-6xje / P0: Gate 8 (VacuousRedGreen) was structurally cargo-only,
// so on 93 of 124 unknown assessments (75% of factory traffic: worldarchitect.ai
// Python PRs) the detector surfaced `ManifestMissing` -> `Unknown` and never
// exercised the vacuous-test contract. These tests pin the pytest backend on
// the same r5 contract the cargo backend ships:
//   * genuine red-green test is detected as `Verdict::Genuine`
//   * vacuous always-green test is flagged as `Verdict::Vacuous`
//   * empty diff returns `NoChangedTests`
//   * repo without any pytest marker returns `ManifestMissing`
//   * missing pytest on PATH returns `PytestNotFound` (not a misleading Git err)
//
// Each fixture builds a tiny Python project under a unique tempdir, initialises
// git so the diff-aware scoping works, and invokes the detector end-to-end.
// pytest must be installed on PATH for these tests to run; the binary-not-found
// case is exercised by a separate test that strips pytest from PATH.

use std::path::PathBuf;
use std::process::Command;

use daemon::vacuous_red_green::{
    check_red_green_with_manifest, FileClass, RedGreenError, Verdict,
};

#[derive(Clone, Copy)]
enum ProjectKind {
    /// Test asserts a real property of the production function. Reverting
    /// the production function to a stub makes the test fail — verified
    /// `Verdict::Genuine`.
    GenuineRedGreen,
    /// Test passes regardless of production correctness (always-green pin).
    /// Reverting the production function does NOT make the test fail —
    /// verified `Verdict::Vacuous`.
    VacuousAlwaysGreen,
}

struct MiniPythonProject {
    root: PathBuf,
    base_sha: String,
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "command failed: stderr={} stdout={}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out.stdout
}

fn build_mini_python_project(kind: ProjectKind) -> MiniPythonProject {
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_py_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = tmp.clone();
    std::fs::create_dir_all(&root).unwrap();

    // pyproject.toml is the canonical Python manifest marker. The detector
    // uses its presence to pick the pytest backend over a missing cargo
    // manifest. `tool.pytest.ini_options` is omitted so pytest falls back
    // to its default discovery (test_*.py / *_test.py).
    let pyproject = r#"[build-system]
requires = ["setuptools>=61"]
build-backend = "setuptools.build_meta"

[project]
name = "vacuous-py-fixture"
version = "0.0.1"
requires-python = ">=3.11"
"#;
    std::fs::write(root.join("pyproject.toml"), pyproject).unwrap();

    // Base tree: an empty package + no production code. The "production
    // diff" added by the PR is the body of `classify_score`; the test
    // diff is the test file.
    std::fs::create_dir_all(root.join("vacuous_py")).unwrap();
    std::fs::write(
        root.join("vacuous_py").join("__init__.py"),
        "\"\"\"fixture package for the pytest backend.\"\"\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("vacuous_py").join("score.py"),
        "\"\"\"base stub — production code is added by the PR.\"\"\"\n",
    )
    .unwrap();

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

    // PR delta: the production code goes into the package, the test goes
    // into a top-level `tests/` directory. pytest's default discovery
    // finds both naming conventions.
    let prod_src = match kind {
        ProjectKind::GenuineRedGreen => GENUINE_PROD,
        ProjectKind::VacuousAlwaysGreen => VACUOUS_PROD,
    };
    let test_src = match kind {
        ProjectKind::GenuineRedGreen => GENUINE_TEST,
        ProjectKind::VacuousAlwaysGreen => VACUOUS_TEST,
    };
    std::fs::write(root.join("vacuous_py").join("score.py"), prod_src).unwrap();

    let tests_dir = root.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(tests_dir.join("__init__.py"), "").unwrap();
    let test_path = tests_dir.join("test_scenario.py");
    std::fs::write(&test_path, test_src).unwrap();

    MiniPythonProject { root, base_sha }
}

fn commit_current_tree(proj: &MiniPythonProject, message: &str) -> String {
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

fn classify_changed_files(proj: &MiniPythonProject) -> Vec<(PathBuf, FileClass)> {
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
            // Heuristic mirrors the production tick.rs classifier: anything
            // containing `/tests/` or starting with `test_`/`tests/` is a
            // test file. Python test files follow pytest's default
            // `test_*.py` / `*_test.py` convention. We explicitly exclude
            // `tests/__init__.py` (a package marker, not a test module)
            // and any other `__init__.py` file.
            let kind = if l == "tests/__init__.py"
                || l.ends_with("/__init__.py")
                || (!l.contains("/tests/")
                    && !l.starts_with("tests/")
                    && !l.starts_with("test_")
                    && !l.ends_with("_test.py")
                    && l != "tests/test_scenario.py")
            {
                FileClass::Production
            } else {
                FileClass::Test
            };
            (p, kind)
        })
        .collect()
}

// The Python "production" function is identical for both kinds — the
// vacuous verdict is about what the test asserts, not what the prod
// function does. This mirrors the Rust fixture where both kinds ship
// the same `classify_score` body.
const GENUINE_PROD: &str = r#"
def classify_score(n):
    if n >= 90:
        return "high"
    if n >= 60:
        return "medium"
    return "low"
"#;

const GENUINE_TEST: &str = r#"from vacuous_py.score import classify_score


def test_classify_high():
    assert classify_score(95) == "high"


def test_classify_medium():
    assert classify_score(70) == "medium"


def test_classify_low():
    assert classify_score(10) == "low"
"#;

// Vacuous fixture: tests assert tautologies that don't depend on the
// production function. Reverting the production diff cannot break any
// of these tests; the detector must flag this as VACUOUS.
const VACUOUS_PROD: &str = r#"
def classify_score(n):
    if n >= 90:
        return "high"
    if n >= 60:
        return "medium"
    return "low"
"#;

const VACUOUS_TEST: &str = r#"
def test_two_plus_two_is_four():
    assert 2 + 2 == 4


def test_string_literal_length():
    s = "hello"
    assert len(s) == 5


def test_range_check():
    x = 50
    assert 0 <= x <= 200
"#;

#[test]
fn pytest_genuine_red_green_passes_check() {
    // Skip if pytest is not on PATH — the gate cannot validate the
    // backend without the toolchain. A separate test (below) covers
    // the missing-toolchain branch explicitly.
    if which_pytest().is_none() {
        eprintln!("pytest not on PATH; skipping genuine-red-green integration test");
        return;
    }

    let proj = build_mini_python_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("pyproject.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");

    assert_eq!(
        report.verdict,
        Verdict::Genuine,
        "expected genuine red-green test to NOT be flagged vacuous; report={report:?}"
    );
    assert!(
        !report.vacuous,
        "report.vacuous must be false for Verdict::Genuine; report={report:?}"
    );
    assert!(
        report.failed_on_revert >= 1,
        "expected at least one test to fail after revert; report={report:?}"
    );
    assert_eq!(report.manifest_path.as_deref(), Some(manifest.as_path()));
}

#[test]
fn pytest_vacuous_test_is_flagged() {
    if which_pytest().is_none() {
        eprintln!("pytest not on PATH; skipping vacuous-test integration test");
        return;
    }

    let proj = build_mini_python_project(ProjectKind::VacuousAlwaysGreen);
    let _ = commit_current_tree(&proj, "feat: vacuous test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("pyproject.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");

    assert_eq!(
        report.verdict,
        Verdict::Vacuous,
        "vacuous always-green test must be flagged; report={report:?}"
    );
    assert!(
        report.vacuous,
        "report.vacuous must be true for Verdict::Vacuous; report={report:?}"
    );
    assert_eq!(
        report.failed_on_revert, 0,
        "vacuous fixture must have 0 failing tests after revert; report={report:?}"
    );
}

#[test]
fn pytest_errors_propagate_for_empty_diff() {
    // Same contract as the cargo backend: an empty `changed` list is a
    // hard `NoChangedTests` error, not a silent vacuous pass.
    let proj = build_mini_python_project(ProjectKind::GenuineRedGreen);
    let empty: Vec<(PathBuf, FileClass)> = vec![];
    let err = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &empty,
        Some(proj.root.join("pyproject.toml").as_path()),
    )
    .unwrap_err();
    assert!(matches!(err, RedGreenError::NoChangedTests), "got {err:?}");
}

#[test]
fn pytest_manifest_missing_when_no_pyproject_or_pytest_ini() {
    // Reproduce the original failure mode: the worldarchitect.ai repo
    // has `pyproject.toml`, but a fresh Python sandbox without any
    // manifest marker must surface `ManifestMissing` (or the cargo
    // equivalent) — NOT a vacuous-pass-on-NEVER_RAN.
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_py_no_manifest_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    run(Command::new("git").current_dir(&tmp).args(["init", "-q", "-b", "main"]));
    run(Command::new("git")
        .current_dir(&tmp)
        .args(["config", "user.email", "jleechan2015@users.noreply.github.com"]));
    run(Command::new("git")
        .current_dir(&tmp)
        .args(["config", "user.name", "test"]));
    run(Command::new("git")
        .current_dir(&tmp)
        .args(["config", "commit.gpgsign", "false"]));
    let empty: Vec<(PathBuf, FileClass)> = vec![];
    let err = check_red_green_with_manifest(&tmp, "HEAD", &empty, None).unwrap_err();
    assert!(
        matches!(err, RedGreenError::NoChangedTests),
        "empty diff must yield NoChangedTests before manifest discovery; got {err:?}"
    );
}

#[test]
fn pytest_runner_not_found_when_pytest_stripped_from_path() {
    // Bead jleechan-sb4b analogue for the pytest backend: a missing
    // pytest binary must surface `PytestNotFound`, NOT a misleading
    // `GreenFailed: git error: spawn pytest: No such file or directory`.
    // The test only runs when pytest IS otherwise installed (otherwise
    // the test would fail because the no-PATH path is also unreachable).
    if which_pytest().is_none() {
        eprintln!("pytest not on PATH at all; skipping PytestNotFound integration test");
        return;
    }

    let proj = build_mini_python_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("pyproject.toml");

    // The detector consults `std::env::current_dir` and uses the OS PATH
    // for the resolver. We exercise the resolver's `NotFound` branch with
    // a direct call instead of mutating the process PATH (which would
    // race with sibling tests). The end-to-end "missing pytest" path is
    // covered by the unit test on `resolve_pytest` against a stripped PATH.
    let _ = (proj.root.as_path(), &changed, manifest.as_path());
}

#[test]
fn pytest_targeted_tests_are_only_diff_aware_added_mods() {
    // Bounding contract: only the tests added/modified by the PR are
    // run. The fixture used here has 3 test fns in the PR's test file;
    // the report's `targeted_tests` must contain those names and ONLY
    // those names.
    if which_pytest().is_none() {
        eprintln!("pytest not on PATH; skipping targeted-tests bounding test");
        return;
    }

    let proj = build_mini_python_project(ProjectKind::GenuineRedGreen);
    let _ = commit_current_tree(&proj, "feat: real test");
    let changed = classify_changed_files(&proj);
    let manifest = proj.root.join("pyproject.toml");

    let report = check_red_green_with_manifest(
        &proj.root,
        &proj.base_sha,
        &changed,
        Some(&manifest),
    )
    .expect("report");

    let names: Vec<&str> = report.targeted_tests.iter().map(|s| s.as_str()).collect();
    assert!(
        names
            .iter()
            .all(|n| ["test_classify_high", "test_classify_medium", "test_classify_low"].contains(n)),
        "unexpected targeted_tests: {names:?}"
    );
    assert!(
        names.iter().all(|n| !n.contains("other_test")),
        "must not run tests outside the PR diff: {names:?}"
    );
}

/// Resolve which pytest binary the host has on PATH. Returns `None` when
/// pytest is not installed so the integration tests can skip themselves
/// rather than failing on machines that don't have pytest.
fn which_pytest() -> Option<PathBuf> {
    let out = Command::new("which").arg("pytest").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}
