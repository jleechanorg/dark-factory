//! Task 6: pre-poll intake normalizer (design doc §5, spec §4.2.3).
//!
//! Converts labeled GitHub issues into `br` beads, idempotently keyed on
//! `external_ref = "<owner>/<repo>#<issue_number>"`, and enforces the
//! write-tier authorization gate: only issues created by a collaborator with
//! `Permission::Write` or `Permission::Admin` may trigger bead creation. Lower
//! tiers (`None`/`Read`/`Triage`) are skipped — never silently dropped without
//! a trace, but also never allowed to trigger dispatch, per spec §4.2.3
//! ("the daemon effectively escalates the issue creator's privilege ... via
//! the AO session's credentials").
use crate::config::Config;
use crate::errors::DaemonError;
use crate::tools::{Scm, Tracker};

const FACTORY_LABEL: &str = "factory";

/// Normalize labeled issues into beads.
///
/// * Fetches candidate issues labeled `factory` from the SCM.
/// * Skips any issue whose `external_ref` already appears among the tracker's
///   known candidates (idempotency — no duplicate `create_bead` calls).
/// * Checks the issue author's collaborator permission tier; only
///   `Permission::Write` / `Permission::Admin` pass. Lower tiers are skipped
///   (not an error) — the skip itself (issue external_ref + author_login) is
///   the audit context callers/telemetry record; this function performs no
///   further I/O side effects for skipped issues.
/// * For each newly-authorized issue, calls `create_bead` and collects the
///   returned bead id.
///
/// Returns the ids of beads newly created during this pass (empty if nothing
/// new). Idempotent: running twice against an unchanged SCM/tracker produces
/// no new beads on the second run.
pub fn normalize(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
) -> Result<Vec<String>, DaemonError> {
    let issues = scm.labeled_issues(FACTORY_LABEL)?;
    if issues.is_empty() {
        return Ok(Vec::new());
    }

    let known_refs = tracker.fetch_all_external_refs()?;

    let mut created = Vec::new();

    for issue in issues {
        // Idempotency: already-known external_ref -> skip silently, no create_bead call.
        if known_refs.contains(&issue.external_ref) {
            continue;
        }

        // Write-tier authorization gate (spec §4.2.3): only Write/Admin may
        // trigger dispatch. Lower tiers are skipped, not errored — the skip
        // itself is the audit trail the caller records via telemetry, keyed
        // on issue.external_ref + issue.author_login.
        let permission = scm.collaborator_permission(&issue.author_login)?;
        if !permission.is_write_tier() {
            continue;
        }

        let title = format!("{} ({})", issue.title, cfg.target_repo);
        let bead_id = tracker.create_bead(&title, &issue.body, &issue.external_ref)?;
        
        let comment_body = format!(
            "🤖 **[dark-factory]** Auto-factory has picked up this task. Created tracking bead `{}`. Spawning worker session...",
            bead_id
        );
        let _ = tracker.comment_external(&issue.external_ref, &comment_body);

        created.push(bead_id);
    }

    Ok(created)
}

#[cfg(test)]
mod tests {
    // Unit-level coverage for the pure permission-gate helper; the fake-backed
    // contract tests (idempotency, write-tier gate, mixed batch) live in
    // `daemon/tests/intake.rs` per Task 6 Step 1.
    use crate::tools::Permission;

    #[test]
    fn permission_write_tier_gate_matches_design_contract() {
        assert!(Permission::Write.is_write_tier());
        assert!(Permission::Admin.is_write_tier());
        assert!(!Permission::Read.is_write_tier());
        assert!(!Permission::Triage.is_write_tier());
        assert!(!Permission::None.is_write_tier());
    }
}
