// Integration tests for the vacuous-test detector (PR #387 / bead jleechan-ijod).
// The detector MUST:
//   1. flag every fixture under fixtures/vacuous_test_detector/vacuous_examples/
//   2. emit zero findings for fixtures/vacuous_test_detector/clean_examples/
//   3. expose a stable public API so callers can also drive it directly.
//
// Each test below is itself NON-vacuous: removing the detector (forcing every
// scan_* call to return an empty report) makes the vacuous-expectation tests
// fail; flipping the detector to always-flag makes the clean-expectation tests
// fail. The TDD red->green proof lives in the pr-body evidence bundle.

use daemon::vacuous::{
    scan_test_directory, scan_test_file, scan_test_source, VacuousFinding, VacuousKind,
};

fn fixtures_root() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/vacuous_test_detector");
    p
}

fn kinds(f: &VacuousFinding) -> String {
    format!("{:?}", f.kind)
}

#[test]
fn flags_vacuous_trivial_assert_example() {
    let mut p = fixtures_root();
    p.push("vacuous_examples/trivial_assert.rs");
    let report = scan_test_file(&p).expect("scan should succeed on fixture");
    let kinds: Vec<String> = report.findings.iter().map(kinds).collect();
    assert!(
        kinds.iter().any(|k| k == "TrivialAssert"),
        "expected a TrivialAssert finding in trivial_assert.rs, got: {kinds:?}"
    );
    let has_trivial_assert_text = report
        .findings
        .iter()
        .any(|f| f.snippet.contains("assert!(true"));
    assert!(
        has_trivial_assert_text,
        "TrivialAssert finding must reference the assert!(true) line text"
    );
}

#[test]
fn flags_vacuous_fixture_only_example() {
    let mut p = fixtures_root();
    p.push("vacuous_examples/fixture_only.rs");
    let report = scan_test_file(&p).expect("scan ok");
    let kinds: Vec<String> = report.findings.iter().map(kinds).collect();
    // The fixture exercises a production function but asserts on values
    // that echo the test's own literal input (`p.seq, 1`, `p.payload, ...`).
    // Both `FixtureOnlyAssert` and `ProductionOutputEchoesInput` are valid
    // classifications; we require that the file produce ANY vacuous finding
    // (the exact kind reflects which rule fires first), not a specific one.
    assert!(
        !kinds.is_empty(),
        "expected any vacuous finding in fixture_only.rs, got: {kinds:?}"
    );
}

#[test]
fn flags_vacuous_no_production_symbol_example() {
    let mut p = fixtures_root();
    p.push("vacuous_examples/no_production_symbol.rs");
    let report = scan_test_file(&p).expect("scan ok");
    let kinds: Vec<String> = report.findings.iter().map(kinds).collect();
    // The fixture may surface as NoProductionSymbolUse (when the detector
    // reports the lack of production use) or as another vacuous kind if the
    // rule cascade resolves differently across versions. The contract is
    // "this fixture MUST surface a vacuous finding".
    assert!(
        !kinds.is_empty(),
        "expected any vacuous finding in no_production_symbol.rs, got: {kinds:?}"
    );
}

#[test]
fn flags_vacuous_symmetric_tautology_example() {
    let mut p = fixtures_root();
    p.push("vacuous_examples/symmetric_tautology.rs");
    let report = scan_test_file(&p).expect("scan ok");
    let kinds: Vec<String> = report.findings.iter().map(kinds).collect();
    // The fixture deliberately exercises an idempotent-input shape
    // (`let out = f(&raw); assert_eq!(out, raw)`); it MUST surface a vacuous
    // finding. Both `SymmetricTautology` (shape 2) and the production-output
    // echo fall under the same umbrella rule.
    assert!(
        !kinds.is_empty(),
        "expected any vacuous finding in symmetric_tautology.rs, got: {kinds:?}"
    );
}

#[test]
fn does_not_flag_clean_real_production_failure_example() {
    let mut p = fixtures_root();
    p.push("clean_examples/real_production_failure.rs");
    let report = scan_test_file(&p).expect("scan ok");
    assert!(
        report.findings.is_empty(),
        "real-production-failure example has NO vacuous patterns — detector must stay silent, got: {:?}",
        report.findings
    );
}

#[test]
fn does_not_flag_clean_error_path_enforced_example() {
    let mut p = fixtures_root();
    p.push("clean_examples/error_path_enforced.rs");
    let report = scan_test_file(&p).expect("scan ok");
    assert!(
        report.findings.is_empty(),
        "error-path-enforced example has NO vacuous patterns — detector must stay silent, got: {:?}",
        report.findings
    );
}

#[test]
fn directory_scan_reports_both_classes_correctly() {
    // One pass over the whole fixtures tree must surface the four vacuous
    // files and stay silent on the two clean ones. The count of unique
    // vacuous-example files in findings must equal the number of vacuous
    // example files on disk (one-or-more findings per file is allowed —
    // multiple findings on the same file count once for membership).
    let dir = fixtures_root();
    let report = scan_test_directory(&dir).expect("dir scan ok");
    let vacuous_files: std::collections::BTreeSet<_> = report
        .findings
        .iter()
        .map(|f| f.file.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let must_have = [
        "trivial_assert.rs",
        "fixture_only.rs",
        "no_production_symbol.rs",
        "symmetric_tautology.rs",
    ];
    for name in must_have {
        assert!(
            vacuous_files.contains(name),
            "directory scan missed vacuous example {name}, got: {vacuous_files:?}"
        );
    }
    let must_not_have = ["real_production_failure.rs", "error_path_enforced.rs"];
    for name in must_not_have {
        assert!(
            !vacuous_files.contains(name),
            "directory scan incorrectly flagged clean example {name}"
        );
    }
    assert!(
        report.files_scanned >= 6,
        "scanned file count too low: {}",
        report.files_scanned
    );
}

#[test]
fn in_source_string_form_also_flags_vacuous_patterns() {
    // The string-form API is what the CLI / shell pipeline uses when it has
    // already read the file (or for the synthesized test from a git diff
    // hunk). It must produce the same classification as the file-form.
    let vacuous = r#"
        #[test]
        fn vacuous_inline() {
            let s = String::from("foo");
            assert!(true);
        }
    "#;
    let report = scan_test_source(vacuous);
    // The contract is "the vacuous pattern is flagged", not "exactly one
    // finding". `String::from(...)` is std so the body has no production
    // symbols referenced; the body may simultaneously trip TrivialAssert
    // (the `assert!(true)`) AND the no-production-symbol rule. We accept
    // either ordering, but require TrivialAssert to be one of the findings.
    assert!(
        !report.findings.is_empty(),
        "in-source vacuous example produced no findings"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.kind == VacuousKind::TrivialAssert),
        "expected a TrivialAssert finding in vacuous inline example: {:?}",
        report.findings
    );
}

#[test]
fn in_source_string_form_stays_silent_on_clean() {
    let clean = r#"
        #[derive(PartialEq, Debug)]
        struct S { v: i32 }

        fn add_production(x: i32) -> i32 { x + 1 }

        #[test]
        fn add_one_real() {
            let s = S { v: add_production(2) };
            assert_eq!(s, S { v: 3 });
        }
    "#;
    let report = scan_test_source(clean);
    assert!(
        report.findings.is_empty(),
        "clean in-source example must produce zero findings, got: {:?}",
        report.findings
    );
}

#[test]
fn shell_wrapper_produces_nonzero_exit_when_vacuous_fixtures_present() {
    // Smoke-test the operator-facing wrapper end-to-end. This will fail if
    // someone reverts the CLI binary's exit-code handling or stubs `vacuous::scan_*`
    // to always-return-empty without updating this test.
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = project_root.parent().unwrap().to_path_buf();
    let wrapper = project_root.join("daemon/scripts/vacuous-test-detector.sh");
    assert!(
        wrapper.is_file(),
        "expected wrapper at {}",
        wrapper.display()
    );
    let output = std::process::Command::new("bash")
        .arg(&wrapper)
        .arg("--quiet")
        .current_dir(&project_root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                ".cargo/bin",
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .expect("wrapper invocation must succeed (build only)");
    assert_eq!(
        output.status.code(),
        Some(1),
        "shell wrapper against vacuous fixtures must exit 1, got {:?}\nstdout={:?}\nstderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn shell_wrapper_produces_zero_exit_for_clean_only_paths() {
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_root = project_root.parent().unwrap().to_path_buf();
    let wrapper = project_root.join("daemon/scripts/vacuous-test-detector.sh");
    let output = std::process::Command::new("bash")
        .arg(&wrapper)
        .arg("--quiet")
        .arg("--files")
        .arg("daemon/tests/fixtures/vacuous_test_detector/clean_examples")
        .current_dir(&project_root)
        .output()
        .expect("wrapper invocation must succeed (build only)");
    assert_eq!(
        output.status.code(),
        Some(0),
        "shell wrapper against clean fixtures must exit 0, got {:?}\nstderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
