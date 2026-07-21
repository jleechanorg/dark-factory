//! Daemon-side contract-echo (issue #386).
//!
//! The Python `runner/skeptic_gate.py` (and `skeptic_gate_cli.py`) implement
//! the production contract-echo step for the Stage-1 Skeptic gate. The
//! daemon-side `tick::skeptic_evidence` path is SEPARATE — it's the
//! Stage-1 *fast-tier* assessment that runs inside the daemon process
//! before the GitHub Actions skeptic-gate workflow fires.
//!
//! r8 (issue #386 r4 ATTEMPT GUIDANCE gap 1): the daemon's `skeptic_evidence`
//! MUST honor the bead's contract when one is available — load the contract
//! from `br show --json <bead_id>`, append the acceptance items + prior
//! findings to the reviewer prompt, parse the reviewer's `CONTRACT_ECHO:`
//! block, and convert any NOT-ADDRESSED item into a `Fail(verbatim_reason)`
//! verdict so the next-round reroll's `red_reasons` carries the exact
//! problem into `constraints::extract`.
//!
//! Mirroring `runner/skeptic_gate.py`'s public surface keeps the daemon
//! and the GHA gate on the same contract shape:
//!   - `AcceptanceItem { id, text, required }`
//!   - `PriorFinding { source, text }`
//!   - `BeadContract { id, description, notes, prior_findings, acceptance_items }`
//!   - `ContractEchoVerdict` ∈ {ADDRESSED, NOT-ADDRESSED, N-A}
//!   - `ContractEchoItem { id, verdict, cite, reason }`
//!
//! The struct shapes match the Python types by field name + JSON key
//! (snake_case → camelCase is left to the daemon's serde layer; the
//! `br show --json` output uses snake_case, which this module consumes
//! directly).

use crate::errors::DaemonError;
use serde::{Deserialize, Serialize};
use std::process::Command;

/// Per-item contract-echo verdict emitted by the reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE")]
pub enum ContractEchoVerdict {
    Addressed,
    NotAddressed,
    NA,
}

/// A single acceptance item from the bead's contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceItem {
    pub id: String,
    pub text: String,
    /// r3 (issue #386 r4 ATTEMPT GUIDANCE gap 5): when true, a reviewer
    /// verdict of N-A is treated as unaddressed — the bead author says
    /// "this must be done", so the reviewer cannot opt out.
    #[serde(default)]
    pub required: bool,
}

/// A finding from a prior round that the bead author wants the reviewer
/// to address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorFinding {
    pub source: String,
    pub text: String,
}

/// The bead's contract: the durable input to the contract-echo step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadContract {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub prior_findings: Vec<PriorFinding>,
    pub acceptance_items: Vec<AcceptanceItem>,
}

/// A per-item contract-echo verdict emitted by the reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractEchoItem {
    pub id: String,
    pub verdict: ContractEchoVerdict,
    /// File:line cite (e.g. "runner/skeptic_gate.py:42"). Required for
    /// ADDRESSED, may be empty for NOT-ADDRESSED / N-A.
    #[serde(default)]
    pub cite: String,
    /// Reviewer's justification — required for NOT-ADDRESSED and N-A.
    #[serde(default)]
    pub reason: String,
}

/// Result of evaluating a reviewer's contract-echo output against the
/// contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEchoResult {
    /// True iff every acceptance item is ADDRESSED, or N-A with non-empty
    /// reason AND the item is not required=true. False iff any item is
    /// NOT-ADDRESSED, missing, or a required item is N-A.
    pub ok: bool,
    /// Verbatim acceptance items that did not pass.
    pub unaddressed_items: Vec<AcceptanceItem>,
    /// Human-readable constraint string suitable for embedding in the
    /// gate's failure comment or handing to the next roll. Contains
    /// the verbatim text of every unaddressed item.
    pub constraint: String,
}

/// Load a `BeadContract` directly from the live bead source via
/// `br show --json <bead_id>`.
///
/// This is the r8 (issue #386 r4 ATTEMPT GUIDANCE gap 2) production
/// path: never hand-author contracts, always read the bead source.
///
/// Subprocess and parse errors return `Err` so callers fail closed —
/// never silently fabricate a contract.
pub fn load_bead_contract_from_br(
    bead_id: &str,
    br_bin: &str,
) -> Result<BeadContract, DaemonError> {
    let output = Command::new(br_bin)
        .args(["show", "--json", bead_id])
        .output()
        .map_err(|e| DaemonError::Tool {
            tool: "br".into(),
            rc: -1,
            stderr: format!("failed to spawn {br_bin} show --json {bead_id}: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        return Err(DaemonError::Tool {
            tool: "br".into(),
            rc: output.status.code().unwrap_or(-1),
            stderr: format!(
                "{br_bin} show --json {bead_id} failed: stderr={stderr:?} stdout={stdout:?}"
            ),
        });
    }

    let raw = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut payload: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| DaemonError::Parse(format!(
            "{br_bin} show --json {bead_id} reply did not parse as JSON: {e} (reply: {raw:?})"
        )))?;

    // `br show --json` returns a JSON object whose shape is documented
    // in `~/.claude/docs/beads.md`. The bead record is the root; map
    // the canonical fields onto `BeadContract`.
    if let Some(obj) = payload.as_object_mut() {
        if !obj.contains_key("id") {
            obj.insert("id".into(), serde_json::Value::String(bead_id.into()));
        }
        if !obj.contains_key("description") {
            // Some beads use `body` rather than `description`. Fall
            // back so the contract still loads.
            if let Some(body) = obj.remove("body") {
                obj.insert("description".into(), body);
            } else {
                obj.insert(
                    "description".into(),
                    serde_json::Value::String(String::new()),
                );
            }
        }
        if !obj.contains_key("acceptance_items") {
            obj.insert(
                "acceptance_items".into(),
                serde_json::Value::Array(Vec::new()),
            );
        }
    }

    let contract: BeadContract = serde_json::from_value(payload).map_err(|e| {
        DaemonError::Parse(format!(
            "bead {bead_id} JSON did not match BeadContract shape: {e}"
        ))
    })?;

    if contract.acceptance_items.is_empty() {
        return Err(DaemonError::Parse(format!(
            "bead {bead_id} contract has no acceptance_items — refusing to \
             run contract-echo without a contract; the bead author must \
             supply at least one acceptance item"
        )));
    }

    Ok(contract)
}

/// Render the contract items + prior findings as a `CONTRACT:` block
/// appended to the reviewer prompt. Format mirrors
/// `runner/skeptic_gate.build_prompt`'s contract block shape so the
/// reviewer's mental model is identical whether the prompt came from
/// the daemon or the Python gate.
pub fn build_contract_prompt_block(contract: &BeadContract) -> String {
    let mut out = String::new();
    out.push_str("\n\nCONTRACT (issue #386 contract-echo):\n");
    out.push_str(&format!("bead_id: {}\n", contract.id));
    out.push_str(&format!("description: {}\n", contract.description));
    if !contract.notes.is_empty() {
        out.push_str("notes:\n");
        for note in &contract.notes {
            out.push_str(&format!("  - {note}\n"));
        }
    }
    if !contract.prior_findings.is_empty() {
        out.push_str("prior_findings:\n");
        for pf in &contract.prior_findings {
            out.push_str(&format!("  - source={} text={}\n", pf.source, pf.text));
        }
    }
    out.push_str("acceptance_items:\n");
    for it in &contract.acceptance_items {
        let required_marker = if it.required { " [REQUIRED]" } else { "" };
        out.push_str(&format!(
            "  - id={}{} text={}\n",
            it.id, required_marker, it.text
        ));
    }
    out.push_str("\nFor each acceptance_item emit one line:\n");
    out.push_str(
        "  ITEM: <id> VERDICT: <ADDRESSED|NOT-ADDRESSED|N-A> \
         CITE: <file:line> (for ADDRESSED) or REASON: <text> \
         (for NOT-ADDRESSED / N-A)\n",
    );
    out.push_str("Wrap them under a `CONTRACT_ECHO:` header. \
                  Required items cannot be N-A.");
    out
}

/// Parse the `CONTRACT_ECHO:` block out of the reviewer's stdout.
///
/// Mirrors `runner/skeptic_gate.parse_contract_echo`'s strict shape:
///   CONTRACT_ECHO:
///   ITEM: <id> VERDICT: <ADDRESSED|NOT-ADDRESSED|N-A> CITE: <file:line>
///   ITEM: <id> VERDICT: <N-A> REASON: <text>
///
/// `None` is returned when the block is absent or unparseable — the
/// caller treats `None` as every item NOT-ADDRESSED (fail-closed).
pub fn parse_contract_echo(output: &str) -> Option<Vec<ContractEchoItem>> {
    if !output.contains("CONTRACT_ECHO:") {
        return None;
    }
    let after_header = output.split("CONTRACT_ECHO:").nth(1)?;
    let mut items: Vec<ContractEchoItem> = Vec::new();
    for raw_line in after_header.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("ITEM:") {
            // Out-of-block content. Mirrors `parse_contract_echo`'s
            // strict shape: an unparseable line anywhere in the block
            // fails the whole parse (no guesses). The Python gate
            // enforces the same fail-closed posture so the daemon
            // and the GHA gate agree on what counts as a valid
            // `CONTRACT_ECHO:` reply.
            return None;
        }
        // Strip the `ITEM:` prefix and parse the trailing fields.
        let body = line.strip_prefix("ITEM:")?.trim();
        // First token is the item id (no whitespace).
        let mut parts = body.splitn(2, char::is_whitespace);
        let id = parts.next()?.trim().to_string();
        if id.is_empty() {
            return None;
        }
        let rest = parts.next().unwrap_or("").trim();
        // Locate VERDICT: and pull out the verdict token + the trailing
        // fields (CITE: / REASON: in any order).
        let verdict_token = rest
            .strip_prefix("VERDICT:")
            .map(|s| s.split_whitespace().next().unwrap_or(""))
            .unwrap_or("");
        let verdict = match verdict_token {
            "ADDRESSED" => ContractEchoVerdict::Addressed,
            "NOT-ADDRESSED" => ContractEchoVerdict::NotAddressed,
            "N-A" => ContractEchoVerdict::NA,
            _ => return None,
        };
        // After the verdict token, CITE: and REASON: may appear in any
        // order. Take everything after the verdict token and parse each
        // marker independently. Trim between the two `trim_start_matches`
        // calls because the verdict token may be preceded by whitespace
        // (so `trim_start_matches` alone won't strip it).
        let after_verdict = rest
            .trim_start_matches("VERDICT:")
            .trim()
            .trim_start_matches(verdict_token)
            .trim();
        let cite = after_verdict
            .strip_prefix("CITE:")
            .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
            .unwrap_or_default();
        let reason = after_verdict
            .strip_prefix("REASON:")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        // Strict validation mirrors Python:
        //   - ADDRESSED requires a CITE: matching file:line.
        //   - NOT-ADDRESSED / N-A require a non-empty REASON:.
        match verdict {
            ContractEchoVerdict::Addressed => {
                if !is_valid_cite(&cite) {
                    return None;
                }
            }
            _ => {
                if reason.is_empty() {
                    return None;
                }
            }
        }
        items.push(ContractEchoItem {
            id,
            verdict,
            cite,
            reason,
        });
    }
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn is_valid_cite(cite: &str) -> bool {
    // Mirrors Python: `^[\w./\-]+:\d+$`
    if cite.is_empty() {
        return false;
    }
    let Some((path, line_no)) = cite.rsplit_once(':') else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    if !line_no.chars().all(|c| c.is_ascii_digit()) || line_no.is_empty() {
        return false;
    }
    path.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '/' || c == '-')
}

/// Evaluate the parsed contract-echo report against the contract.
///
/// r8 (issue #386 r4 ATTEMPT GUIDANCE gap 5): required=true items
/// cannot be N-A. An item the reviewer omitted entirely is also
/// treated as NOT-ADDRESSED. The `constraint` field carries the
/// verbatim text of every unaddressed item, suitable for the
/// next-round reroll's `red_reasons`.
pub fn evaluate_contract_echo(
    report: Option<&[ContractEchoItem]>,
    contract: &BeadContract,
) -> ContractEchoResult {
    let mut unaddressed: Vec<AcceptanceItem> = Vec::new();
    let by_id: std::collections::HashMap<&str, &ContractEchoItem> = match report {
        Some(items) => items
            .iter()
            .map(|it| (it.id.as_str(), it))
            .collect(),
        None => std::collections::HashMap::new(),
    };

    for item in &contract.acceptance_items {
        let verdict = by_id.get(item.id.as_str()).map(|it| it.verdict);
        let unaddressed_here = match verdict {
            None => true, // missing
            Some(ContractEchoVerdict::NotAddressed) => true,
            Some(ContractEchoVerdict::NA) => item.required,
            Some(ContractEchoVerdict::Addressed) => false,
        };
        if unaddressed_here {
            unaddressed.push(item.clone());
        }
    }

    let ok = unaddressed.is_empty();
    let mut constraint = String::new();
    if !unaddressed.is_empty() {
        constraint.push_str("UNADDRESSED ACCEPTANCE ITEMS:\n");
        for it in &unaddressed {
            let marker = if it.required { " [REQUIRED]" } else { "" };
            constraint.push_str(&format!("- {}{}: {}\n", it.id, marker, it.text));
        }
    }
    ContractEchoResult {
        ok,
        unaddressed_items: unaddressed,
        constraint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contract() -> BeadContract {
        BeadContract {
            id: "jleechan-pq08".into(),
            description: "Contract-echo review step".into(),
            notes: vec!["do NOT N-A away acceptance items".into()],
            prior_findings: vec![PriorFinding {
                source: "r5 reviewer".into(),
                text: "rN attempts ship without closing their own acceptance criteria".into(),
            }],
            acceptance_items: vec![
                AcceptanceItem {
                    id: "A1".into(),
                    text: "diff addresses A1".into(),
                    required: false,
                },
                AcceptanceItem {
                    id: "A2".into(),
                    text: "required=true acceptance items must NOT be N-A-eligible".into(),
                    required: true,
                },
                AcceptanceItem {
                    id: "A3".into(),
                    text: "3-item round-trip fixture".into(),
                    required: false,
                },
            ],
        }
    }

    #[test]
    fn parse_three_item_addressed_addressed_not_addressed() {
        let output = "\
            CONTRACT_ECHO:\n\
            ITEM: A1 VERDICT: ADDRESSED CITE: foo/bar.py:1\n\
            ITEM: A2 VERDICT: ADDRESSED CITE: foo/bar.py:2\n\
            ITEM: A3 VERDICT: NOT-ADDRESSED REASON: handler missing\n";
        let items = parse_contract_echo(output);
        if items.is_none() {
            panic!(
                "parse_contract_echo returned None; debug: output bytes = {:?}",
                output
            );
        }
        let items = items.expect("parse must succeed");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "A1");
        assert_eq!(items[0].verdict, ContractEchoVerdict::Addressed);
        assert_eq!(items[2].verdict, ContractEchoVerdict::NotAddressed);
    }

    #[test]
    fn parse_returns_none_when_block_absent() {
        assert!(parse_contract_echo("VERDICT: PASS\nno contract here").is_none());
    }

    #[test]
    fn parse_rejects_unparseable_line() {
        let output = "\
            CONTRACT_ECHO:\n\
            ITEM: A1 VERDICT: ADDRESSED CITE: foo.py:1\n\
            THIS IS NOT A VALID ITEM LINE\n";
        assert!(parse_contract_echo(output).is_none());
    }

    #[test]
    fn parse_rejects_addressed_without_cite() {
        let output = "\
            CONTRACT_ECHO:\n\
            ITEM: A1 VERDICT: ADDRESSED\n";
        assert!(parse_contract_echo(output).is_none());
    }

    #[test]
    fn parse_rejects_na_without_reason() {
        let output = "\
            CONTRACT_ECHO:\n\
            ITEM: A1 VERDICT: N-A\n";
        assert!(parse_contract_echo(output).is_none());
    }

    #[test]
    fn evaluate_three_item_round_trip_unaddressed_only_a3() {
        let contract = sample_contract();
        let report = vec![
            ContractEchoItem {
                id: "A1".into(),
                verdict: ContractEchoVerdict::Addressed,
                cite: "foo/bar.py:1".into(),
                reason: String::new(),
            },
            ContractEchoItem {
                id: "A2".into(),
                verdict: ContractEchoVerdict::Addressed,
                cite: "foo/bar.py:2".into(),
                reason: String::new(),
            },
            ContractEchoItem {
                id: "A3".into(),
                verdict: ContractEchoVerdict::NotAddressed,
                cite: String::new(),
                reason: "handler missing".into(),
            },
        ];
        let res = evaluate_contract_echo(Some(&report), &contract);
        assert!(!res.ok);
        assert_eq!(res.unaddressed_items.len(), 1);
        assert_eq!(res.unaddressed_items[0].id, "A3");
        assert!(res.constraint.contains("A3"));
        assert!(
            res.constraint.contains("3-item round-trip fixture"),
            "verbatim A3 text must appear in constraint: {:?}",
            res.constraint
        );
    }

    #[test]
    fn evaluate_required_item_na_is_unaddressed() {
        // r8 gap 5: required=true items cannot be N-A'd away.
        let contract = sample_contract();
        let report = vec![
            ContractEchoItem {
                id: "A1".into(),
                verdict: ContractEchoVerdict::Addressed,
                cite: "foo/bar.py:1".into(),
                reason: String::new(),
            },
            ContractEchoItem {
                id: "A2".into(),
                verdict: ContractEchoVerdict::NA,
                cite: String::new(),
                reason: "skipped".into(),
            },
            ContractEchoItem {
                id: "A3".into(),
                verdict: ContractEchoVerdict::Addressed,
                cite: "foo/bar.py:3".into(),
                reason: String::new(),
            },
        ];
        let res = evaluate_contract_echo(Some(&report), &contract);
        assert!(!res.ok);
        assert_eq!(res.unaddressed_items.len(), 1);
        assert_eq!(res.unaddressed_items[0].id, "A2");
        assert!(res.constraint.contains("[REQUIRED]"));
    }

    #[test]
    fn evaluate_missing_report_is_all_unaddressed() {
        let contract = sample_contract();
        let res = evaluate_contract_echo(None, &contract);
        assert!(!res.ok);
        assert_eq!(res.unaddressed_items.len(), 3);
        assert!(res.constraint.contains("A1"));
        assert!(res.constraint.contains("A2"));
        assert!(res.constraint.contains("A3"));
    }

    #[test]
    fn build_prompt_block_carries_verbatim_text_and_required_marker() {
        let contract = sample_contract();
        let block = build_contract_prompt_block(&contract);
        assert!(block.contains("jleechan-pq08"));
        assert!(block.contains("diff addresses A1"));
        assert!(block.contains("required=true acceptance items must NOT be N-A-eligible"));
        // Required marker appears on A2.
        assert!(block.contains("A2 [REQUIRED]"));
        // No required marker on A1 or A3.
        assert!(!block.contains("A1 [REQUIRED]"));
        // Notes + prior findings are in the block.
        assert!(block.contains("do NOT N-A away acceptance items"));
        assert!(block.contains("r5 reviewer"));
    }
}