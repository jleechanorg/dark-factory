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
    /// jleechan-dljf (issue #271): resolved repo identity from the PR's
    /// external_ref prefix, propagated through to the adoption path so
    /// `run_slow_tier` never needs to re-derive it.
    pub target_repo: Option<String>,
    pub newly_created: bool,
}

/// jleechan-eazj: the five verdicts every factory-labeled candidate must
/// resolve to, exactly once, per slow tick. `Adopted` is reported separately
/// by the caller (it already carries a bead id via `created`/`ExistingPrIntake`
/// and an INTAKE_BEAD_CREATED/EXISTING_PR_ADOPTED telemetry event); the four
/// variants here cover every path that does NOT result in a bead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeVerdict {
    /// `external_ref` already tracked by the bead store (idempotency skip,
    /// either via the pre-check or a `create_bead` uniqueness-constraint
    /// race recovered at write time).
    SkippedDuplicate,
    /// Fork/cross-repository PR — branch-stealing guard rejected it.
    SkippedFork,
    /// Label present but some other precondition failed. `precondition`
    /// names exactly which one (e.g. "author_permission_below_write_tier",
    /// "empty_head_ref_name") so the telemetry line is self-explanatory
    /// without cross-referencing source.
    SkippedIneligible { precondition: String },
    /// An operation on this specific candidate failed (e.g. `create_bead` or
    /// `collaborator_permission` errored). `reason` is the real error
    /// string, not a generic message. Recorded and the loop *continues* —
    /// one candidate's error must never abort processing of the rest of the
    /// batch (jleechan-eazj: this was the actual root cause of issue #8171
    /// vanishing with zero telemetry — an earlier candidate's non-transient
    /// `create_bead` error used to `return Err(..)`, which propagates all
    /// the way to `main()` and calls `std::process::exit(1)`, killing the
    /// whole daemon process before later candidates in the same fetch batch
    /// were ever visited).
    Errored { reason: String },
}

/// One verdict for one candidate, keyed on the same `external_ref` used
/// everywhere else in the daemon (`"<owner>/<repo>#<number>"`) so telemetry
/// consumers can `grep` a specific issue/PR number and always find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeOutcome {
    pub external_ref: String,
    pub verdict: IntakeVerdict,
}

/// jleechan-35y4 Stage A: resolve which repo a bead belongs to, in
/// precedence order:
///
/// 1. An explicit `target_repo: <owner>/<name>` field in the bead body.
/// 2. Else the `owner/repo` prefix of the bead's `external_ref`.
/// 3. Else `None` — legacy bead with no way to determine its repo;
///    callers fall back to `cfg.target_repo` via `BeadOverlay::repo`.
///
/// jleechan-dljf (issue #271): resolved results are validated with
/// `is_valid_owner_repo` — malformed strings return `None` rather than
/// propagating an unparseable string.
///
/// This is the single call site for body-field/external_ref repo-identity
/// parsing — do not re-implement this precedence elsewhere.
pub fn resolve_target_repo(body: &str, external_ref: Option<&str>) -> Option<String> {
    if let Some(explicit) = parse_target_repo_body_field(body) {
        if crate::config::is_valid_owner_repo(&explicit) {
            return Some(explicit);
        }
    }
    external_ref
        .and_then(parse_owner_repo_from_external_ref)
        .map(|(owner_repo, _issue)| owner_repo)
        .filter(|s| crate::config::is_valid_owner_repo(s))
}

/// Same `owner/repo#N` split as the private `parse_external_ref` helpers in
/// `adapters.rs`/`tick.rs` (each module keeps its own copy rather than
/// sharing a `pub(crate)` — matches the existing duplication pattern in this
/// crate). Strict: exactly one `#`, else `None` rather than guessing.
fn parse_owner_repo_from_external_ref(external_ref: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// Scan `body` line-by-line for `target_repo: <value>` (matching the
/// `existing_branch:`/`existing_pr:` grammar documented in
/// `.claude/skills/auto-factory/SKILL.md`). Returns `None` for a missing
/// field OR a present-but-blank value — a malformed field must not win over
/// a usable `external_ref` fallback.
fn parse_target_repo_body_field(body: &str) -> Option<String> {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("target_repo:") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
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
/// Returns `(created, outcomes)`: `created` is the ids of beads newly
/// created during this pass (empty if nothing new, preserved for existing
/// callers). `outcomes` carries exactly one `IntakeOutcome` for every
/// candidate that did NOT result in a newly-created bead (skips + errors) —
/// combined with one `INTAKE_BEAD_CREATED` telemetry event per `created`
/// entry, every candidate `scm.labeled_issues` returned resolves to exactly
/// one verdict (jleechan-eazj). Idempotent: running twice against an
/// unchanged SCM/tracker produces no new beads on the second run.
///
/// No single candidate's failure can prevent the rest of the batch from
/// being processed: every per-candidate operation that can fail
/// (`collaborator_permission`, `create_bead`) is caught and converted into
/// an `Errored` outcome rather than propagated with `?` — jleechan-eazj
/// traced issue #8171's total telemetry silence to exactly this pattern: an
/// earlier candidate's non-transient error used to abort this whole
/// function via `return Err(..)`, which propagates through `run_slow_tier`
/// to `main()` and calls `std::process::exit(1)`, so no candidate after the
/// failing one in the same fetch batch was ever visited, let alone logged.
pub fn normalize(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
) -> Result<(Vec<String>, Vec<IntakeOutcome>), DaemonError> {
    let issues = scm.labeled_issues(FACTORY_LABEL)?;
    if issues.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let known_refs = tracker.fetch_all_external_refs()?;

    let mut created = Vec::new();
    let mut outcomes = Vec::new();

    for issue in issues {
        // Idempotency: already-known external_ref -> skip silently, no create_bead call.
        if known_refs.contains(&issue.external_ref) {
            outcomes.push(IntakeOutcome {
                external_ref: issue.external_ref.clone(),
                verdict: IntakeVerdict::SkippedDuplicate,
            });
            continue;
        }

        // Write-tier authorization gate (spec §4.2.3): only Write/Admin may
        // trigger dispatch. Lower tiers are skipped, not errored — the skip
        // itself is the audit trail the caller records via telemetry, keyed
        // on issue.external_ref + issue.author_login. A failure to even
        // *determine* the permission tier (e.g. a transient GitHub API
        // error) is recorded as Errored for this candidate only — it must
        // not abort the rest of the batch.
        let permission = match scm.collaborator_permission(&issue.author_login) {
            Ok(p) => p,
            Err(e) => {
                outcomes.push(IntakeOutcome {
                    external_ref: issue.external_ref.clone(),
                    verdict: IntakeVerdict::Errored {
                        reason: e.to_string(),
                    },
                });
                continue;
            }
        };
        if !permission.is_write_tier() {
            outcomes.push(IntakeOutcome {
                external_ref: issue.external_ref.clone(),
                verdict: IntakeVerdict::SkippedIneligible {
                    precondition: format!("author_permission_below_write_tier:{permission:?}"),
                },
            });
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
                    outcomes.push(IntakeOutcome {
                        external_ref: issue.external_ref.clone(),
                        verdict: IntakeVerdict::SkippedDuplicate,
                    });
                    continue;
                }
                // jleechan-eazj: do NOT `return Err(e)` here — that used to
                // abort the whole `normalize` call (and, via `?` upstream,
                // the whole daemon process) on the first non-duplicate
                // `create_bead` failure, silently starving every subsequent
                // candidate in this fetch batch of any telemetry at all.
                // Record the real error and move on to the next candidate;
                // an unresolved issue is retried again next slow tick since
                // it never gets added to `known_refs`.
                outcomes.push(IntakeOutcome {
                    external_ref: issue.external_ref.clone(),
                    verdict: IntakeVerdict::Errored {
                        reason: e.to_string(),
                    },
                });
                continue;
            }
        };

        let comment_body = format!(
            "🤖 **[dark-factory]** Auto-factory has picked up this task. Created tracking bead `{}`. Spawning worker session...",
            bead_id
        );
        let _ = tracker.comment_external(&issue.external_ref, &comment_body);

        created.push(bead_id);
    }

    Ok((created, outcomes))
}

/// Normalize open PRs labeled `factory` into beads that should attach to the
/// existing PR/head branch rather than dispatching a fresh factory branch.
///
/// Returns `(intakes, outcomes)` with the same jleechan-eazj guarantee as
/// `normalize`: every PR `scm.labeled_prs` returns resolves to exactly one
/// verdict this tick — either an `ExistingPrIntake` entry (ADOPTED, reported
/// by the caller via EXISTING_PR_ADOPTED) or one `IntakeOutcome` here. No
/// single PR's error aborts the rest of the batch.
pub fn normalize_labeled_prs(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
) -> Result<(Vec<ExistingPrIntake>, Vec<IntakeOutcome>), DaemonError> {
    let prs = scm.labeled_prs(FACTORY_LABEL)?;
    if prs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let tracker_candidates = tracker.fetch_candidates()?;
    let known_refs = tracker.fetch_all_external_refs()?;
    let mut intakes = Vec::new();
    let mut outcomes = Vec::new();

    for pr in prs {
        if pr.head_ref_name.trim().is_empty() {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedIneligible {
                    precondition: "empty_head_ref_name".to_string(),
                },
            });
            continue;
        }

        if !same_repo_pr(&pr, cfg) {
            let comment_body = "🤖 **[dark-factory]** Escalation required: fork/cross-repository PR adoption is not supported in v1. Same-repo factory PRs can be verified automatically; fork remediation lands with bead `jleechan-tfs1`.";
            let _ = tracker.comment_external(&pr.external_ref, comment_body);
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedFork,
            });
            continue;
        }

        let permission = match scm.collaborator_permission(&pr.author_login) {
            Ok(p) => p,
            Err(e) => {
                outcomes.push(IntakeOutcome {
                    external_ref: pr.external_ref.clone(),
                    verdict: IntakeVerdict::Errored {
                        reason: e.to_string(),
                    },
                });
                continue;
            }
        };
        if !permission.is_write_tier() {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedIneligible {
                    precondition: format!("author_permission_below_write_tier:{permission:?}"),
                },
            });
            continue;
        }

        if let Some(bead) = tracker_candidates
            .iter()
            .find(|bead| bead.external_ref.as_deref() == Some(pr.external_ref.as_str()))
        {
            let target_repo =
                resolve_target_repo(&pr.body, Some(pr.external_ref.as_str()));
            intakes.push(ExistingPrIntake {
                bead_id: bead.id.clone(),
                pr_number: pr.number,
                head_ref_name: pr.head_ref_name,
                external_ref: pr.external_ref,
                target_repo,
                newly_created: false,
            });
            continue;
        }

        if known_refs.contains(&pr.external_ref) {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedDuplicate,
            });
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
                    outcomes.push(IntakeOutcome {
                        external_ref: pr.external_ref.clone(),
                        verdict: IntakeVerdict::SkippedDuplicate,
                    });
                    continue;
                }
                // jleechan-eazj: do NOT `return Err(e)` here — see the
                // matching comment in `normalize` above. One PR's
                // non-duplicate `create_bead` failure must not starve every
                // other candidate in this fetch batch of telemetry (or crash
                // the daemon process via the `?` upstream in `run_slow_tier`).
                outcomes.push(IntakeOutcome {
                    external_ref: pr.external_ref.clone(),
                    verdict: IntakeVerdict::Errored {
                        reason: e.to_string(),
                    },
                });
                continue;
            }
        };
        let comment_body = format!(
            "🤖 **[dark-factory]** Auto-factory has picked up this pull request. Created tracking bead `{}` and will verify the existing branch `{}`.",
            bead_id, pr.head_ref_name
        );
        let _ = tracker.comment_external(&pr.external_ref, &comment_body);
        let target_repo =
            resolve_target_repo(&pr.body, Some(pr.external_ref.as_str()));
        intakes.push(ExistingPrIntake {
            bead_id,
            pr_number: pr.number,
            head_ref_name: pr.head_ref_name,
            external_ref: pr.external_ref,
            target_repo,
            newly_created: true,
        });
    }

    Ok((intakes, outcomes))
}

#[cfg(test)]
mod tests {
    // Unit-level coverage for the pure permission-gate helper; the fake-backed
    // contract tests (idempotency, write-tier gate, mixed batch) live in
    // `daemon/tests/intake.rs` per Task 6 Step 1.
    use crate::tools::Permission;
    use super::resolve_target_repo;

    #[test]
    fn permission_write_tier_gate_matches_design_contract() {
        assert!(Permission::Write.is_write_tier());
        assert!(Permission::Admin.is_write_tier());
        assert!(!Permission::Read.is_write_tier());
        assert!(!Permission::Triage.is_write_tier());
        assert!(!Permission::None.is_write_tier());
    }

    // jleechan-35y4 Stage A: intake repo-resolution precedence. Three
    // branches, exactly matching the doc's precedence order — explicit body
    // field wins, then external_ref, then None (legacy/global).

    #[test]
    fn resolve_target_repo_prefers_explicit_body_field_over_external_ref() {
        let body = "Some description.\ntarget_repo: jleechanorg/dark-factory\nexisting_branch: fix/x\n";
        let got = resolve_target_repo(body, Some("jleechanorg/worldarchitect.ai#123"));
        assert_eq!(got.as_deref(), Some("jleechanorg/dark-factory"));
    }

    #[test]
    fn resolve_target_repo_falls_back_to_external_ref_prefix_when_no_body_field() {
        let body = "Just a plain description, no structured fields.";
        let got = resolve_target_repo(body, Some("jleechanorg/worldarchitect.ai#456"));
        assert_eq!(got.as_deref(), Some("jleechanorg/worldarchitect.ai"));
    }

    #[test]
    fn resolve_target_repo_is_none_when_neither_body_field_nor_external_ref_present() {
        let got = resolve_target_repo("Just a plain description.", None);
        assert_eq!(got, None);
    }

    #[test]
    fn resolve_target_repo_ignores_blank_target_repo_value() {
        // A malformed/empty `target_repo:` line must not win over a usable
        // external_ref fallback.
        let body = "target_repo:   \n";
        let got = resolve_target_repo(body, Some("jleechanorg/dark-factory#7"));
        assert_eq!(got.as_deref(), Some("jleechanorg/dark-factory"));
    }

    #[test]
    fn resolve_target_repo_ignores_malformed_external_ref() {
        // `external_ref` without exactly one '#' doesn't parse — falls
        // through to None rather than guessing.
        let got = resolve_target_repo("no structured fields here", Some("not-a-valid-ref"));
        assert_eq!(got, None);
    }
}
