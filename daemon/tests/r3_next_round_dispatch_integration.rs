// Issue #408 / bead jleechan-1a5e r3: operator-required regression test
// asserting the next-round red dispatch consumes the structured
// NOT-ADDRESSED items the previous round wrote into spec.toml. r1+r2
// wrote the spec block + telemetry but the coder prompt builder never
// read the spec back — without this fix, structured NOT-ADDRESSED is
// dead text. This test wires the full pipeline end-to-end:
//
//   1. Construct the `[[reroll]]` TOML block the previous round would
//      have written (structured NOT-ADDRESSED + N-A entries).
//   2. Append the block to spec.toml via `append_mutation`.
//   3. `parse_latest_reroll_block` reads it back.
//   4. `format_prior_constraints_block` renders the next-round block.
//   5. `build_coder_prompt` (via dispatch::build_coder_prompt_with_prior)
//      MUST carry those NOT-ADDRESSED items verbatim into the spawn
//      prompt.

use daemon::constraints::{
    append_mutation, format_prior_constraints_block, parse_latest_reroll_block, ReviewerStatus,
};

#[test]
fn r3_next_round_coder_prompt_consumes_structured_not_addressed() {
    // (1) Build the spec block the previous round would have written.
    // Use the wire format shape the r3 baseline inlined into
    // `reroll.rs::execute()`.
    let block = r#"
[[reroll]]
         reviewer = "claude"
         attempt = 1
         inhibition_specs = [
           "no global mutable state",
         ]
         positive_assertions = [
           "must call redact_holdouts before LLM",
         ]
         not_addressed = [
           "3-check coverage",
           "ignored_without_skip_reason gate",
         ]
         not_addressed_structured = [
           { key = "3-check coverage", status = "NOT-ADDRESSED" },
           { key = "ignored_without_skip_reason gate", status = "NOT-ADDRESSED" },
           { key = "legacy flat shape", status = "N-A" },
         ]
         raw_feedback = """
         raw reviewer feedback
         """
"#;

    // (2) Append the block to a real spec.toml so the parser exercises
    // the file path (not just an in-memory TOML string).
    let tmp = std::env::temp_dir().join(format!(
        "vacuous_rg_r3_prior_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&tmp).expect("mkdir tmp");
    let spec_path = tmp.join("bead-jleechan-1a5e.toml");
    append_mutation(&spec_path, block).expect("append_mutation");

    // (3) Parse it back.
    let prior = parse_latest_reroll_block(&spec_path)
        .expect("parse_latest_reroll_block must surface the [[reroll]] block");
    assert_eq!(prior.attempt, 1);
    assert_eq!(prior.reviewer, "claude");
    assert_eq!(
        prior.not_addressed_structured.len(),
        3,
        "structured NOT-ADDRESSED + N-A must round-trip; got {:?}",
        prior.not_addressed_structured
    );
    assert!(
        prior
            .not_addressed_structured
            .iter()
            .any(|(k, s)| k == "3-check coverage" && *s == ReviewerStatus::NotAddressed),
        "structured NOT-ADDRESSED entry missing; got {:?}",
        prior.not_addressed_structured
    );
    assert!(
        prior
            .not_addressed_structured
            .iter()
            .any(|(k, s)| k == "legacy flat shape" && *s == ReviewerStatus::NotApplicable),
        "structured N-A entry missing; got {:?}",
        prior.not_addressed_structured
    );

    // (4) Render the prior-constraints block.
    let prior_section = format_prior_constraints_block(&prior);
    assert!(
        prior_section.contains("PREVIOUS ATTEMPT CONSTRAINTS"),
        "rendered block missing the section header; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("3-check coverage"),
        "rendered block must carry structured NOT-ADDRESSED key; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("ignored_without_skip_reason gate"),
        "rendered block must carry the second structured NOT-ADDRESSED key; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("Out-of-scope"),
        "rendered block must distinguish N-A items so the next coder knows not to chase them; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("legacy flat shape"),
        "rendered block must surface N-A keys under the Out-of-scope section; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("Inhibition specs")
            && prior_section.contains("no global mutable state"),
        "rendered block must surface inhibition specs; got:\n{prior_section}"
    );
    assert!(
        prior_section.contains("Positive assertions")
            && prior_section.contains("must call redact_holdouts before LLM"),
        "rendered block must surface positive assertions; got:\n{prior_section}"
    );

    // (5) Build the coder prompt and assert the rendered prior_section
    // is carried into the spawn prompt verbatim. Without this the
    // structured NOT-ADDRESSED items are dead text.
    let prompt = {
        use daemon::dispatch::build_coder_prompt_with_prior_pub;
        let bead = daemon::tools::Bead {
            id: "bead-jleechan-1a5e".to_string(),
            title: "jleechan-1a5e r3 prior-constraints regression".to_string(),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        };
        build_coder_prompt_with_prior_pub(
            &bead,
            "factory/bead-jleechan-1a5e-r3",
            "jleechanorg/dark-factory",
            "origin",
            &prior_section,
        )
    };
    assert!(
        prompt.contains("PREVIOUS ATTEMPT CONSTRAINTS"),
        "next-round coder prompt missing the prior-constraints block; full prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("3-check coverage"),
        "next-round coder prompt must surface structured NOT-ADDRESSED key; got:\n{prompt}"
    );
    assert!(
        prompt.contains("ignored_without_skip_reason gate"),
        "next-round coder prompt must surface the second structured NOT-ADDRESSED key; got:\n{prompt}"
    );
    assert!(
        prompt.contains("legacy flat shape"),
        "next-round coder prompt must surface N-A keys (out-of-scope, do-not-chase); got:\n{prompt}"
    );

    // Cleanup.
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn r3_first_attempt_bead_prompt_is_byte_identical_to_pre_r3() {
    // First-attempt beads (no prior [[reroll]] block in spec.toml)
    // must produce a prompt BYTE-IDENTICAL to the pre-r3 renderer —
    // the prior_block is empty for them, so the prompt template is
    // unchanged. This guards against accidentally adding visible
    // markers or noise for beads that don't carry prior-attempt data.
    use daemon::dispatch::build_coder_prompt;
    let bead = daemon::tools::Bead {
        id: "bead-first-attempt".to_string(),
        title: "first attempt, no prior constraints".to_string(),
        description: "a description".to_string(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: None,
    };
    let p1 = build_coder_prompt(
        &bead,
        "factory/bead-first-attempt-r1",
        "jleechanorg/dark-factory",
        "origin",
    );
    assert!(
        !p1.contains("PREVIOUS ATTEMPT CONSTRAINTS"),
        "first-attempt prompt must NOT include the prior-constraints block; got:\n{p1}"
    );
}

#[test]
fn r3_prompt_template_remains_byte_identical_when_prior_block_is_empty() {
    // Cross-check: build_coder_prompt (no prior) and
    // build_coder_prompt_with_prior_pub (with empty prior) MUST
    // produce the same output. Any divergence means we accidentally
    // added visible artifacts to the prompt template.
    use daemon::dispatch::{build_coder_prompt, build_coder_prompt_with_prior_pub};
    let bead = daemon::tools::Bead {
        id: "bead-stable".to_string(),
        title: "stable test".to_string(),
        description: "desc".to_string(),
        notes: "n".to_string(),
        file_tree_summary: "t".to_string(),
        external_ref: None,
    };
    let p1 = build_coder_prompt(
        &bead,
        "factory/x-r1",
        "jleechanorg/dark-factory",
        "origin",
    );
    let p2 = build_coder_prompt_with_prior_pub(
        &bead,
        "factory/x-r1",
        "jleechanorg/dark-factory",
        "origin",
        "",
    );
    assert_eq!(
        p1, p2,
        "build_coder_prompt and build_coder_prompt_with_prior_pub with empty prior must agree"
    );
}