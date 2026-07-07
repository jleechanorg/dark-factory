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
use crate::tools::{LabeledPr, Scm, Tracker};

const FACTORY_LABEL: &str = "factory";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPrIntake {
    pub bead_id: String,
    pub pr_number: u64,
    pub head_ref_name: String,
    pub external_ref: String,
    pub newly_created: bool,
}

fn same_repo_pr(pr: &LabeledPr, cfg: &Config) -> bool {
    if pr.is_cross_repository {
        return false;
    }
    if let Some(head_repo) = pr.head_repo_full_name.as_deref() {
        return head_repo.eq_ignore_ascii_case(&cfg.target_repo);
    }
    if let Some(head_owner) = pr.head_repo_owner_login.as_deref() {
        let target_owner = cfg.target_repo.split('/').next().unwrap_or_default();
        return head_owner.eq_ignore_ascii_case(target_owner);
    }
    true
}

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
        let bead_id = match tracker.create_bead(&title, &issue.body, &issue.external_ref) {
            Ok(id) => id,
            Err(e) => {
                // jleechan-u4gb: the known_refs pre-check above is a bulk
                // snapshot read that can race with a concurrent write (e.g.
                // a duplicate labeled-issue entry within the same batch, or
                // staleness/skew in the underlying `br list` snapshot) and
                // miss a ref that was actually already tracked. `br create`'s
                // own uniqueness constraint is authoritative and catches it
                // at write time; treat that as "already tracked" (same
                // outcome as the known_refs.contains skip above) instead of
                // failing the whole tick and retrying forever — the ref will
                // *always* already exist on retry, so propagating this as a
                // transient error just burns exponential backoff for no
                // benefit.
                if let Some(existing_bead_id) = e.duplicate_external_ref_bead_id() {
                    eprintln!(
                        "auto-factory daemon: intake race recovered — external_ref {:?} already tracked by {existing_bead_id} (known_refs pre-check missed it); skipping create_bead",
                        issue.external_ref
                    );
                    continue;
                }
                return Err(e);
            }
        };

        let comment_body = format!(
            "🤖 **[dark-factory]** Auto-factory has picked up this task. Created tracking bead `{}`. Spawning worker session...",
            bead_id
        );
        let _ = tracker.comment_external(&issue.external_ref, &comment_body);

        created.push(bead_id);
    }

    Ok(created)
}

/// Normalize open PRs labeled `factory` into beads that should attach to the
/// existing PR/head branch rather than dispatching a fresh factory branch.
pub fn normalize_labeled_prs(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
) -> Result<Vec<ExistingPrIntake>, DaemonError> {
    let prs = scm.labeled_prs(FACTORY_LABEL)?;
    if prs.is_empty() {
        return Ok(Vec::new());
    }

    let tracker_candidates = tracker.fetch_candidates()?;
    let known_refs = tracker.fetch_all_external_refs()?;
    let mut intakes = Vec::new();

    for pr in prs {
        if pr.head_ref_name.trim().is_empty() {
            continue;
        }

        if !same_repo_pr(&pr, cfg) {
            let comment_body = "🤖 **[dark-factory]** Escalation required: fork/cross-repository PR adoption is not supported in v1. Same-repo factory PRs can be verified automatically; fork remediation lands with bead `jleechan-tfs1`.";
            let _ = tracker.comment_external(&pr.external_ref, comment_body);
            continue;
        }

        let permission = scm.collaborator_permission(&pr.author_login)?;
        if !permission.is_write_tier() {
            continue;
        }

        if let Some(bead) = tracker_candidates
            .iter()
            .find(|bead| bead.external_ref.as_deref() == Some(pr.external_ref.as_str()))
        {
            intakes.push(ExistingPrIntake {
                bead_id: bead.id.clone(),
                pr_number: pr.number,
                head_ref_name: pr.head_ref_name,
                external_ref: pr.external_ref,
                newly_created: false,
            });
            continue;
        }

        if known_refs.contains(&pr.external_ref) {
            continue;
        }

        let title = format!("{} ({})", pr.title, cfg.target_repo);
        let bead_id = match tracker.create_bead(&title, &pr.body, &pr.external_ref) {
            Ok(id) => id,
            Err(e) => {
                // jleechan-u4gb: same write-time-vs-read-time race as
                // intake::normalize above — `br create`'s uniqueness
                // constraint is authoritative; treat a caught duplicate as
                // already-adopted rather than failing the whole tick.
                if let Some(existing_bead_id) = e.duplicate_external_ref_bead_id() {
                    eprintln!(
                        "auto-factory daemon: PR intake race recovered — external_ref {:?} already tracked by {existing_bead_id} (known_refs pre-check missed it); skipping create_bead",
                        pr.external_ref
                    );
                    continue;
                }
                return Err(e);
            }
        };
        let comment_body = format!(
            "🤖 **[dark-factory]** Auto-factory has picked up this pull request. Created tracking bead `{}` and will verify the existing branch `{}`.",
            bead_id, pr.head_ref_name
        );
        let _ = tracker.comment_external(&pr.external_ref, &comment_body);
        intakes.push(ExistingPrIntake {
            bead_id,
            pr_number: pr.number,
            head_ref_name: pr.head_ref_name,
            external_ref: pr.external_ref,
            newly_created: true,
        });
    }

    Ok(intakes)
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
