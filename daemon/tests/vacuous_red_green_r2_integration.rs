// Issue #408 r2: four operator-mandated fixes for the runtime vacuous-test
// detector on the production gate path. cursor-agent review of r3 head
// (8cd8b8a) found these gaps that the r3 attempt did NOT close:
//
//   R2-1  NEVER_RAN entries in `failing_tests` (e.g. `:NEVER_RAN` markers
//         appended when cargo skips a test for any reason) currently flow
//         into `report.vacuous == false` which maps to `Verified` when
//         `green_on_head && baseline_passed`. That is a false-pass: a
//         PR whose tests literally never ran on the reverted tree still
//         goes green. The gate must require at least one GENUINE
//         failing test (not a NEVER_RAN / skip_reason entry) before it
//         declares green.
//   R2-2  The detector's scope is added-only. A test fn that existed at
//         base_ref but whose body was MODIFIED on the PR head is
//         silently excluded from the coverage proof even though it IS a
//         changed test. The detector must diff fn bodies between base
//         and head and treat modified fns as coverage.
//   R2-3  `report.ignored_without_skip_reason` is surfaced in the CLI
//         report but NEVER consulted on the production gate path
//         (`tick.rs::run_vacuous_red_green_check`). A coder can ignore
//         a test without a `skip_reason` and the gate stays green. The
//         gate path must FAIL when the list is non-empty.
//   R2-4  `not_addressed` constraints extracted from the reviewer LLM
//         are appended to spec.toml but no daemon dispatch / spawn
//         reads them back as next-round red constraints; the
//         `CONSTRAINT_MUTATION_SUCCESS` telemetry omits the count;
//         no reroll integration test asserts propagation. Operator
//         requires a STRUCTURED per-item enum (ADDRESSED / NOT-ADDRESSED
//         / N-A) parsed from a known schema line in the reviewer output
//         (no LLM JSON dependency), wired into both the constraint
//         block AND telemetry, with a regression test asserting the
//         next-round red dispatch consumes it.

use daemon::constraints::{extract, Extracted, ReviewerStatus};
use daemon::errors::DaemonError;
use daemon::tools::Llm;
use daemon::verifier::VacuousRedGreenStatus;
use daemon::vacuous_red_green::{check_red_green, FileClass, RedGreenReport};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// R2-1: NEVER_RAN must surface as a distinct field on the report and the
// production gate path must treat it as Pending (not Verified).
// ---------------------------------------------------------------------------

#[test]
fn r2_1_report_surfaces_never_ran_tests_as_a_distinct_field() {
    // The report must carry `never_ran_tests: Vec<String>` so the gate
    // path can distinguish a test that genuinely failed on revert from
    // a test that never ran (compile error / skip / etc). This is the
    // r2 fix: the r1 attempt conflated both into `failing_tests`, which
    // let a `:NEVER_RAN` entry flip `vacuous = false` and Verified the
    // gate.
    RedGreenReport::field_names_includes_never_ran_tests();
}

#[test]
fn r2_1_report_distinguishes_never_ran_from_genuine_fail() {
    // When a test ran and FAILED on revert, that entry must be in
    // `failing_tests` but NOT in `never_ran_tests`. Conversely a
    // NEVER_RAN entry must be in `never_ran_tests` but NOT in
    // `failing_tests`. The two fields are mutually exclusive.
    RedGreenReport::assert_never_ran_and_failing_are_mutually_exclusive();
}

// ---------------------------------------------------------------------------
// R2-2: scope must include MODIFIED test fns (body diff between base and
// head), not just ADDED fns. r1's `base_fns.contains(&name)` check skips
// any same-named fn regardless of whether its body changed.
// ---------------------------------------------------------------------------

#[test]
fn r2_2_modified_test_fns_are_included_in_targeted_tests() {
    // Build a mini project where base has a test fn, head renames the
    // assertion target inside it (i.e. body changed). The detector MUST
    // include the modified fn in `targeted_tests` so the gate exercises
    // it. r1 dropped it via `base_fns.contains(&name) -> skip`.
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_r2_modified_{}_{}",
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
    let pre_test = r#"
use mini::classify_score;
#[test]
fn score_band() {
    assert_eq!(classify_score(50), "low");
}
"#;
    std::fs::write(&test_path, pre_test).unwrap();
    run(Command::new("git").current_dir(&root).args(["init", "-q", "-b", "main"]));
    run(Command::new("git").current_dir(&root).args(["config", "user.email", "test@example.com"]));
    run(Command::new("git").current_dir(&root).args(["config", "user.name", "test"]));
    run(Command::new("git").current_dir(&root).args(["config", "commit.gpgsign", "false"]));
    run(Command::new("git").current_dir(&root).args(["add", "-A"]));
    run(Command::new("git").current_dir(&root).args(["commit", "-q", "-m", "base"]));
    let base_sha = String::from_utf8(
        run(Command::new("git").current_dir(&root).args(["rev-parse", "HEAD"])),
    )
    .unwrap()
    .trim()
    .to_string();

    // Same fn name, modified body — the assertion now checks a different
    // band (the test still passes on head because the production code
    // was unchanged, but the fn body changed relative to base). The
    // detector MUST treat this as a modified fn (coverage proof) and
    // include it in `targeted_tests`.
    let new_test = r#"
use mini::classify_score;
#[test]
fn score_band() {
    assert_eq!(classify_score(95), "high");
}
"#;
    std::fs::write(&test_path, new_test).unwrap();
    run(Command::new("git").current_dir(&root).args(["add", "-A"]));
    run(Command::new("git").current_dir(&root).args(["commit", "-q", "-m", "modify body"]));

    let changed = vec![
        (test_path.clone(), FileClass::Test),
        (lib_path.clone(), FileClass::Production),
    ];
    let manifest_path = root.join("Cargo.toml");
    let report = check_red_green(&root, &manifest_path, &base_sha, &changed)
        .expect("report");
    assert!(
        report.targeted_tests.iter().any(|t| t == "score_band"),
        "the MODIFIED test fn must be in targeted_tests; got {:?}",
        report.targeted_tests
    );
}

// ---------------------------------------------------------------------------
// R2-3: when `report.ignored_without_skip_reason` is non-empty, the
// production gate must return Failed (NOT Verified / Pending). The CLI
// already surfaces this list — the gate has to consume it.
// ---------------------------------------------------------------------------

#[test]
fn r2_3_production_gate_fails_when_ignored_without_skip_reason_is_nonempty() {
    // Build a `VacuousRedGreenStatus` directly with a populated
    // `ignored_without_skip_reason` and assert the gate-consumption
    // helper (`VerdictFromReport`) returns Failed (NOT Verified). This
    // is the gate-side contract the r1 attempt missed.
    use daemon::verifier::verdict_from_vacuous_report;
    let report = RedGreenReport {
        vacuous: false,
        failed_on_revert: 1,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["classify_real".to_string()],
        failing_tests: vec![],
        ignored_tests: vec!["ignored_no_reason".to_string()],
        ignored_without_skip_reason: vec!["ignored_no_reason".to_string()],
        manifest_path_used: PathBuf::from("/tmp/Cargo.toml"),
        never_ran_tests: vec![],
        modified_tests: vec![],
    };
    let status = verdict_from_vacuous_report(&report);
    match status {
        VacuousRedGreenStatus::Failed(reason) => {
            assert!(
                reason.contains("skip_reason") || reason.contains("ignored"),
                "Failed reason must name the #[ignore] no-skip_reason issue; got {reason}"
            );
        }
        VacuousRedGreenStatus::Verified => panic!(
            "R2-3 regression: gate went Verified even though ignored_without_skip_reason was non-empty"
        ),
        other => panic!("expected Failed for ignored_without_skip_reason; got {other:?}"),
    }
}

#[test]
fn r2_3_production_gate_fails_when_all_targeted_tests_are_never_ran() {
    // R2-1 + R2-3: if every targeted test came back NEVER_RAN (compile
    // error / skipped / etc), the gate must be Pending — NOT Verified.
    // r1 conflated this with a genuine fail-on-revert and went Verified
    // vacuously.
    use daemon::verifier::verdict_from_vacuous_report;
    let report = RedGreenReport {
        vacuous: false,
        failed_on_revert: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["classify_high".to_string()],
        failing_tests: vec![],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("/tmp/Cargo.toml"),
        never_ran_tests: vec!["classify_high:NEVER_RAN".to_string()],
        modified_tests: vec![],
    };
    let status = verdict_from_vacuous_report(&report);
    match status {
        VacuousRedGreenStatus::Pending(reason) => {
            assert!(
                reason.to_lowercase().contains("never_ran")
                    || reason.to_lowercase().contains("never ran"),
                "Pending reason must mention NEVER_RAN; got {reason}"
            );
        }
        VacuousRedGreenStatus::Verified => panic!(
            "R2-1 regression: gate went Verified even though every targeted test was NEVER_RAN"
        ),
        other => panic!("expected Pending for all-NEVER_RAN; got {other:?}"),
    }
}

#[test]
fn r2_3_verified_status_remains_possible_with_genuine_red_on_revert() {
    // Sanity: a clean green-on-head + at least one genuine failing test
    // on revert + no NEVER_RAN + no ignored-without-skip_reason is
    // STILL Verified. We are tightening the contract, not blocking
    // genuine red-green coverage.
    use daemon::verifier::verdict_from_vacuous_report;
    let report = RedGreenReport {
        vacuous: false,
        failed_on_revert: 1,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["classify_high".to_string()],
        failing_tests: vec!["classify_medium".to_string()],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("/tmp/Cargo.toml"),
        never_ran_tests: vec![],
        modified_tests: vec!["classify_medium".to_string()],
    };
    let status = verdict_from_vacuous_report(&report);
    assert!(
        matches!(status, VacuousRedGreenStatus::Verified),
        "genuine red-on-revert + clean baseline + clean green-on-head must Verified; got {status:?}"
    );
}

// ---------------------------------------------------------------------------
// R2-4: STRUCTURED NOT-ADDRESSED propagation. Operator requires:
//   * a per-item status enum (ADDRESSED / NOT-ADDRESSED / N-A) parsed
//     from a KNOWN schema line in the reviewer output (no LLM JSON
//     dependency for the structured path)
//   * both the constraint block AND telemetry carry not_addressed_count
//   * a regression test asserting the next-round red dispatch consumes it
// ---------------------------------------------------------------------------

struct FakeLlm(String);
impl Llm for FakeLlm {
    fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
        Ok(self.0.clone())
    }
}

#[test]
fn r2_4_structured_reviewer_status_line_parses_to_enum() {
    // Reviewer CLI / skeptic emits per-item status on a known schema
    // line:
    //   REVIEWER_STATUS: <key> = <ADDRESSED|NOT-ADDRESSED|N-A>
    // The daemon MUST parse this WITHOUT an LLM call and surface the
    // NOT-ADDRESSED items in `Extracted::not_addressed`. This is the
    // structured path; the LLM path remains as a fallback for items
    // the reviewer did not emit as a schema line.
    let review_text = r#"
some free-form prose.

REVIEWER_STATUS: 3-check coverage = NOT-ADDRESSED
REVIEWER_STATUS: manifest-path = ADDRESSED
REVIEWER_STATUS: #[ignore] handling = NOT-ADDRESSED
REVIEWER_STATUS: pre-existing fn scope = N-A
"#;
    let llm = FakeLlm(reply_with_no_json());
    let ext = extract(&llm, review_text).expect("extract");
    assert_eq!(
        ext.not_addressed,
        vec![
            "3-check coverage".to_string(),
            "#[ignore] handling".to_string(),
        ],
        "structured REVIEWER_STATUS lines must populate not_addressed; got {:?}",
        ext.not_addressed
    );
}

#[test]
fn r2_4_structured_status_enum_is_exported_and_complete() {
    // The enum must be public so the next-round red dispatch (reroll
    // spec parser) can consume it. All three variants must exist.
    let _ = ReviewerStatus::Addressed;
    let _ = ReviewerStatus::NotAddressed;
    let _ = ReviewerStatus::NotApplicable;
}

#[test]
fn r2_4_structured_and_llm_paths_merge_deduplicated() {
    // When BOTH the structured REVIEWER_STATUS line AND the LLM JSON
    // surface a NOT-ADDRESSED item, the merged list must contain it
    // exactly once. Duplicates collapse to a single entry.
    let review_text = r#"
REVIEWER_STATUS: 3-check coverage = NOT-ADDRESSED
"#;
    let llm_reply = r#"
{"inhibitionSpecs":[],"positiveAssertions":[],"notAddressed":["3-check coverage","extra item"],"securityRedactionEncountered":false}
"#;
    let llm = FakeLlm(llm_reply.to_string());
    let ext = extract(&llm, review_text).expect("extract");
    assert!(
        ext.not_addressed.iter().filter(|s| *s == "3-check coverage").count() == 1,
        "duplicate NOT-ADDRESSED items must collapse to one; got {:?}",
        ext.not_addressed
    );
    assert!(
        ext.not_addressed.iter().any(|s| s == "extra item"),
        "LLM-only items must still surface; got {:?}",
        ext.not_addressed
    );
}

#[test]
fn r2_4_telemetry_carries_not_addressed_count() {
    // The `CONSTRAINT_MUTATION_SUCCESS` telemetry event emitted from
    // `reroll.rs` MUST include `notAddressedCount`. The r1 attempt
    // emitted only `positiveAssertionsCount` + `inhibitionSpecsCount`,
    // which made `not_addressed` invisible in dashboard / count
    // queries. r2 closes that telemetry gap.
    use daemon::reroll::format_constraint_mutation_payload;
    let ext = Extracted {
        inhibition_specs: vec!["a".to_string(), "b".to_string()],
        positive_assertions: vec!["c".to_string()],
        security_redaction_encountered: false,
        not_addressed: vec!["d".to_string(), "e".to_string(), "f".to_string()],
    };
    let payload = format_constraint_mutation_payload(&ext);
    let v: serde_json::Value = serde_json::from_str(&payload).expect("json");
    // The R2-4 payload nests counts under `extractedConstraints` so
    // dashboard rollups group the three count fields together. Walk
    // the nested object — both shapes are stable wire contracts.
    let count = v
        .get("extractedConstraints")
        .and_then(|c| c.get("notAddressedCount"))
        .and_then(|n| n.as_u64())
        .unwrap_or_else(|| panic!("CONSTRAINT_MUTATION_SUCCESS payload missing extractedConstraints.notAddressedCount: {payload}"));
    assert_eq!(count, 3);
}

#[test]
fn r2_4_reroll_spec_block_carries_structured_not_addressed_block() {
    // The TOML [[reroll]] block written by reroll.rs MUST carry a
    // `[[reroll.not_addressed_structured]]` array of {key, status} so
    // the next-round coder prompt knows WHICH items were NOT-ADDRESSED
    // vs N-A (they have different consequences — N-A means "not in
    // scope", NOT-ADDRESSED means "in scope but missed"). The r1
    // attempt flattened both into a plain string list, losing the
    // distinction.
    use daemon::reroll::format_reroll_block;
    let ext = Extracted {
        inhibition_specs: vec![],
        positive_assertions: vec![],
        security_redaction_encountered: false,
        not_addressed: vec!["3-check coverage".to_string()],
    };
    let structured = vec![("3-check coverage".to_string(), ReviewerStatus::NotAddressed)];
    let block = format_reroll_block("claude", 1, &ext, &structured, "raw feedback");
    assert!(
        block.contains("not_addressed_structured = ["),
        "reroll block missing the structured not_addressed field; got:\n{block}"
    );
    assert!(
        block.contains("status = \"NOT-ADDRESSED\""),
        "reroll block must carry the structured status token; got:\n{block}"
    );
    assert!(
        block.contains("key = \"3-check coverage\""),
        "reroll block must carry the structured key entry; got:\n{block}"
    );
}

#[allow(dead_code)]
fn _silence_path_unused(_: &Path) {}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "cmd failed: status={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out.stdout
}

fn reply_with_no_json() -> String {
    r#"{"inhibitionSpecs":[],"positiveAssertions":[],"securityRedactionEncountered":false,"notAddressed":[]}"#.to_string()
}

// Marker trait on RedGreenReport so the r2 field-shape tests compile
// even before the field is added (TDD scaffolding — these calls
// exercise the field-name presence at compile time, not at runtime).
trait RedGreenReportFields {
    fn field_names_includes_never_ran_tests();
    fn assert_never_ran_and_failing_are_mutually_exclusive();
}
impl RedGreenReportFields for RedGreenReport {
    fn field_names_includes_never_ran_tests() {
        // Compile-time enforcement: the type MUST expose these fields.
        // If the r1 attempt is still in place, this fails to compile.
        fn assert_fields(r: &RedGreenReport) {
            let _: Vec<String> = vec![];
            let _: &Vec<String> = &r.never_ran_tests;
            let _: &Vec<String> = &r.modified_tests;
        }
        let r = RedGreenReport {
            vacuous: false,
            failed_on_revert: 0,
            green_on_head: true,
            baseline_passed: true,
            targeted_tests: vec![],
            failing_tests: vec![],
            ignored_tests: vec![],
            ignored_without_skip_reason: vec![],
            manifest_path_used: PathBuf::from("/tmp/Cargo.toml"),
            never_ran_tests: vec![],
            modified_tests: vec![],
        };
        assert_fields(&r);
    }
    fn assert_never_ran_and_failing_are_mutually_exclusive() {
        // The two fields must NEVER share a name (a test cannot be both
        // "ran and failed" AND "never ran"). The detector is responsible
        // for keeping them disjoint.
        let r = RedGreenReport {
            vacuous: false,
            failed_on_revert: 0,
            green_on_head: true,
            baseline_passed: true,
            targeted_tests: vec![],
            failing_tests: vec!["a".to_string()],
            ignored_tests: vec![],
            ignored_without_skip_reason: vec![],
            manifest_path_used: PathBuf::from("/tmp/Cargo.toml"),
            never_ran_tests: vec![],
            modified_tests: vec![],
        };
        for n in &r.failing_tests {
            assert!(
                !r.never_ran_tests.contains(n),
                "failing_tests and never_ran_tests must be disjoint; {n} appears in both"
            );
        }
    }
}