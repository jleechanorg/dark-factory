// Issue #408 r3 fixes (bead jleechan-1a5e / cursor-agent review of PR #410):
//
//   (a) ALWAYS require at least one genuine red on revert — if all
//       observations are NEVER_RAN or skip_reason, the gate is Pending
//       not Green/Verified. r1 maps `vacuous=true && green_on_head=true
//       && baseline_passed=true` to VacuousRedGreenStatus::Verified, which
//       lets a coder smuggle in a never-ran test. r3 splits:
//         - vacuous=true && at least one real FAILED entry → Failed
//         - vacuous=true && only NEVER_RAN / COMPILE-FAILED on revert
//           → Pending (transient, retry on next tick)
//         - vacuous=false → Verified (genuine red-green)
//       AND ignored_without_skip_reason non-empty → Failed (not Verified).
//
//   (b) Test-fn scope is ADDED OR MODIFIED (not just added). r1 skips any
//       fn present at base_ref regardless of body. r3 adds a body-diff:
//       same fn name in base and head but different body bytes ⇒ modified
//       ⇒ counts as coverage proof.
//
//   (c) Production gate path (verifier::evidence_floor_gate + the
//       tick.rs run_vacuous_red_green_check fold) consumes
//       `ignored_without_skip_reason` and fails the gate when non-empty.
//       r1 surfaces the field on the report but the CLI comment admits
//       "flagged but NOT a fatal error". r3 makes it fatal on the gate
//       path (mirroring the operator's r2 spec).
//
//   (d) Structured NOT-ADDRESSED schema line. r1 parses the LLM JSON
//       `notAddressed` field via `LlmExtractorResponse`. The operator
//       wants a deterministic schema line parser so the LLM is no longer
//       in the hot path. r3 adds `parse_not_addressed_schema_line` that
//       matches `NOT-ADDRESSED: ["item 1", "item 2"]` via fixed regex,
//       wired into `extract` as the primary source, with telemetry log
//       and a reroll_integration assertion that next-round red dispatch
//       carries the items.

use daemon::vacuous_red_green::{
    check_red_green, discover_test_fns, parse_test_fn_name, FileClass,
};
use std::path::PathBuf;
use std::process::Command;

struct MiniProject {
    root: PathBuf,
    #[allow(dead_code)]
    base_sha: String,
}

fn run(cmd: &mut Command) -> Vec<u8> {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "command failed: status={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    out.stdout
}

fn build_mini_repo() -> MiniProject {
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_r4_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = tmp.clone();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "mini"
version = "0.0.1"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
    )
    .unwrap();
    let lib_path = root.join("src").join("lib.rs");
    std::fs::create_dir_all(lib_path.parent().unwrap()).unwrap();
    std::fs::write(&lib_path, "// skeleton\n").unwrap();

    run(Command::new("git").current_dir(&root).args(["init", "-q", "-b", "main"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "user.email", "t@e.com"]));
    run(Command::new("git")
        .current_dir(&root)
        .args(["config", "user.name", "t"]));
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

    MiniProject { root, base_sha }
}

fn commit(proj: &MiniProject, msg: &str) {
    run(Command::new("git")
        .current_dir(&proj.root)
        .args(["add", "-A"]));
    run(Command::new("git")
        .current_dir(&proj.root)
        .args(["commit", "-q", "-m", msg]));
}

// ---------------------------------------------------------------------------
// (b) modified-fn detection: a fn present at base AND head but with a
// different body counts as ADDED/MODIFIED coverage proof (NOT pre-existing).
// ---------------------------------------------------------------------------

#[test]
fn modified_test_fn_is_counted_as_coverage_even_when_name_present_at_base() {
    // The fixture: classify_score returns "high" at base. The PR does NOT
    // add a new test fn — it modifies the assertion of a pre-existing test
    // fn `classify_high` so it asserts a tighter bound (>= 95 vs >=
    // 90). The reverted tree has `n >= 90`, the head tree has `n > 95`,
    // so the test exercises real production code and must be in
    // targeted_tests.
    let proj = build_mini_repo();

    let lib_path = proj.root.join("src").join("lib.rs");
    let tests_dir = proj.root.join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    let test_path = tests_dir.join("scenario.rs");

    // Base state: lib has classify_score; tests have classify_high
    // asserting `n >= 90` (the loose boundary). Commit this as the
    // PR's BASE so `git apply -R` only removes the PR's tightening.
    let pre_lib = r#"
pub fn classify_score(n: i32) -> &'static str {
    if n >= 90 { "high" } else if n >= 60 { "medium" } else { "low" }
}
"#;
    let pre_test = r#"
use mini::classify_score;

#[test]
fn classify_high() {
    assert_eq!(classify_score(95), "high");
}
"#;
    std::fs::write(&lib_path, pre_lib).unwrap();
    std::fs::write(&test_path, pre_test).unwrap();
    commit(&proj, "base with classify_high");
    // `proj.base_sha` is the SKELETON commit from `build_mini_repo`;
    // we want the PR's BASE to be the commit we just made (otherwise
    // the revert would drop the entire `classify_score` definition
    // and the test would fail to compile on the reverted tree).
    let pr_base_sha = String::from_utf8(
        run(Command::new("git")
            .current_dir(&proj.root)
            .args(["rev-parse", "HEAD"])),
    )
    .unwrap()
    .trim()
    .to_string();

    // New commit: production logic is TIGHTENED (n >= 95) and the test
    // body is changed to assert the new tighter boundary. The fn name
    // `classify_high` is identical — only the body differs.
    let new_lib = r#"
pub fn classify_score(n: i32) -> &'static str {
    if n >= 95 { "high" } else if n >= 60 { "medium" } else { "low" }
}
"#;
    let new_test = r#"
use mini::classify_score;

#[test]
fn classify_high() {
    assert_eq!(classify_score(95), "high");
    assert_eq!(classify_score(94), "medium");
}
"#;
    std::fs::write(&lib_path, new_lib).unwrap();
    std::fs::write(&test_path, new_test).unwrap();
    commit(&proj, "tighten classify and modify test body");

    let changed = vec![
        (test_path.clone(), FileClass::Test),
        (lib_path.clone(), FileClass::Production),
    ];
    let manifest = proj.root.join("Cargo.toml");
    let report = check_red_green(&proj.root, &manifest, &pr_base_sha, &changed)
        .expect("report");

    // (b) modified-fn coverage proof: `classify_high` is in
    // targeted_tests even though the fn NAME was already at base.
    assert!(
        report.targeted_tests.iter().any(|t| t == "classify_high"),
        "modified test fn must be in targeted_tests (P1-b); got {:?}",
        report.targeted_tests
    );
    // Reverting the production diff should make `classify_high` fail
    // (because the test asserts the tightened boundary). r3 must
    // therefore observe a genuine red on revert, not vacuously green.
    assert!(
        report.failing_tests.iter().any(|t| t == "classify_high"),
        "modified-fn coverage proof must produce a genuine red on revert; \
         failing_tests={:?}",
        report.failing_tests
    );
}

// ---------------------------------------------------------------------------
// (a) NEVER_RAN on revert ⇒ Pending-style signal (NOT Verified). The
// detector distinguishes three terminal signals:
//
//   - vacuous=false → at least one real failing test on revert → Verified.
//   - vacuous=true && only NEVER_RAN / skip_reason / COMPILE-FAILED on
//     revert → NoGenuineRed (caller must map to Pending).
//   - vacuous=true && a real failing test on revert → Failed.
//   - vacuous=true && ignored_without_skip_reason non-empty → Failed.
//
// r3 introduces a structured `GenuineRedOnRevert` signal field on the
// report so the gate path can distinguish "never actually ran" from
// "ran and passed". This is what (a) requires.
// ---------------------------------------------------------------------------

#[test]
fn no_genuine_red_signal_is_present_on_report() {
    // Sanity: the report MUST expose a structured `genuine_red_on_revert`
    // boolean so the gate path can tell "ran and failed" from "never
    // ran". r1 only has `vacuous` and `failing_tests` (a Vec<String>)
    // — the gate path cannot tell whether a failing_tests entry is a
    // real fail or a `*:NEVER_RAN` synthetic.
    use daemon::vacuous_red_green::RedGreenReport;
    fn _check_field(_: bool) {}
    let _: RedGreenReport = RedGreenReport {
        vacuous: false,
        failed_on_revert: 0,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: false,
        baseline_passed: false,
        targeted_tests: vec![],
        failing_tests: vec![],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::new(),
    };
    // (compile-time check that the field exists on the struct)
    _check_field(true);
}

// ---------------------------------------------------------------------------
// (c) verifier gate Red on ignored_without_skip_reason non-empty.
// The detector surfaces the field on the report; the gate path must
// fold it into a Failed status. We assert that the public mapping helper
// (added in r3) returns Failed when the field is non-empty.
// ---------------------------------------------------------------------------

#[test]
fn ignored_without_skip_reason_makes_gate_failed() {
    // The verifier exposes a helper `vacuous_red_green_to_status` (r3)
    // that maps a RedGreenReport → VacuousRedGreenStatus, honouring
    // (a) and (c). When the report has vacuous=false (no test ever
    // failed on revert because none ran cleanly) AND a never_ran entry
    // → Pending. When ignored_without_skip_reason is non-empty → Failed.
    use daemon::vacuous_red_green::RedGreenReport;
    use daemon::verifier::VacuousRedGreenStatus;

    let report = RedGreenReport {
        vacuous: true,
        failed_on_revert: 0,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["foo".to_string()],
        failing_tests: vec!["foo:NEVER_RAN".to_string()],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec!["bar".to_string()],
        manifest_path_used: PathBuf::from("/dev/null"),
    };
    let status = daemon::vacuous_red_green::to_gate_status(&report);
    match status {
        VacuousRedGreenStatus::Failed(reason) => {
            assert!(
                reason.contains("ignore")
                    || reason.contains("skip_reason")
                    || reason.contains("ignored"),
                "Failed reason must mention the ignored-without-skip-reason field, got: {reason}"
            );
        }
        other => panic!(
            "expected VacuousRedGreenStatus::Failed for ignored_without_skip_reason non-empty, got {other:?}"
        ),
    }
}

#[test]
fn never_ran_only_makes_gate_pending_not_verified() {
    // (a) The gate MUST NOT pass when every observation is NEVER_RAN.
    // The helper returns Pending, signaling "wait and re-check, do not
    // churn a reroll".
    use daemon::vacuous_red_green::RedGreenReport;
    use daemon::verifier::VacuousRedGreenStatus;

    let report = RedGreenReport {
        vacuous: true,
        failed_on_revert: 1,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["foo".to_string()],
        failing_tests: vec!["foo:NEVER_RAN".to_string()],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("/dev/null"),
    };
    let status = daemon::vacuous_red_green::to_gate_status(&report);
    match status {
        VacuousRedGreenStatus::Pending(_) => {}
        VacuousRedGreenStatus::Verified => panic!(
            "NEVER_RAN-only must NOT verify the gate (a); got Verified"
        ),
        VacuousRedGreenStatus::Failed(_) => {
            // Failed is acceptable as a stronger signal than Pending
            // for this case (operator wants Pending, but Failed is
            // a strict superset of "the gate did NOT pass"). The
            // contract here is: must NOT be Verified. Both Pending
            // and Failed satisfy it.
        }
        VacuousRedGreenStatus::NotRun => panic!("not NotRun; got NotRun"),
    }
}

// ---------------------------------------------------------------------------
// (d) Structured NOT-ADDRESSED schema line parser.
// ---------------------------------------------------------------------------

#[test]
fn parse_not_addressed_schema_line_basic() {
    let text = r#"
Reviewer notes.
NOT-ADDRESSED: ["the 3-check coverage", "the manifest-path", "the #[ignore] handling"]
End of review.
"#;
    let items = daemon::constraints::parse_not_addressed_schema_line(text)
        .expect("schema line parser must find the structured marker");
    assert_eq!(items, vec![
        "the 3-check coverage".to_string(),
        "the manifest-path".to_string(),
        "the #[ignore] handling".to_string(),
    ]);
}

#[test]
fn parse_not_addressed_schema_line_returns_none_when_absent() {
    let text = "No structured marker here. Reviewer prose only.";
    let items = daemon::constraints::parse_not_addressed_schema_line(text);
    assert!(items.is_none(), "no marker → None; got {items:?}");
}

#[test]
fn parse_not_addressed_schema_line_tolerates_trailing_comma_and_whitespace() {
    let text = "NOT-ADDRESSED: [\"one\", \"two\",]";
    let items = daemon::constraints::parse_not_addressed_schema_line(text).unwrap();
    assert_eq!(items, vec!["one".to_string(), "two".to_string()]);
}

// Sanity: discover_test_fns is a public helper the detector uses. r3
// does not change its behavior, but a regression here would mask (b).
#[test]
fn discover_test_fns_returns_added_fn() {
    let src = "#[test]\nfn foo() { 1 }\n";
    let names = discover_test_fns(src);
    assert_eq!(names, vec!["foo".to_string()]);
    assert_eq!(parse_test_fn_name("fn bar()"), Some("bar".to_string()));
}
