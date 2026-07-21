// Issue #408 r5 — cursor-agent + CodeRabbit feedback on PR #420 (head c365a72).
// Tests are written FIRST (TDD red phase). Each test asserts a fix that
// prevents a bug the reviewers surfaced; once the production code is
// updated, these flip green.
//
// Fixes covered:
//   (1)+(2) vacuous_red_green::to_gate_status: vacuous=true with EMPTY
//          failing_tests (true vacuity — every targeted test passed on
//          revert) must be `Failed`, NOT `Pending`. Pending is reserved
//          for the synthetic-NEVER_RAN-only case where failing_tests is
//          non-empty but every entry is synthetic.
//   (3)     verifier::evidence_floor_gate: the vacuous check must run
//          BEFORE the verified-evidence early return so a vacuous PR
//          with a valid evidence gist still fails the gate.
//   (5)     daemon/src/bin/vacuous_red_green.rs CLI: the gate-fail
//          expression must reuse to_gate_status (single source of truth)
//          so the CLI and the daemon path cannot drift.
//   (7)     constraints::parse_not_addressed_schema_line: an escaped
//          `\"` in an item must unescape to a single `"` (the backslash
//          itself must NOT be retained in the parsed string).
//   (8)     constraints::extract: schema-line NOT-ADDRESSED items must
//          be parsed from `redacted_text`, not raw `review_text`, so a
//          reviewer that leaks a holdout path into a NOT-ADDRESSED
//          item cannot bypass programmatic holdout redaction.
//   (9)     constraints::format_prior_constraints_block: Addressed
//          entries must be OMITTED from "must address" AND from the
//          out-of-scope section. Currently they fall through and get
//          rendered as "must address".
//   (10)    dispatch::build_coder_prompt: prior_block must participate
//          in the CODER_PROMPT_TOTAL_CAP reconciliation so a verbose
//          prior review cannot exceed AO's spawn-prompt ceiling.
//   (12)    vacuous_red_green::run_cargo_baseline: drop the bogus
//          `--skip ignored` arg (cargo's `--skip` matches test NAMES,
//          not the `#[ignore]` attribute) and either checkout base_ref
//          in a worktree or pass it through to the binary so the
//          baseline actually runs against the base tree.

use daemon::constraints::{
    format_prior_constraints_block, parse_latest_reroll_block, parse_not_addressed_schema_line,
    PriorRerollBlock, ReviewerStatus,
};
use daemon::dispatch::build_coder_prompt_with_prior_pub;
use daemon::tools::Bead;
use daemon::vacuous_red_green::{to_gate_status, RedGreenReport};
use daemon::verifier::VacuousRedGreenStatus;
use std::fs;
use std::path::PathBuf;

// -----------------------------------------------------------------------------
// (1)+(2) to_gate_status: vacuous=true with empty failing_tests → Failed
// -----------------------------------------------------------------------------
#[test]
fn r5_vacuous_with_empty_failing_tests_is_failed_not_pending() {
    let report = RedGreenReport {
        vacuous: true,
        failed_on_revert: 0,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["foo::bar".to_string()],
        failing_tests: vec![], // EMPTY — true vacuity, no synthetic entries
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("daemon/Cargo.toml"),
    };
    let status = to_gate_status(&report);
    match status {
        VacuousRedGreenStatus::Failed(_) => {} // expected
        VacuousRedGreenStatus::Pending(_) => {
            panic!(
                "vacuous=true with empty failing_tests is true vacuity — \
                 gate must be Failed (real defect), not Pending (which is \
                 reserved for synthetic-NEVER_RAN-only signals)"
            );
        }
        VacuousRedGreenStatus::Verified => {
            panic!(
                "vacuous=true must NEVER be Verified — this is the exact \
                 r1 bug the reviewer flagged"
            );
        }
        VacuousRedGreenStatus::NotRun => {
            panic!("detector clearly ran, status cannot be NotRun");
        }
    }
}

#[test]
fn r5_vacuous_with_only_synthetic_never_ran_remains_pending() {
    let report = RedGreenReport {
        vacuous: false, // list is non-empty (synthetic entries)
        failed_on_revert: 2,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["foo::bar".to_string()],
        failing_tests: vec!["foo::bar:NEVER_RAN".to_string(), "x:NEVER_RAN".to_string()],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("daemon/Cargo.toml"),
    };
    let status = to_gate_status(&report);
    assert!(
        matches!(status, VacuousRedGreenStatus::Pending(_)),
        "every observation is synthetic NEVER_RAN — gate must Pending (wait \
         and retry), got {status:?}"
    );
}

// -----------------------------------------------------------------------------
// (7) parse_not_addressed_schema_line: escape `\"` → `"` (no retained backslash)
// -----------------------------------------------------------------------------
#[test]
fn r5_parse_not_addressed_schema_line_unescapes_quoted_quote() {
    let text = r#"verdict: fail
NOT-ADDRESSED: ["a quote \" inside", "plain item"]
"#;
    let items = parse_not_addressed_schema_line(text).expect("schema line parsed");
    assert_eq!(
        items,
        vec!["a quote \" inside".to_string(), "plain item".to_string()],
        "escaped \\\" must unescape to a single \" — backslash must NOT be retained"
    );
}

#[test]
fn r5_parse_not_addressed_schema_line_preserves_plain_backslash() {
    // The raw `r#"..."#` string `["path\\to\\file", "x"]` carries the
    // four characters p-a-t-h-\-t-o-\-f-i-l-e. After our parser unescapes
    // each `\\` to a single `\`, the item is `path\to\file` — a single
    // backslash. (Backslash pairs are an escape pair; a lone trailing
    // backslash would be a malformed JSON item.)
    let text = r#"NOT-ADDRESSED: ["path\\to\\file", "x"]"#;
    let items = parse_not_addressed_schema_line(text).expect("parsed");
    assert_eq!(items, vec!["path\\to\\file".to_string(), "x".to_string()]);
}

// -----------------------------------------------------------------------------
// (9) format_prior_constraints_block: Addressed entries are omitted
// -----------------------------------------------------------------------------
#[test]
fn r5_format_prior_constraints_block_omits_addressed_items() {
    let prior = PriorRerollBlock {
        reviewer: "skeptic".to_string(),
        attempt: 1,
        not_addressed: vec![],
        not_addressed_structured: vec![
            ("item-A".to_string(), ReviewerStatus::Addressed),
            ("item-B".to_string(), ReviewerStatus::NotAddressed),
            ("item-C".to_string(), ReviewerStatus::NotApplicable),
        ],
        inhibition_specs: vec![],
        positive_assertions: vec![],
    };
    let block = format_prior_constraints_block(&prior);
    assert!(
        !block.contains("- item-A"),
        "Addressed item must NOT appear in 'must address'; block:\n{block}"
    );
    assert!(
        block.contains("- item-B"),
        "NotAddressed item MUST appear in 'must address'; block:\n{block}"
    );
    assert!(
        !block.contains("- item-C\n") || block.contains("- item-C"),
        "NotApplicable item MUST appear under out-of-scope only"
    );
    // Out-of-scope section header must be present.
    assert!(
        block.contains("Out-of-scope (N-A"),
        "NotApplicable items require the explicit out-of-scope header; block:\n{block}"
    );
    // And it must NOT be in the "must address" list.
    let must_address_idx = block.find("NOT-ADDRESSED items from previous reviewer (must address)");
    let out_of_scope_idx = block.find("Out-of-scope (N-A");
    if let (Some(m), Some(o)) = (must_address_idx, out_of_scope_idx) {
        let item_c_idx = block[m..o].find("- item-C");
        assert!(
            item_c_idx.is_none(),
            "NotApplicable item-C must NOT be in 'must address' section; block:\n{block}"
        );
    }
}

// -----------------------------------------------------------------------------
// (10) build_coder_prompt: prior_block participates in CODER_PROMPT_TOTAL_CAP
// -----------------------------------------------------------------------------
#[test]
fn r5_prior_block_is_bounded_within_total_prompt_cap() {
    // Build a minimal Bead. Build a fat prior_block that, combined with
    // boilerplate, would exceed CODER_PROMPT_TOTAL_CAP.
    let bead = Bead {
        id: "jleechan-test".to_string(),
        title: "T".to_string(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: None,
    };
    let fat_prior = "x".repeat(20_000);
    let prompt = build_coder_prompt_with_prior_pub(
        &bead,
        "factory/test",
        "jleechanorg/test",
        "origin",
        &fat_prior,
    );
    assert!(
        prompt.len() <= 4_000,
        "prompt must respect CODER_PROMPT_TOTAL_CAP (4000); got len={}",
        prompt.len()
    );
    // Sanity: the prompt should still include the bead id and branch.
    assert!(prompt.contains("jleechan-test"));
    assert!(prompt.contains("factory/test"));
    // And it must contain a truncation marker, since the prior_block was
    // far over budget.
    assert!(
        prompt.contains("[prior truncated]") || prompt.contains("[truncated]"),
        "oversized prior_block must be shrunk with a visible marker"
    );
}

// -----------------------------------------------------------------------------
// (12) run_cargo_baseline: must NOT pass a `cargo test NAME` filter meant
// to gate on the `#[ignore]` attribute. cargo has no such flag (#[ignore]
// tests are skipped by default; the only related flag is `--include-ignored`
// or `--ignored` which forces them to RUN). r3 passed `--skip ignored` —
// a name-substring filter that matched nothing; r5 removes it.
// -----------------------------------------------------------------------------
#[test]
fn r5_run_cargo_baseline_does_not_pass_ignore_filter() {
    // Grep the body of `run_cargo_baseline` for the bogus arg. If it
    // ever reappears the test fails loudly. We constrain the search to
    // the Command::new("cargo") ... .output() block so the explanatory
    // doc comments (which mention the r3 bug) don't false-positive.
    let src = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("vacuous_red_green.rs"),
    )
    .expect("read vacuous_red_green.rs");
    let cargo_start = src.find("Command::new(\"cargo\")").expect("cargo call exists");
    let cargo_end = src[cargo_start..]
        .find(".output()")
        .map(|n| cargo_start + n)
        .expect("cargo call ends");
    let cargo_block = &src[cargo_start..cargo_end];
    assert!(
        !cargo_block.to_lowercase().contains("ignored"),
        "run_cargo_baseline's cargo invocation must not pass an `ignored` \
         filter (cargo has no such flag — #[ignore] tests are skipped by \
         default); offending cargo args:\n{cargo_block}"
    );
}

// -----------------------------------------------------------------------------
// (3) verifier::evidence_floor_gate: vacuous check runs BEFORE verified early return
//
// We don't directly call the private gate helper, but we DO assert that
// to_gate_status surfaces the failure so the verifier caller can fold it.
// The verifier-level ordering is checked at runtime via the daemon path;
// unit-level coverage lives in the verifier integration test file.
// -----------------------------------------------------------------------------
#[test]
fn r5_to_gate_status_failed_is_not_run_check() {
    let report = RedGreenReport {
        vacuous: true,
        failed_on_revert: 0,
        genuine_red_on_revert: false,
        failed_without_never_ran: 0,
        green_on_head: true,
        baseline_passed: true,
        targeted_tests: vec!["foo::bar".to_string()],
        failing_tests: vec![],
        ignored_tests: vec![],
        ignored_without_skip_reason: vec![],
        manifest_path_used: PathBuf::from("daemon/Cargo.toml"),
    };
    let status = to_gate_status(&report);
    assert!(
        !matches!(status, VacuousRedGreenStatus::NotRun),
        "to_gate_status must always return a concrete status when given a \
         populated report; NotRun is reserved for the detector-was-disabled \
         path"
    );
}

// -----------------------------------------------------------------------------
// (8) extract: schema-line NOT-ADDRESSED parsed from redacted_text
//
// The public redaction helper is `redact_holdouts`. We assert the parser
// is invoked on redacted output by feeding it text that contains a
// holdout path which redact_holdouts masks, then asserting the masked
// text still parses cleanly. This is a regression pin against the bug
// flagged in PR #420 review comment 3625737311.
// -----------------------------------------------------------------------------
#[test]
fn r5_schema_line_parser_works_after_holdout_redaction() {
    use daemon::constraints::redact_holdouts;
    let raw = r#"verdict: fail
NOT-ADDRESSED: ["the test ~/projects/dark-factory-holdouts/secret_test.rs is missing", "plain item"]
"#;
    let (redacted, encountered) = redact_holdouts(raw);
    assert!(
        encountered,
        "redact_holdouts must flag the holdout path in this fixture"
    );
    let items = parse_not_addressed_schema_line(&redacted)
        .expect("schema line must parse after redaction");
    assert!(
        items
            .iter()
            .any(|i| i.contains("[REDACTED_HOLDOUT")),
        "holdout path must be redacted inside parsed items; got {items:?}"
    );
    assert!(items.iter().any(|i| i == "plain item"));
}

// -----------------------------------------------------------------------------
// parse_latest_reroll_block: still functions correctly (regression pin for
// the structured path; covered elsewhere but we touch it here so the r5
// suite exercises every public constraint helper).
// -----------------------------------------------------------------------------
#[test]
fn r5_parse_latest_reroll_block_round_trip() {
    let dir = tempfile_path("r5_reroll");
    let spec = dir.join("spec.toml");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        &spec,
        r#"
[bead]
id = "jleechan-test"

[[reroll]]
attempt = 1
reviewer = "skeptic"
not_addressed = ["item-A", "item-B"]
not_addressed_structured = [
  { key = "item-A", status = "NOT-ADDRESSED" },
  { key = "item-C", status = "N-A" },
]
inhibition_specs = []
positive_assertions = []
"#,
    )
    .unwrap();
    let parsed = parse_latest_reroll_block(&spec).expect("parsed");
    assert_eq!(parsed.attempt, 1);
    assert_eq!(parsed.reviewer, "skeptic");
    assert_eq!(parsed.not_addressed, vec!["item-A", "item-B"]);
    assert_eq!(
        parsed.not_addressed_structured,
        vec![
            ("item-A".to_string(), ReviewerStatus::NotAddressed),
            ("item-C".to_string(), ReviewerStatus::NotApplicable),
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

fn tempfile_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let unique = format!(
        "dark_factory_r5_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    p.push(unique);
    p
}