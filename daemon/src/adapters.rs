use crate::errors::{DaemonError, SpawnBatchCleanupFailure};
use crate::tools::{run_tool, run_tool_in_dir, Bead, Issue, LabeledPr, Llm, Permission, PrHeadBranch, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs, WorktreeHeadAncestry};
use std::process::{Command, Stdio};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// jleechan-9sl1 test-isolation fix: a single process-wide lock shared by
/// EVERY `#[cfg(test)]` module in this file that mutates the global `PATH`
/// (and related) env vars to inject a fake `gh`/`codex` binary for hermetic
/// subprocess testing (`chain_llm_fallback_argv_tests`,
/// `pr_snapshot_checks_fetch_failure_tests`, `cli_vcs_gh_tests`). Those
/// modules used to each define their OWN independent `ENV_LOCK` static,
/// which only serialized tests WITHIN a module -- two tests in DIFFERENT
/// modules could still mutate `PATH` concurrently under `cargo test`'s
/// default parallel execution, since `Command::new("gh")`/`"codex"` PATH
/// resolution and the temp-dir cleanup race is a single shared global
/// resource with no per-module partition. That cross-module race is exactly
/// what caused `pr_snapshot_checks_fetch_failure_tests::
/// genuinely_empty_checks_via_fallback_still_reports_ci_pending` to
/// intermittently fail with "No such file or directory" pointing at a
/// DIFFERENT module's already-cleaned-up temp shim dir once a third
/// PATH-mutating module (`cli_vcs_gh_tests`) was added. Every module below
/// must call `gh_env_test_lock()` (directly or via a thin per-module
/// `env_lock()` wrapper) instead of defining its own `ENV_LOCK` -- do not
/// reintroduce a module-local lock for PATH/env mutation in this file.
#[cfg(test)]
static GH_ENV_TEST_LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
#[cfg(test)]
fn gh_env_test_lock() -> &'static Mutex<()> {
    GH_ENV_TEST_LOCK.get_or_init(|| Mutex::new(()))
}


pub struct CliTracker;

impl Tracker for CliTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        // `--limit 0` = unlimited: the default page size (50) silently truncates
        // the queue once open beads exceed one page (jleechan-v09l).
        let out = run_tool(
            "br",
            &["list", "--status", "open", "--label", "factory", "--json", "--limit", "0"],
            30,
        )?;
        let json_start = out.find('{').unwrap_or(0);
        #[derive(serde::Deserialize)]
        struct BrListOutput {
            issues: Vec<BrIssue>,
            #[serde(default)]
            has_more: bool,
        }
        #[derive(serde::Deserialize)]
        struct BrIssue {
            id: String,
            title: String,
            description: Option<String>,
            // jleechan-0hqx (issue #338): operator-authored per-attempt
            // guidance, set via `br update --notes`. Surfaced into the coder
            // prompt as the higher-priority-than-description
            // OPERATOR GUIDANCE section so attempt rN coders stop
            // re-litigating scope the operator already settled on requeue.
            // `Option` because beads predating this field, or with no notes
            // set, omit it from the `br list --json` payload.
            notes: Option<String>,
            external_ref: Option<String>,
        }
        let data: BrListOutput = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse br list JSON: {e}"))
        })?;
        // Fail closed on truncated output: a partial candidate queue silently
        // starves beads beyond page one (jleechan-v09l).
        if data.has_more {
            return Err(DaemonError::Parse(
                "br list output truncated (has_more=true); refusing partial candidate queue — pass --limit 0".to_string(),
            ));
        }
        let file_tree_summary = crate::tools::summarize_file_tree(std::path::Path::new("."), 100);
        let beads = data.issues.into_iter().map(|issue| Bead {
            id: issue.id,
            title: issue.title,
            description: issue.description.unwrap_or_default(),
            notes: issue.notes.unwrap_or_default(),
            file_tree_summary: file_tree_summary.clone(),
            external_ref: issue.external_ref,
        }).collect();
        Ok(beads)
    }

    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
        // `br list --status all` returns zero rows; merge open + closed so closed beads
        // (e.g. jleechan-9byt.5 → worldarchitect.ai#8171) block duplicate create_bead.
        // `--limit 0` = unlimited. Without it the default page size (50) hides
        // refs beyond page one from dedup, so the daemon re-creates a bead for an
        // already-tracked issue and the resulting br rc=7 kills the whole tick
        // (jleechan-v09l: 130 open / 125 closed beads vs 50-row pages).
        // jleechan-u4gb: this bulk read is a point-in-time snapshot and can
        // race with a concurrent `br create` (or reflect pagination
        // skew/staleness in `br list` itself) — see
        // `DaemonError::duplicate_external_ref_bead_id` and its call sites
        // in `intake.rs` for the authoritative write-time fallback that
        // makes create_bead idempotent even when this snapshot is stale.
        let mut refs = parse_external_refs_from_br_list(&run_tool(
            "br",
            &["list", "--status", "open", "--json", "--limit", "0"],
            30,
        )?)?;
        refs.extend(parse_external_refs_from_br_list(&run_tool(
            "br",
            &["list", "--status", "closed", "--json", "--limit", "0"],
            30,
        )?)?);
        Ok(refs)
    }

    fn create_bead(
        &self,
        title: &str,
        body: &str,
        external_ref: &str,
    ) -> Result<String, DaemonError> {
        let out = run_tool(
            "br",
            &[
                "create",
                "--title",
                title,
                "--description",
                body,
                "--external-ref",
                external_ref,
                "--labels",
                "factory",
                "--silent",
            ],
            30,
        )?;
        Ok(out.trim().to_string())
    }

    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError> {
        if let Some((repo, issue)) = canonicalize_external_ref_for_comment(external_ref) {
            run_tool("gh", &["issue", "comment", &issue, "--repo", &repo, "--body", body], 30)?;
            Ok(())
        } else {
            Err(DaemonError::Parse(format!(
                "invalid external_ref format for comment: {external_ref}"
            )))
        }
    }
}

#[cfg(test)]
mod cli_tracker_br_db_tests {
    use super::{gh_env_test_lock, CliTracker};
    use crate::tools::Tracker;

    #[test]
    #[cfg(unix)]
    fn cli_tracker_passes_configured_db_to_br_read_and_write_calls() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "dark_factory_cli_tracker_br_db_{}_{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("br.log");
        let db = root.join("state/beads.db");
        let br = root.join("br");
        std::fs::write(
            &br,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$DARK_FACTORY_BR_LOG"
if [ "$1" = "--db" ]; then shift 2; fi
case "${1:-}" in
  list) printf '{"issues":[],"has_more":false}\n' ;;
  create) printf 'bead-from-fake-br\n' ;;
  *) exit 64 ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&br, std::fs::Permissions::from_mode(0o755)).unwrap();

        let prior_path = std::env::var_os("PATH");
        let prior_db = std::env::var_os("DARK_FACTORY_BR_DB");
        let prior_log = std::env::var_os("DARK_FACTORY_BR_LOG");
        unsafe {
            std::env::set_var(
                "PATH",
                format!("{}:{}", root.display(), prior_path.as_deref().unwrap_or_default().to_string_lossy()),
            );
            std::env::set_var("DARK_FACTORY_BR_DB", &db);
            std::env::set_var("DARK_FACTORY_BR_LOG", &log);
        }

        let tracker = CliTracker;
        let reads = tracker.fetch_candidates();
        let created = tracker.create_bead("test", "body", "owner/repo#1");

        unsafe {
            match prior_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
            match prior_db {
                Some(value) => std::env::set_var("DARK_FACTORY_BR_DB", value),
                None => std::env::remove_var("DARK_FACTORY_BR_DB"),
            }
            match prior_log {
                Some(value) => std::env::set_var("DARK_FACTORY_BR_LOG", value),
                None => std::env::remove_var("DARK_FACTORY_BR_LOG"),
            }
        }

        assert!(reads.unwrap().is_empty());
        assert_eq!(created.unwrap(), "bead-from-fake-br");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().collect::<Vec<_>>(),
            vec![
                format!("--db {} list --status open --label factory --json --limit 0", db.display()),
                format!("--db {} create --title test --description body --external-ref owner/repo#1 --labels factory --silent", db.display()),
            ]
        );
        std::fs::remove_dir_all(root).ok();
    }
}

/// Parse `external_ref` values from `br list --json` output.
pub(crate) fn parse_external_refs_from_br_list(
    out: &str,
) -> Result<std::collections::HashSet<String>, DaemonError> {
    let json_start = out.find('{').unwrap_or(0);
    #[derive(serde::Deserialize)]
    struct BrListOutput {
        issues: Vec<BrIssue>,
        #[serde(default)]
        has_more: bool,
    }
    #[derive(serde::Deserialize)]
    struct BrIssue {
        external_ref: Option<String>,
    }
    let data: BrListOutput = serde_json::from_str(&out[json_start..]).map_err(|e| {
        DaemonError::Parse(format!("failed to parse br list JSON: {e}"))
    })?;
    // Fail closed on truncated output: a partial dedup set means the daemon
    // re-creates beads for already-tracked issues (jleechan-v09l).
    if data.has_more {
        return Err(DaemonError::Parse(
            "br list output truncated (has_more=true); refusing partial external-ref dedup — pass --limit 0".to_string(),
        ));
    }
    Ok(data
        .issues
        .into_iter()
        .filter_map(|issue| issue.external_ref)
        .collect())
}

fn parse_external_ref(external_ref: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        let url_parts: Vec<&str> = external_ref
            .strip_prefix("https://github.com/")?
            .split('/')
            .collect();
        match url_parts.as_slice() {
            [owner, repo, kind, number] if matches!(*kind, "pull" | "issues") => {
                Some((format!("{owner}/{repo}"), number.to_string()))
            }
            _ => None,
        }
    }
}

/// jtg8: parse GitHub's RFC3339 timestamp into epoch seconds. Used by the
/// adoption-probe cache to key per-PR probes on `updated_at`. Returns `None`
/// for any unparseable input — the daemon treats `None` as "uncacheable"
/// (preserves pre-fix behavior for missing/malformed upstream fields).
pub(crate) fn parse_rfc3339_to_epoch_secs(s: &str) -> Option<u64> {
    // Strip fractional seconds (we only care about second precision for the
    // cache key — anything sub-second doesn't change adoption decisions) and
    // the trailing `Z` so the date/time parser below sees plain numbers.
    let trimmed = s.trim();
    let no_frac = trimmed.split('.').next().unwrap_or(trimmed);
    let no_tz = no_frac.trim_end_matches('Z');
    let parts: Vec<&str> = no_tz.split(['-', ':', 'T']).collect();
    if parts.len() != 6 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;
    let hour: i64 = parts[3].parse().ok()?;
    let minute: i64 = parts[4].parse().ok()?;
    let second: i64 = parts[5].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days-from-civil algorithm — pure integer math, no
    // chrono dependency. Converts a Gregorian date into days since the
    // Unix epoch (1970-01-01). Equivalent to chrono's NaiveDate conversion
    // but avoids the dep.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let m_idx = m as u64;
    let doy = (153 * m_idx + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    if days < 0 {
        return None;
    }
    Some((days as u64) * 86_400 + (hour as u64) * 3_600 + (minute as u64) * 60 + second as u64)
}

/// jleechan-mdgr defense-in-depth: recovers the real `<repo>#<pr>` comment
/// target from the "double external_ref suffix" corruption, where a bead
/// that already had a valid `parse_external_ref`-shaped ref ALSO got a
/// `#local-<bead-id>` disambiguation suffix appended on top of it (see
/// `daemon/scripts/backfill_external_ref.py`'s `#local-<id>` convention,
/// which is only ever supposed to apply to a base ref with no `#` of its
/// own — e.g. a bare GitHub PR URL). The 2026-07-11T00:05:15Z incident
/// (bead jleechan-8dyu, `jleechanorg/worldarchitect.ai#7888#local-8dyu`)
/// shows the writer mixing that convention with the SHORT canonical
/// `owner/repo#N` base, which already contains a `#`, producing 3
/// `#`-delimited segments and a permanent `comment_external` parse failure.
///
/// `parse_external_ref` intentionally stays strict for short-form refs
/// (exactly one `#`) while also accepting exact GitHub pull/issue URLs, so
/// this corrupted shape is still detected as invalid on its own. This helper
/// is the single call site (escalation comment posting) that recognizes the
/// specific `<repo>#<pr>#local-<token>` shape and strips the trailing
/// disambiguation suffix to recover the real target, so already-corrupted
/// stored data (which this PR does NOT bulk-repair) can still have its
/// escalation comment posted. Any other malformed shape — including bare
/// `local-<id>` refs — is left untouched.
fn canonicalize_external_ref_for_comment(external_ref: &str) -> Option<(String, String)> {
    if let Some(parsed) = parse_external_ref(external_ref) {
        return Some(parsed);
    }
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 3 && parts[2].starts_with("local-") {
        return parse_external_ref(&format!("{}#{}", parts[0], parts[1]));
    }
    None
}

fn unresolved_thread_count_from_gql(gql_out: &str) -> Result<u32, DaemonError> {
    #[derive(serde::Deserialize)]
    struct GhGqlResponse {
        data: GhGqlData,
    }
    #[derive(serde::Deserialize)]
    struct GhGqlData {
        repository: GhGqlRepository,
    }
    #[derive(serde::Deserialize)]
    struct GhGqlRepository {
        #[serde(rename = "pullRequest")]
        pull_request: Option<GhGqlPullRequest>,
    }
    #[derive(serde::Deserialize)]
    struct GhGqlPullRequest {
        #[serde(rename = "reviewThreads")]
        review_threads: GhGqlReviewThreads,
    }
    #[derive(serde::Deserialize)]
    struct GhGqlReviewThreads {
        nodes: Vec<GhGqlNode>,
    }
    #[derive(serde::Deserialize)]
    struct GhGqlNode {
        #[serde(rename = "isResolved")]
        is_resolved: bool,
    }

    let json_start = gql_out.find('{').unwrap_or(0);
    let gql: GhGqlResponse = serde_json::from_str(&gql_out[json_start..]).map_err(|e| {
        DaemonError::Parse(format!("failed to parse gh graphql JSON: {e}"))
    })?;
    let pr_data = gql.data.repository.pull_request.ok_or_else(|| {
        DaemonError::Parse("gh graphql response omitted pullRequest".into())
    })?;
    Ok(pr_data
        .review_threads
        .nodes
        .iter()
        .filter(|n| !n.is_resolved)
        .count() as u32)
}

pub struct CliScm {
    pub repo: String,
    labeled_issues_cache: Mutex<HashMap<String, (Vec<Issue>, Instant)>>,
    permission_cache: Mutex<HashMap<String, (Permission, Instant)>>,
    pr_snapshot_cache: Mutex<HashMap<u64, (PrSnapshot, Instant)>>,
    branch_commit_cache: Mutex<HashMap<String, (Option<u64>, Instant)>>,
}

impl CliScm {
    pub fn new(repo: String) -> Self {
        Self {
            repo,
            labeled_issues_cache: Mutex::new(HashMap::new()),
            permission_cache: Mutex::new(HashMap::new()),
            pr_snapshot_cache: Mutex::new(HashMap::new()),
            branch_commit_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Return a handle targeting a different repo string (bead jleechan-35y4
    /// Stage B — see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`'s "keep
    /// traits, add `with_repo` constructor" note). Deliberately a fresh
    /// `Self::new(repo)` rather than cloning this instance's in-memory TTL
    /// caches: those caches (`pr_snapshot_cache` keyed by bare PR number,
    /// `labeled_issues_cache`/`branch_commit_cache` keyed by label/branch
    /// name) carry no repo qualifier, so reusing them for a different repo
    /// could return another repo's cached PR/issue/branch data under a
    /// colliding key. Construction is cheap (empty `HashMap`s), so a fresh
    /// instance is both simpler and correct.
    pub fn with_repo(&self, repo: &str) -> Self {
        Self::new(repo.to_string())
    }

    fn labeled_prs_via_rest(&self, label: &str, gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
        let out = run_tool(
            "gh",
            &[
                "api",
                // REST default per_page is 30; 100 is the API maximum (jleechan-v09l
                // truncation class).
                &format!(
                    "repos/{}/issues?labels={label}&state=open&per_page=100",
                    self.repo
                ),
            ],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct RestIssue {
            number: u64,
            title: String,
            body: Option<String>,
            user: Option<RestUser>,
            pull_request: Option<serde_json::Value>,
        }
        #[derive(serde::Deserialize)]
        struct RestUser {
            login: String,
        }

        let json_start = out.find('[').unwrap_or(0);
        let issues: Vec<RestIssue> = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh labeled PR REST list: {e}"))
        })?;
        // PR #629 follow-up fix (codex P2 "Enforce the call cap within each
        // repository scan"): the sweep-wide gh-call budget was only checked
        // BETWEEN repos, in `normalize_labeled_prs_outcome`'s outer loop
        // (daemon/src/intake.rs) -- this single call can perform the list
        // request above plus up to 100 per-PR `pulls/{n}` requests before
        // ever returning, so a sweep starting well below the cap could
        // still blow through it here. Collecting into `pr_issues` first
        // (instead of filtering the iterator inline) lets the loop below
        // report exactly how many labeled PRs were left unqueried when the
        // budget runs out mid-scan.
        let pr_issues: Vec<RestIssue> = issues
            .into_iter()
            .filter(|issue| issue.pull_request.is_some())
            .collect();
        let total = pr_issues.len();
        let mut prs = Vec::new();
        for (idx, issue) in pr_issues.into_iter().enumerate() {
            if Self::rest_fallback_budget_exhausted(*gh_calls) {
                eprintln!(
                    "auto-factory daemon: WARNING intake REST fallback for {} stopping mid-repo-scan at gh_call_count={} (cap={}); {} of {} labeled PR(s) not queried this tick",
                    self.repo,
                    *gh_calls,
                    crate::intake::MAX_INTAKE_SWEEP_GH_CALLS,
                    total - idx,
                    total
                );
                break;
            }
            // jtg8-r5: count each per-PR `pulls/{n}` call so the
            // slow-tier `gh_call_count` warning sees the real O(N) burn
            // (the r4 implementation only counted the list query, so the
            // warning never fired under the fallback path — codex P2).
            *gh_calls += 1;
            let pull_out = run_tool(
                "gh",
                &[
                    "api",
                    // jtg8-r5: now that `parse_rest_pull_payload` deserializes
                    // `head.sha` and `updated_at` directly, the `?fields=`
                    // filter is informational only — GitHub returns the full
                    // pull object either way, and the parser is the canonical
                    // surface for what fields we extract.
                    &format!(
                        "repos/{}/pulls/{}?fields=number,head,updated_at",
                        self.repo, issue.number
                    ),
                ],
                30,
            )?;
            let json_start = pull_out.find('{').unwrap_or(0);
            // jtg8-r5: delegate to the canonical parser. The parser
            // reads `head.sha` directly (GitHub's real REST shape), so
            // REST-fallback PRs now carry `head_sha: Some(...)` and the
            // adoption-probe cache can short-circuit per-PR probes on
            // them just like the primary `gh pr list` path. r4 looked
            // for a non-existent top-level `head_sha` field, which is
            // the bug codex flagged in P2 review.
            let (head_sha_opt, updated_at_epoch_opt, head_ref_name, head_repo_full_name, head_repo_owner_login, is_cross_repository) =
                Self::parse_rest_pull_payload(&self.repo, &pull_out[json_start..])?;
            prs.push(LabeledPr {
                number: issue.number,
                title: issue.title,
                body: issue.body.unwrap_or_default(),
                author_login: issue.user.map(|u| u.login).unwrap_or_default(),
                external_ref: format!("{}#{}", self.repo, issue.number),
                head_ref_name,
                is_cross_repository,
                head_repo_full_name,
                head_repo_owner_login,
                head_sha: head_sha_opt,
                updated_at_epoch: updated_at_epoch_opt,
            });
        }
        Ok(prs)
    }

    /// PR #629 follow-up fix (codex P2 "Enforce the call cap within each
    /// repository scan"): pure boundary check so the REST-fallback per-PR
    /// pulls loop's cap enforcement is unit-testable without shelling out
    /// to a real/faked `gh` binary — mirrors this file's own
    /// `parse_rest_pull_payload` precedent (extracted specifically so its
    /// logic is directly testable). `gh_calls` is the SAME cumulative,
    /// sweep-wide counter threaded through `normalize_labeled_prs_outcome`
    /// (`daemon/src/intake.rs`), not a per-repo counter, so this correctly
    /// bounds the TOTAL sweep budget rather than giving each repo its own
    /// separate allowance.
    pub(crate) fn rest_fallback_budget_exhausted(gh_calls: u32) -> bool {
        gh_calls >= crate::intake::MAX_INTAKE_SWEEP_GH_CALLS
    }

    /// jtg8-r5 (P2 review "Populate the REST fallback SHA from head.sha"):
    /// GitHub's `/pulls/{n}` REST response keeps the head SHA nested under
    /// `head.sha`, NOT at a top-level `head_sha` field. The r4
    /// `RestPullExt` struct looked for a top-level field and so every
    /// REST-fallback PR came back with `head_sha = None`, making the
    /// adoption-probe cache treat it as uncacheable — defeating the r4
    /// fix on the REST fallback path. This parser is the canonical
    /// deserializer for a single `/pulls/{n}` payload; the production
    /// `labeled_prs_via_rest` loop delegates here so the unit tests can
    /// exercise it directly without shelling out to a fake `gh`.
    #[allow(clippy::type_complexity)]
    pub(crate) fn parse_rest_pull_payload(
        repo: &str,
        pull_json: &str,
    ) -> Result<
        (
            Option<String>, // head_sha
            Option<u64>,    // updated_at_epoch
            String,         // head_ref_name
            Option<String>, // head_repo_full_name
            Option<String>, // head_repo_owner_login
            bool,           // is_cross_repository
        ),
        DaemonError,
    > {
        #[derive(serde::Deserialize)]
        struct RestPull {
            head: RestHead,
            #[serde(default)]
            updated_at: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct RestHead {
            #[serde(rename = "ref")]
            ref_name: String,
            sha: Option<String>,
            repo: Option<RestRepo>,
        }
        #[derive(serde::Deserialize)]
        struct RestRepo {
            full_name: Option<String>,
            owner: Option<RestUser>,
        }
        #[derive(serde::Deserialize)]
        struct RestUser {
            login: String,
        }
        let pull: RestPull = serde_json::from_str(pull_json).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pull REST response: {e}"))
        })?;
        let head_repo_full_name = pull
            .head
            .repo
            .as_ref()
            .and_then(|repo| repo.full_name.clone());
        let head_repo_owner_login = pull
            .head
            .repo
            .as_ref()
            .and_then(|repo| repo.owner.as_ref().map(|owner| owner.login.clone()));
        let target_owner = repo.split('/').next().unwrap_or_default();
        let is_cross_repository = head_repo_full_name
            .as_ref()
            .map(|name| !name.eq_ignore_ascii_case(repo))
            .or_else(|| {
                head_repo_owner_login
                    .as_ref()
                    .map(|owner| !owner.eq_ignore_ascii_case(target_owner))
            })
            .unwrap_or(false);
        Ok((
            pull.head.sha,
            pull.updated_at
                .as_deref()
                .and_then(parse_rfc3339_to_epoch_secs),
            pull.head.ref_name,
            head_repo_full_name,
            head_repo_owner_login,
            is_cross_repository,
        ))
    }
}

#[cfg(test)]
mod rest_fallback_budget_exhausted_tests {
    use super::CliScm;

    /// PR #629 follow-up fix (codex P2): the pure boundary check must stay
    /// permissive strictly BELOW the sweep-wide cap and trip AT it (not
    /// one-past-it) — `labeled_prs_via_rest`'s loop calls this BEFORE
    /// incrementing `gh_calls` for the call it is about to make, so
    /// tripping exactly at the cap (rather than one over) is what prevents
    /// the (cap+1)-th call from ever being issued.
    #[test]
    fn permissive_below_cap_trips_at_cap() {
        assert!(
            !CliScm::rest_fallback_budget_exhausted(crate::intake::MAX_INTAKE_SWEEP_GH_CALLS - 1),
            "one call below the cap must still be allowed to proceed"
        );
        assert!(
            CliScm::rest_fallback_budget_exhausted(crate::intake::MAX_INTAKE_SWEEP_GH_CALLS),
            "reaching the cap exactly must stop further calls (matches the outer \
             sweep loop's own `>=` check in normalize_labeled_prs_outcome)"
        );
        assert!(
            CliScm::rest_fallback_budget_exhausted(crate::intake::MAX_INTAKE_SWEEP_GH_CALLS + 1),
            "already over the cap (e.g. from a prior repo's overshoot) must also stop"
        );
    }

    #[test]
    fn zero_calls_never_exhausted() {
        assert!(
            !CliScm::rest_fallback_budget_exhausted(0),
            "a fresh sweep with zero calls made so far must never report exhausted"
        );
    }
}

#[cfg(test)]
mod parse_rest_pull_payload_tests {
    use super::CliScm;

    /// jtg8-r5 red test #1: the canonical REST payload shape (head SHA
    /// nested under `head.sha`) must produce a populated `head_sha`. The
    /// r4 code looked for a non-existent top-level `head_sha` field and
    /// so every REST-fallback PR came back with `head_sha = None`.
    #[test]
    fn parse_rest_pull_payload_extracts_head_sha_from_head_dot_sha() {
        let payload = serde_json::json!({
            "number": 42,
            "head": {
                "ref": "feature/jtg8-r5",
                "sha": "abc123def456abc123def456abc123def456abc1",
                "repo": {
                    "full_name": "jleechanorg/dark-factory",
                    "owner": { "login": "jleechanorg" },
                },
            },
            "updated_at": "2026-07-22T18:00:00Z",
        })
        .to_string();
        let (head_sha, updated_at_epoch, head_ref_name, head_repo_full_name, head_repo_owner_login, is_cross_repository) =
            CliScm::parse_rest_pull_payload("jleechanorg/dark-factory", &payload).unwrap();
        assert_eq!(
            head_sha.as_deref(),
            Some("abc123def456abc123def456abc123def456abc1"),
            "head_sha must be populated from head.sha, not from a non-existent top-level field"
        );
        assert_eq!(
            head_ref_name, "feature/jtg8-r5",
            "head_ref_name must be populated from head.ref"
        );
        assert!(
            updated_at_epoch.is_some(),
            "updated_at must parse to an epoch second (got None for 2026-07-22T18:00:00Z)"
        );
        assert_eq!(
            head_repo_full_name.as_deref(),
            Some("jleechanorg/dark-factory"),
            "head_repo_full_name must be populated from head.repo.full_name"
        );
        assert_eq!(
            head_repo_owner_login.as_deref(),
            Some("jleechanorg"),
            "head_repo_owner_login must be populated from head.repo.owner.login"
        );
        assert!(
            !is_cross_repository,
            "same-repo PR must report is_cross_repository=false"
        );
    }

    /// A fork PR (head lives in a different repo) must report
    /// `is_cross_repository=true` and still populate head_sha / updated_at.
    #[test]
    fn parse_rest_pull_payload_detects_cross_repo_pr() {
        let payload = serde_json::json!({
            "number": 99,
            "head": {
                "ref": "contributor-patch",
                "sha": "deadbeef00000000deadbeef00000000deadbeef",
                "repo": {
                    "full_name": "contributor/dark-factory-fork",
                    "owner": { "login": "contributor" },
                },
            },
            "updated_at": "2026-07-22T19:30:00Z",
        })
        .to_string();
        let (head_sha, _updated_at_epoch, _head_ref_name, _head_repo_full_name, _head_repo_owner_login, is_cross_repository) =
            CliScm::parse_rest_pull_payload("jleechanorg/dark-factory", &payload).unwrap();
        assert_eq!(
            head_sha.as_deref(),
            Some("deadbeef00000000deadbeef00000000deadbeef"),
            "fork PR head_sha must still be populated (cache key still populated for fork's branch SHA)"
        );
        assert!(
            is_cross_repository,
            "cross-repo PR must report is_cross_repository=true so the daemon's same-repo guard fires"
        );
    }

    /// Missing `updated_at` (older gh versions) must NOT fail parsing —
    /// `head_sha` still populates and `updated_at_epoch` is `None`,
    /// matching the r4 `#[serde(default)]` tolerance pattern.
    #[test]
    fn parse_rest_pull_payload_tolerates_missing_updated_at() {
        let payload = serde_json::json!({
            "number": 1,
            "head": {
                "ref": "feat/x",
                "sha": "f00d0000000000000000000000000000000000f0",
                "repo": {
                    "full_name": "jleechanorg/dark-factory",
                    "owner": { "login": "jleechanorg" },
                },
            },
        })
        .to_string();
        let (head_sha, updated_at_epoch, _head_ref_name, _head_repo_full_name, _head_repo_owner_login, _is_cross_repository) =
            CliScm::parse_rest_pull_payload("jleechanorg/dark-factory", &payload).unwrap();
        assert_eq!(
            head_sha.as_deref(),
            Some("f00d0000000000000000000000000000000000f0"),
            "missing updated_at must not break head_sha parsing"
        );
        assert!(
            updated_at_epoch.is_none(),
            "missing updated_at must produce updated_at_epoch=None (uncacheable PR)"
        );
    }
}


impl Scm for CliScm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError> {
        let offline_path = std::path::Path::new(".beads/offline").join(format!("labeled_issues_{}.json", label));
        if offline_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&offline_path) {
                #[derive(serde::Deserialize)]
                struct OfflineIssue {
                    number: u64,
                    title: String,
                    body: String,
                    author_login: String,
                }
                if let Ok(issues_raw) = serde_json::from_str::<Vec<OfflineIssue>>(&raw) {
                    let issues = issues_raw.into_iter().map(|issue| Issue {
                        number: issue.number,
                        title: issue.title,
                        body: issue.body,
                        author_login: issue.author_login,
                        external_ref: format!("{}#{}", self.repo, issue.number),
                    }).collect();
                    return Ok(issues);
                }
            }
        }
        {
            let cache = self.labeled_issues_cache.lock().unwrap();
            if let Some((val, timestamp)) = cache.get(label) {
                if timestamp.elapsed() < Duration::from_secs(60) {
                    return Ok(val.clone());
                }
            }
        }
        let out_issues = match run_tool(
            "gh",
            &[
                "issue",
                "list",
                "--repo",
                &self.repo,
                "--label",
                label,
                "--state",
                "open",
                // gh defaults to 30 rows; same truncation class as jleechan-v09l.
                "--limit",
                "1000",
                "--json",
                "number,title,body,author",
            ],
            30,
        ) {
            Ok(out) => out,
            Err(_) => {
                run_tool(
                    "gh",
                    &[
                        "api",
                        // REST default per_page is 30; 100 is the API maximum.
                        &format!(
                            "repos/{}/issues?labels={label}&state=open&per_page=100",
                            self.repo
                        ),
                    ],
                    30,
                )?
            }
        };
        #[derive(serde::Deserialize)]
        struct GhIssue {
            number: u64,
            title: String,
            body: Option<String>,
            author: Option<GhAuthor>, // from GraphQL
            user: Option<GhAuthor>,   // from REST
        }
        #[derive(serde::Deserialize)]
        struct GhAuthor {
            login: String,
        }
        let json_start_issues = out_issues.find('[').unwrap_or(0);
        let gh_issues: Vec<GhIssue> = serde_json::from_str(&out_issues[json_start_issues..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh issue list: {e}"))
        })?;
        let mut issues: Vec<Issue> = Vec::new();
        for item in gh_issues {
            if !issues.iter().any(|i| i.number == item.number) {
                let author_login = item.author.as_ref().or(item.user.as_ref())
                    .map(|a| a.login.clone())
                    .unwrap_or_default();
                issues.push(Issue {
                    number: item.number,
                    title: item.title,
                    body: item.body.unwrap_or_default(),
                    author_login,
                    external_ref: format!("{}#{}", self.repo, item.number),
                });
            }
        }
        {
            let mut cache = self.labeled_issues_cache.lock().unwrap();
            cache.insert(label.to_string(), (issues.clone(), Instant::now()));
        }
        Ok(issues)
    }

    fn labeled_prs(&self, label: &str, gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
        // jtg8-r5: count the list query regardless of which branch we end
        // up on (primary `gh pr list` or REST fallback). The fallback's
        // per-PR pulls/{n} calls are counted inside `labeled_prs_via_rest`.
        *gh_calls += 1;
        let out = match run_tool(
            "gh",
            &[
                "pr",
                "list",
                "--repo",
                &self.repo,
                "--label",
                label,
                "--state",
                "open",
                // gh defaults to 30 rows; same truncation class as jleechan-v09l.
                "--limit",
                "1000",
                "--json",
                // jtg8: also fetch headRefOid + updatedAt so the adoption-probe
                // cache can short-circuit per-PR probes on unchanged keys.
                "number,title,body,author,headRefName,isCrossRepository,headRepositoryOwner,headRefOid,updatedAt",
            ],
            30,
        ) {
            Ok(out) => out,
            Err(_) => return self.labeled_prs_via_rest(label, gh_calls),
        };
        #[derive(serde::Deserialize)]
        struct GhPr {
            number: u64,
            title: String,
            body: Option<String>,
            author: Option<GhAuthor>,
            #[serde(rename = "headRefName")]
            head_ref_name: String,
            #[serde(rename = "isCrossRepository")]
            is_cross_repository: bool,
            #[serde(rename = "headRepositoryOwner")]
            head_repository_owner: Option<GhAuthor>,
            // jtg8: cache key fields. `headRefOid` is the head commit SHA;
            // `updatedAt` is the PR's last-modified timestamp.
            #[serde(rename = "headRefOid")]
            head_ref_oid: Option<String>,
            #[serde(rename = "updatedAt")]
            updated_at: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct GhAuthor {
            login: String,
        }
        let json_start = out.find('[').unwrap_or(0);
        let prs: Vec<GhPr> = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr list: {e}"))
        })?;
        Ok(prs
            .into_iter()
            .map(|pr| {
                // jtg8: parse GitHub's RFC3339 updatedAt into epoch seconds
                // for the cache key. Returns None on parse failure (the
                // daemon treats None as "uncacheable" and probes fresh).
                let updated_at_epoch = pr
                    .updated_at
                    .as_deref()
                    .and_then(parse_rfc3339_to_epoch_secs);
                LabeledPr {
                    number: pr.number,
                    title: pr.title,
                    body: pr.body.unwrap_or_default(),
                    author_login: pr.author.map(|a| a.login).unwrap_or_default(),
                    external_ref: format!("{}#{}", self.repo, pr.number),
                    head_ref_name: pr.head_ref_name,
                    is_cross_repository: pr.is_cross_repository,
                    head_repo_full_name: None,
                    head_repo_owner_login: pr.head_repository_owner.map(|owner| owner.login),
                    head_sha: pr.head_ref_oid,
                    updated_at_epoch,
                }
            })
            .collect())
    }

    fn labeled_prs_for_repo(
        &self,
        repo: &str,
        label: &str,
        gh_calls: &mut u32,
    ) -> Result<Vec<LabeledPr>, DaemonError> {
        self.with_repo(repo).labeled_prs(label, gh_calls)
    }

    fn collaborator_permission_for_repo(
        &self,
        repo: &str,
        login: &str,
    ) -> Result<Permission, DaemonError> {
        self.with_repo(repo).collaborator_permission(login)
    }

    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError> {
        let offline_path = std::path::Path::new(".beads/offline").join(format!("permission_{}.json", login));
        if offline_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&offline_path) {
                #[derive(serde::Deserialize)]
                struct OfflinePermission {
                    permission: String,
                }
                if let Ok(perm_raw) = serde_json::from_str::<OfflinePermission>(&raw) {
                    let perm = match perm_raw.permission.as_str() {
                        "admin" => Permission::Admin,
                        "write" => Permission::Write,
                        "triage" => Permission::Triage,
                        "read" => Permission::Read,
                        _ => Permission::None,
                    };
                    return Ok(perm);
                }
            }
        }
        {
            let cache = self.permission_cache.lock().unwrap();
            if let Some((val, timestamp)) = cache.get(login) {
                if timestamp.elapsed() < Duration::from_secs(300) {
                    return Ok(*val);
                }
            }
        }
        let path = format!("repos/{}/collaborators/{}/permission", self.repo, login);
        let out = run_tool("gh", &["api", &path], 30)?;
        #[derive(serde::Deserialize)]
        struct GhPermissionResponse {
            permission: String,
        }
        let json_start = out.find('{').unwrap_or(0);
        let resp: GhPermissionResponse = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse collaborator permission response: {e}"))
        })?;
        let perm = match resp.permission.as_str() {
            "admin" => Permission::Admin,
            "write" => Permission::Write,
            "triage" => Permission::Triage,
            "read" => Permission::Read,
            _ => Permission::None,
        };
        {
            let mut cache = self.permission_cache.lock().unwrap();
            cache.insert(login.to_string(), (perm, Instant::now()));
        }
        Ok(perm)
    }


    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
        let offline_path = std::path::Path::new(".beads/offline").join(format!("pr_{}.json", pr));
        if offline_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&offline_path) {
                #[derive(serde::Deserialize)]
                struct OfflinePrSnapshot {
                    ci_success: bool,
                    mergeable: bool,
                    coderabbit_approved: bool,
                    bugbot_error_count: u32,
                    unresolved_thread_count: u32,
                    head_sha: String,
                    body: String,
                    comments: Vec<crate::tools::PrComment>,
                    files: Vec<crate::tools::PrFile>,
                    updated_at_epoch: Option<u64>,
                    head_committed_epoch: Option<u64>,
                }
                if let Ok(snap) = serde_json::from_str::<OfflinePrSnapshot>(&raw) {
                    let ci_status = if snap.ci_success { "green".to_string() } else { "red".to_string() };
                    let coderabbit_status = if snap.coderabbit_approved { "green".to_string() } else { "red".to_string() };
                    return Ok(PrSnapshot {
                        pr_number: pr,
                        ci_success: snap.ci_success,
                        mergeable: snap.mergeable,
                        coderabbit_approved: snap.coderabbit_approved,
                        bugbot_error_count: snap.bugbot_error_count,
                        unresolved_thread_count: Some(snap.unresolved_thread_count),
                        head_sha: snap.head_sha,
                        body: snap.body,
                        comments: snap.comments,
                        files: snap.files,
                        updated_at_epoch: snap.updated_at_epoch.unwrap_or(0),
                        ci_status,
                        coderabbit_status,
                        ci_pending: false,
                        // jleechan-8s2p: offline cache path — Bugbot
                        // outage state was not serialised before phase 2;
                        // default to false (no outage known) so the
                        // detector cannot accidentally fire on stale
                        // cache hits. Test fixtures that need
                        // bugbot_pending=true write a fresh snapshot via
                        // `all_green_snapshot` + mutation, not the
                        // offline cache.
                        bugbot_pending: false,
                        head_committed_epoch: snap.head_committed_epoch.unwrap_or(0),
                    });
                }
            }
        }
        {
            let cache = self.pr_snapshot_cache.lock().unwrap();
            if let Some((val, timestamp)) = cache.get(&pr) {
                if timestamp.elapsed() < Duration::from_secs(60) {
                    return Ok(val.clone());
                }
            }
        }
        let pr_str = pr.to_string();
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhPrView {
            mergeable: String,
            reviews: Vec<GhReview>,
            #[serde(rename = "headRefOid")]
            head_ref_oid: String,
            body: String,
            comments: Vec<GhComment>,
            files: Vec<GhFile>,
            #[serde(rename = "updatedAt")]
            updated_at: String,
        }
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhReview {
            author: GhAuthor,
            state: String,
        }
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhComment {
            author: GhAuthor,
            body: String,
            // jleechan-nplh: comment age, needed to filter stale `/er`
            // verdicts against the head commit. Default-tolerated so a gh
            // schema hiccup degrades to "age unknown" (epoch 0) rather than
            // failing the whole snapshot.
            #[serde(default, rename = "createdAt")]
            created_at: String,
        }
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhAuthor {
            login: String,
        }
        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhFile {
            path: String,
            additions: u32,
            deletions: u32,
        }
        let view: GhPrView = match run_tool(
            "gh",
            &[
                "pr",
                "view",
                &pr_str,
                "--repo",
                &self.repo,
                "--json",
                "mergeable,reviews,headRefOid,body,comments,files,updatedAt",
            ],
            30,
        ) {
            Ok(view_out) => {
                let json_start = view_out.find('{').unwrap_or(0);
                serde_json::from_str(&view_out[json_start..]).map_err(|e| {
                    DaemonError::Parse(format!("failed to parse gh pr view JSON: {e}"))
                })?
            }
            Err(_) => {
                // REST Fallback!
                let pr_url = format!("repos/{}/pulls/{}", self.repo, pr);
                let pr_json = run_tool("gh", &["api", &pr_url], 30)?;
                
                #[derive(serde::Deserialize)]
                struct RestPr {
                    mergeable: Option<bool>,
                    head: RestHead,
                    body: Option<String>,
                    updated_at: String,
                }
                #[derive(serde::Deserialize)]
                struct RestHead {
                    sha: String,
                }
                let rest_pr: RestPr = serde_json::from_str(&pr_json).map_err(|e| {
                    DaemonError::Parse(format!("failed to parse REST PR details: {e}"))
                })?;

                let reviews_url = format!("repos/{}/pulls/{}/reviews", self.repo, pr);
                let reviews_json = run_tool("gh", &["api", &reviews_url], 30).unwrap_or_else(|_| "[]".to_string());
                #[derive(serde::Deserialize)]
                struct RestReview {
                    user: Option<RestUser>,
                    state: String,
                }
                #[derive(serde::Deserialize)]
                struct RestUser {
                    login: String,
                }
                let rest_reviews: Vec<RestReview> = serde_json::from_str(&reviews_json).unwrap_or_default();

                let comments_url = format!("repos/{}/issues/{}/comments", self.repo, pr);
                let comments_json = run_tool("gh", &["api", &comments_url], 30).unwrap_or_else(|_| "[]".to_string());
                #[derive(serde::Deserialize)]
                struct RestComment {
                    user: Option<RestUser>,
                    body: String,
                    #[serde(default)]
                    created_at: String,
                }
                let rest_comments: Vec<RestComment> = serde_json::from_str(&comments_json).unwrap_or_default();

                let files_url = format!("repos/{}/pulls/{}/files", self.repo, pr);
                let files_json = run_tool("gh", &["api", &files_url], 30).unwrap_or_else(|_| "[]".to_string());
                #[derive(serde::Deserialize)]
                struct RestFile {
                    filename: String,
                    additions: u32,
                    deletions: u32,
                }
                let rest_files: Vec<RestFile> = serde_json::from_str(&files_json).unwrap_or_default();

                GhPrView {
                    mergeable: if rest_pr.mergeable.unwrap_or(false) { "MERGEABLE".to_string() } else { "CONFLICTING".to_string() },
                    reviews: rest_reviews.into_iter().map(|r| GhReview { author: GhAuthor { login: r.user.map(|u| u.login).unwrap_or_default() }, state: r.state }).collect(),
                    head_ref_oid: rest_pr.head.sha,
                    body: rest_pr.body.unwrap_or_default(),
                    comments: rest_comments.into_iter().map(|c| GhComment { author: GhAuthor { login: c.user.map(|u| u.login).unwrap_or_default() }, body: c.body, created_at: c.created_at }).collect(),
                    files: rest_files.into_iter().map(|f| GhFile { path: f.filename, additions: f.additions, deletions: f.deletions }).collect(),
                    updated_at: rest_pr.updated_at,
                }
            }
        };
        let mergeable = view.mergeable == "MERGEABLE";

        let last_coderabbit_review = view.reviews.iter()
            .rfind(|r| r.author.login.contains("coderabbit") && r.state != "COMMENTED");

        let coderabbit_status = match last_coderabbit_review {
            Some(r) => {
                if r.state == "APPROVED" {
                    "green".to_string()
                } else if r.state == "CHANGES_REQUESTED" {
                    "red".to_string()
                } else {
                    "unknown".to_string()
                }
            }
            None => "unknown".to_string(),
        };
        let coderabbit_approved = coderabbit_status == "green";

        #[derive(serde::Deserialize, serde::Serialize, Clone)]
        struct GhCheck {
            state: String,
            bucket: String,
            name: String,
        }
        let checks_out = match run_tool(
            "gh",
            &["pr", "checks", &pr_str, "--repo", &self.repo, "--json", "state,bucket,name"],
            30,
        ) {
            Ok(out) => out,
            Err(primary_err) => {
                // REST Fallback!
                let ref_url = format!("repos/{}/commits/{}/check-runs", self.repo, view.head_ref_oid);
                // jleechan-e7lp: the primary GraphQL `gh pr checks` call
                // failed (commonly a GraphQL rate limit). If the REST
                // fallback ALSO fails to execute, or returns a body that
                // isn't valid JSON, that is a genuine "we could not
                // determine CI status" outage — distinct from a PR that
                // legitimately has zero checks yet (a successful REST call
                // reports that as `{"check_runs": []}`, not an error).
                // Previously both failure shapes were silently absorbed
                // into an empty `checks` vec via `.unwrap_or(...)`, which
                // collapsed into `ci_status = "unknown"` -> `ci_pending =
                // true` even though the daemon had no idea what CI actually
                // looked like. Live incident: bead jleechan-93ft / PR
                // jleechanorg/worldarchitect.ai#7888 logged
                // VERIFICATION_PENDING 244+ times in 10 minutes while
                // GraphQL was rate-limited and the PR's CI was already 100%
                // terminal. Propagate a `DaemonError::Tool` here instead so
                // every existing `pr_snapshot` call site's jleechan-qdw
                // `BEAD_SNAPSHOT_TRANSIENT_ERROR` per-bead-isolation
                // handling takes over: the bead stays ATTESTED and is
                // retried next tick, with honest "fetch failed" telemetry
                // instead of misleading "CI still running" telemetry.
                let cr_json = run_tool("gh", &["api", &ref_url], 30).map_err(|fallback_err| {
                    DaemonError::Tool {
                        tool: "gh".to_string(),
                        rc: -1,
                        stderr: format!(
                            "CI check status unavailable for PR #{pr}: primary `gh pr checks` failed ({primary_err}) and REST check-runs fallback also failed ({fallback_err})"
                        ),
                    }
                })?;

                #[derive(serde::Deserialize)]
                struct RestCheckRuns {
                    check_runs: Vec<RestCheckRun>,
                }
                #[derive(serde::Deserialize)]
                struct RestCheckRun {
                    name: String,
                    status: String,
                    conclusion: Option<String>,
                }
                let rest_cr: RestCheckRuns = serde_json::from_str(&cr_json).map_err(|parse_err| {
                    DaemonError::Tool {
                        tool: "gh".to_string(),
                        rc: -1,
                        stderr: format!(
                            "CI check status unavailable for PR #{pr}: primary `gh pr checks` failed ({primary_err}) and REST check-runs fallback returned non-JSON output: {parse_err}"
                        ),
                    }
                })?;

                let mut legacy_checks: Vec<GhCheck> = rest_cr.check_runs.into_iter().map(|cr| {
                    let (state, bucket) = if cr.status == "completed" {
                        match cr.conclusion.as_deref() {
                            Some("success") | Some("neutral") => ("SUCCESS".to_string(), "pass".to_string()),
                            Some("cancelled") => ("CANCELLED".to_string(), "cancel".to_string()),
                            _ => ("FAILURE".to_string(), "fail".to_string()),
                        }
                    } else {
                        ("PENDING".to_string(), "pending".to_string())
                    };
                    GhCheck { state, bucket, name: cr.name }
                }).collect();

                // Some third-party CI (and older GitHub Apps) still post via
                // the legacy Commit Status API instead of the Checks API —
                // merge `/commits/{sha}/statuses` in too so those don't
                // silently vanish from `checks` when the GraphQL `gh pr
                // checks` call is rate-limited.
                let statuses_url = format!("repos/{}/commits/{}/statuses", self.repo, view.head_ref_oid);
                if let Ok(statuses_json) = run_tool("gh", &["api", &statuses_url], 30) {
                    #[derive(serde::Deserialize)]
                    struct RestStatus {
                        context: String,
                        state: String,
                    }
                    let rest_statuses: Vec<RestStatus> = serde_json::from_str(&statuses_json).unwrap_or_default();
                    for s in rest_statuses {
                        let bucket = match s.state.as_str() {
                            "success" => "pass",
                            "pending" => "pending",
                            _ => "fail",
                        };
                        legacy_checks.push(GhCheck {
                            state: s.state.to_uppercase(),
                            bucket: bucket.to_string(),
                            name: s.context,
                        });
                    }
                }
                serde_json::to_string(&legacy_checks).unwrap_or_else(|_| "[]".to_string())
            }
        };
        let json_start_c = checks_out.find('[').unwrap_or(0);
        let checks: Vec<GhCheck> = serde_json::from_str(&checks_out[json_start_c..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr checks JSON: {e}"))
        })?;
        // jleechan-8s2p (phase 2): derive Bugbot's OUTAGE signal from the
        // RAW `checks` array, BEFORE the cap-filter pass below. The
        // filter drops a Capped vendor's check, which would otherwise
        // hide the pending Bugbot from the snapshot and make the
        // waiver path still unreachable even with the detector fix.
        //
        // Bugbot outage = a check run whose name matches Bugbot AND
        // whose bucket is "pending" (the check has not yet produced
        // pass/fail). Bugbot's review-comment surface (`error_count`)
        // is a separate axis and does NOT participate in this signal.
        let bugbot_pending = checks
            .iter()
            .any(|c| c.name.to_lowercase().contains("bugbot") && c.bucket == "pending");
        let mut filtered_checks = Vec::new();
        for c in &checks {
            let name_lower = c.name.to_lowercase();
            if name_lower.contains("coderabbit")
                && crate::vendor_health::is_global_vendor_capped(crate::vendor_health::Vendor::CodeRabbit)
            {
                continue;
            }
            if name_lower.contains("bugbot")
                && crate::vendor_health::is_global_vendor_capped(crate::vendor_health::Vendor::Bugbot)
            {
                continue;
            }
            filtered_checks.push(c.clone());
        }
        // jleechan-mdun follow-up (2026-07-28): gate 1 (CI) must only read
        // check-runs it OWNS.
        let ci_owned_checks: Vec<&GhCheck> = filtered_checks
            .iter()
            .filter(|c| !check_owned_by_dedicated_gate(&c.name))
            .collect();
        let mut any_pending = false;
        let mut any_failed = false;
        for c in &ci_owned_checks {
            if c.bucket == "pending" {
                any_pending = true;
            } else if c.bucket == "fail" || c.bucket == "cancel" {
                any_failed = true;
            }
        }
        let ci_status = if ci_owned_checks.is_empty() || any_pending {
            "unknown".to_string()
        } else if any_failed {
            "red".to_string()
        } else {
            "green".to_string()
        };

        let iteration_stub =
            std::env::var("DARK_FACTORY_ITERATION_STUB").as_deref() == Ok("1");
        let ci_success = ci_success_from_check_buckets(
            &ci_owned_checks
                .iter()
                .map(|c| c.bucket.as_str())
                .collect::<Vec<_>>(),
            iteration_stub,
        );

        let mut bugbot_error_count = 0;
        for comment in &view.comments {
            let author = &comment.author.login;
            if author.contains("cursor") || author.contains("bugbot") {
                let body = comment.body.to_lowercase();
                if body.contains("error") || body.contains("fail") {
                    bugbot_error_count += 1;
                }
            }
        }

        let owner = self.repo.split('/').next().unwrap_or("").to_string();
        let repo = self.repo.split('/').nth(1).unwrap_or("").to_string();
        let query = "query($owner:String!,$repo:String!,$pr:Int!){
            repository(owner:$owner,name:$repo){
              pullRequest(number:$pr){
                reviewThreads(first:100){
                  nodes{
                    isResolved
                  }
                }
              }
            }
        }";
        // No REST fallback exists for this query: GitHub's REST API does not
        // expose per-thread resolution status (only GraphQL's
        // `reviewThreads.isResolved` does), unlike the `pr view` / `pr
        // checks` calls above which do have REST equivalents. If GraphQL is
        // unavailable there is currently no alternate path to this data.
        let gql_out = run_tool(
            "gh",
            &[
                "api",
                "graphql",
                "-F",
                &format!("owner={owner}"),
                "-F",
                &format!("repo={repo}"),
                "-F",
                &format!("pr={pr}"),
                "-f",
                &format!("query={query}"),
            ],
            30,
        );
        // jleechan-kk64: a GraphQL failure or parse failure here (rate
        // limit, transient network, malformed/truncated response) must NOT
        // report a false `0` unresolved-thread count — that would silently
        // green-light the `CommentsResolved` gate (verifier.rs) while real
        // unresolved review threads could still exist, exactly the
        // fail-open bug this fix closes. Report `None` (unknown) instead;
        // `verifier::assess` maps `None` to `GateResult::Unknown`, never
        // `Green`. The rest of `pr_snapshot` still returns `Ok(snapshot)` so
        // an isolated GraphQL hiccup doesn't take down every other gate —
        // same discipline as the `head_committed_epoch` fallback below.
        let unresolved_thread_count: Option<u32> = match gql_out {
            Ok(gql_out_str) => match unresolved_thread_count_from_gql(&gql_out_str) {
                Ok(count) => Some(count),
                Err(e) => {
                    eprintln!(
                        "[warn] failed to parse unresolved-thread GraphQL response; \
                         comments-resolved gate will report Unknown, not Green: {e:?}"
                    );
                    None
                }
            },
            Err(e) => {
                eprintln!(
                    "[warn] GraphQL query failed; comments-resolved gate will report Unknown, \
                     not Green: {e:?}"
                );
                None
            }
        };

        let mut pr_comments: Vec<crate::tools::PrComment> = view.comments.into_iter().map(|c| crate::tools::PrComment {
            author: c.author.login,
            body: c.body,
            created_at_epoch: crate::tools::iso8601_to_epoch(&c.created_at).unwrap_or(0),
        }).collect();

        let updated_at_epoch = crate::tools::iso8601_to_epoch(&view.updated_at).unwrap_or(0);

        for c in &checks {
            if c.name.to_lowercase().contains("skeptic") {
                // Synthetic comments derived from check-runs on the CURRENT
                // head are fresh by construction; stamp them with the PR's
                // updatedAt so the jleechan-nplh staleness filter never
                // discards them as age-unknown.
                if c.bucket == "pass" || c.state == "SUCCESS" {
                    pr_comments.push(crate::tools::PrComment {
                        author: "github-actions".to_string(),
                        body: "skeptic check run: verdict: pass".to_string(),
                        created_at_epoch: updated_at_epoch,
                    });
                } else if c.bucket == "fail" || c.state == "FAILURE" {
                    pr_comments.push(crate::tools::PrComment {
                        author: "github-actions".to_string(),
                        body: "skeptic check run: verdict: fail".to_string(),
                        created_at_epoch: updated_at_epoch,
                    });
                }
            }
        }

        let pr_files = view.files.into_iter().map(|f| crate::tools::PrFile {
            path: f.path,
            additions: f.additions,
            deletions: f.deletions,
        }).collect();

        // jleechan-nplh: the head commit's committer date is the freshness
        // floor for `/er` verdict comments. Failure tolerated (epoch 0 =
        // unknown = staleness filtering disabled) so a transient gh error
        // can't take down the whole snapshot — same discipline as the
        // unresolved-thread GraphQL fallback above.
        let head_committed_epoch = run_tool(
            "gh",
            &[
                "api",
                &format!("repos/{}/commits/{}", self.repo, view.head_ref_oid),
                "--jq",
                ".commit.committer.date",
            ],
            30,
        )
        .ok()
        .and_then(|out| crate::tools::iso8601_to_epoch(out.trim()))
        .unwrap_or_else(|| {
            eprintln!(
                "[warn] failed to fetch head commit committer date for PR #{pr}; \
                 /er staleness filtering disabled this snapshot"
            );
            0
        });

        let ci_pending = ci_status == "unknown";
        let snapshot = PrSnapshot {
            pr_number: pr,
            ci_success,
            mergeable,
            coderabbit_approved,
            bugbot_error_count,
            unresolved_thread_count,
            head_sha: view.head_ref_oid,
            body: view.body,
            comments: pr_comments,
            files: pr_files,
            updated_at_epoch,
            ci_status,
            coderabbit_status,
            ci_pending,
            bugbot_pending,
            head_committed_epoch,
        };
        {
            let mut cache = self.pr_snapshot_cache.lock().unwrap();
            cache.insert(pr, (snapshot.clone(), Instant::now()));
        }
        Ok(snapshot)
    }

    /// jleechan-9xrs Stage D: retarget the query at `repo` via `with_repo`
    /// instead of always fetching against `self.repo` (the daemon's global
    /// `cfg.target_repo`, bound at construction time in `main.rs`). Callers
    /// pass `overlay.repo(cfg)`, so the whole verification loop (skeptic
    /// gate, /er runner, gate assessment) now reads the bead's OWN PR/CI/
    /// review state instead of silently reading another repo's PR with the
    /// same number. Fresh `with_repo` instance (not a cache-sharing clone)
    /// so a cross-repo call can never return another repo's cached
    /// `pr_snapshot_cache` entry under a colliding PR-number key.
    fn pr_snapshot_for_repo(&self, repo: &str, pr: u64) -> Result<PrSnapshot, DaemonError> {
        self.with_repo(repo).pr_snapshot(pr)
    }

    /// jleechan-drive-pr-branch-binding-pcpr: single REST lookup
    /// (`repos/{repo}/pulls/{pr}`) used at dispatch time to decide whether
    /// a bead's `external_ref` names a live, SAME-REPO open PR that
    /// dispatch must bind to instead of fabricating
    /// `factory/<bead>-r<attempt>`. Parsing is delegated to
    /// `parse_open_pr_head_ref` (a pure, unit-testable seam) so the fork
    /// guard can be exercised directly without a `gh` subprocess.
    fn open_pr_head_ref_for_repo(&self, repo: &str, pr: u64) -> Result<PrHeadBranch, DaemonError> {
        let out = match run_tool("gh", &["api", &format!("repos/{repo}/pulls/{pr}")], 15) {
            Ok(out) => out,
            Err(_) => return Ok(PrHeadBranch::NotFound),
        };
        Ok(parse_open_pr_head_ref(&out, repo))
    }

    /// Bead jleechan-t40t (issue #326): resolve the CURRENT open PR whose
    /// head ref is `branch` in `repo`. Implementation issues
    /// `gh pr list --head <branch> --repo <repo> --json number --jq '.[0].number'`
    /// and parses the trimmed stdout as a `u64`. Mirrors the
    /// slow-tier DISPATCHED re-resolution path in `tick.rs::run_slow_tier`
    /// — both have to agree on the same branch→PR contract. `Err(_)` is
    /// returned only on a hard `gh` failure; "no such PR" is `Ok(None)` so
    /// callers can distinguish "transient tool error" (retry next tick)
    /// from "the branch really has no open PR right now" (legitimate —
    /// keep using the existing `pr_number` until one appears).
    fn pr_number_for_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        // jleechan-t40t (issue #326) r6: filter the lookup to SAME-REPO
        // PRs only. `--repo owner/repo` scopes the LIST to that repo, but
        // `--head <branch>` matches against `headRefName` which can
        // collide with a fork PR's head branch name (r6 lesson from
        // PR #305 — `gh pr list --head X --repo owner/repo` may surface
        // a fork PR whose `headRefName == X` because the JSON payload
        // includes `headRepository.nameWithOwner`). The jq filter
        // selects only entries whose headRepository matches the queried
        // repo, mirroring the same-repo guard `intake::same_repo_pr`
        // already applies to PR adoption.
        let jq_filter = format!(
            ".[] | select(.headRepository.nameWithOwner == \"{repo}\") | .number"
        );
        let out = match run_tool(
            "gh",
            &[
                "pr",
                "list",
                "--head",
                branch,
                "--repo",
                repo,
                "--json",
                "number,headRepository",
                "--jq",
                &jq_filter,
            ],
            30,
        ) {
            Ok(o) => o,
            Err(DaemonError::Tool { stderr, .. })
                if stderr.contains("404")
                    || stderr.contains("Not Found")
                    || stderr.contains("not found") =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let trimmed = out.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Ok(None);
        }
        match trimmed.parse::<u64>() {
            Ok(pr) => Ok(Some(pr)),
            Err(_) => Ok(None),
        }
    }

    /// Bead jleechan-yoqy / issue #323 (r5): `gh api gists/<id>` and report the
    /// gist's verification state. A 404 (deleted / private / never existed) is
    /// `Ok(None)` — a DEFINITIVE miss the evidence gate fails on. Any other gh
    /// error is TRANSIENT (`Err`) — the gate waits rather than churning a
    /// reroll on gh noise. A fetchable gist reports `Ok(Some(total_size > 0))`.
    fn gist_nonempty(&self, gist_id: &str) -> Result<Option<bool>, DaemonError> {
        // Sum the `size` of every file in the gist via jq; empty -> 0.
        let out = match run_tool(
            "gh",
            &[
                "api",
                &format!("gists/{gist_id}"),
                "--jq",
                "[.files[].size] | add // 0",
            ],
            30,
        ) {
            Ok(out) => out,
            Err(DaemonError::Tool { stderr, .. })
                if stderr.contains("404")
                    || stderr.contains("Not Found")
                    || stderr.contains("not found") =>
            {
                return Ok(None); // definitively missing
            }
            Err(e) => return Err(e), // transient — the gate waits
        };
        let total: u64 = out.trim().parse().unwrap_or(0);
        Ok(Some(total > 0))
    }
    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError> {
        let offline_path = std::path::Path::new(".beads/offline").join(format!("pr_{}.json", pr));
        if offline_path.exists() {
            let _ = std::fs::remove_file(&offline_path);
            {
                let mut pr_cache = self.pr_snapshot_cache.lock().unwrap();
                pr_cache.remove(&pr);
                let mut issues_cache = self.labeled_issues_cache.lock().unwrap();
                issues_cache.clear();
            }
            return Ok(());
        }
        let pr_str = pr.to_string();
        run_tool(
            "gh",
            &["pr", "close", &pr_str, "--repo", &self.repo, "-c", comment],
            30,
        )?;
        {
            let mut pr_cache = self.pr_snapshot_cache.lock().unwrap();
            pr_cache.remove(&pr);
            let mut issues_cache = self.labeled_issues_cache.lock().unwrap();
            issues_cache.clear();
        }
        Ok(())
    }

    /// jleechan-v6ud / issue #340: retarget `gh pr close` at `repo` via
    /// `with_repo` instead of always closing against `self.repo` (the
    /// daemon's global `cfg.target_repo`, bound at construction time in
    /// `main.rs`). Without this override, a same-numbered PR that already
    /// merged in the default repo (beads 8jxr and 9rkz: the same `#315`
    /// and `#314` had ALREADY merged in `jleechanorg/worldarchitect.ai` at
    /// the moment the reroll closed them) makes `gh pr close` error with
    /// "can't be closed because it was already merged", wedging the bead
    /// on a transient tool error instead of mutating the bead's actual
    /// repo's PR. Fresh `with_repo` instance (not a cache-sharing clone)
    /// so a cross-repo call can never evict another repo's cached
    /// `pr_snapshot_cache` entry under a colliding PR-number key.
    fn close_pr_for_repo(&self, repo: &str, pr: u64, comment: &str) -> Result<(), DaemonError> {
        self.with_repo(repo).close_pr(pr, comment)
    }

    fn remote_branch_last_commit(&self, branch: &str) -> Result<Option<u64>, DaemonError> {
        let offline_path = std::path::Path::new(".beads/offline").join(format!("branch_{}.json", branch));
        if offline_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&offline_path) {
                #[derive(serde::Deserialize)]
                struct OfflineBranch {
                    last_commit_epoch: Option<u64>,
                }
                if let Ok(b) = serde_json::from_str::<OfflineBranch>(&raw) {
                    return Ok(b.last_commit_epoch);
                }
            }
        }
        {
            let cache = self.branch_commit_cache.lock().unwrap();
            if let Some((val, timestamp)) = cache.get(branch) {
                if timestamp.elapsed() < Duration::from_secs(60) {
                    return Ok(*val);
                }
            }
        }
        let path = format!("repos/{}/branches/{}", self.repo, branch);
        let out = match run_tool("gh", &["api", &path], 30) {
            Ok(o) => o,
            Err(DaemonError::Tool { stderr, .. }) if stderr.contains("404") || stderr.contains("Not Found") || stderr.contains("not found") => {
                {
                    let mut cache = self.branch_commit_cache.lock().unwrap();
                    cache.insert(branch.to_string(), (None, Instant::now()));
                }
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        #[derive(serde::Deserialize)]
        struct GhBranch {
            commit: GhCommit,
        }
        #[derive(serde::Deserialize)]
        struct GhCommit {
            commit: GhCommitDetails,
        }
        #[derive(serde::Deserialize)]
        struct GhCommitDetails {
            committer: GhCommitter,
        }
        #[derive(serde::Deserialize)]
        struct GhCommitter {
            date: String,
        }
        let json_start = out.find('{').unwrap_or(0);
        let resp: GhBranch = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse branch commit response: {e}"))
        })?;
        let epoch = crate::tools::iso8601_to_epoch(&resp.commit.commit.committer.date).ok_or_else(|| {
            DaemonError::Parse(format!("failed to parse date: {}", resp.commit.commit.committer.date))
        })?;
        {
            let mut cache = self.branch_commit_cache.lock().unwrap();
            cache.insert(branch.to_string(), (Some(epoch), Instant::now()));
        }
        Ok(Some(epoch))
    }

    /// jleechan-bqdv Stage C: retarget the query at `repo` via `with_repo`
    /// instead of always polling `self.repo` (the daemon's global
    /// `cfg.target_repo`, bound at construction time in `main.rs`). Callers
    /// pass `overlay.repo(cfg)`, so a bead whose resolved repo differs from
    /// the global one is now actually observable instead of silently
    /// invisible to the coder-silence watcher.
    fn remote_branch_last_commit_for_repo(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        self.with_repo(repo).remote_branch_last_commit(branch)
    }
}

/// Pure parser behind [`Scm::open_pr_head_ref_for_repo`] (unit-testable
/// seam — no `gh` subprocess needed to exercise the fork guard).
///
/// Fork guard (codex cross-model review of PR #305): binding to a FORK PR's
/// head branch name and then pushing to the queried repo creates an
/// unrelated same-named branch there and never touches the actual PR —
/// silent wrong behavior. A fork head (`head.repo.full_name != repo`), or a
/// missing/deleted head repo (`head.repo: null` — GitHub omits it once the
/// fork has been deleted), therefore resolves to `PrHeadBranch::Fork`, not
/// `SameRepo`, mirroring the intake-side cross-repository guard
/// (`intake::same_repo_pr`).
pub(crate) fn parse_open_pr_head_ref(out: &str, repo: &str) -> PrHeadBranch {
    #[derive(serde::Deserialize)]
    struct RestPullView {
        state: String,
        head: RestPullViewHead,
    }
    #[derive(serde::Deserialize)]
    struct RestPullViewHead {
        #[serde(rename = "ref")]
        ref_name: String,
        repo: Option<RestPullViewHeadRepo>,
    }
    #[derive(serde::Deserialize)]
    struct RestPullViewHeadRepo {
        full_name: Option<String>,
    }
    let json_start = out.find('{').unwrap_or(0);
    let parsed: RestPullView = match serde_json::from_str(&out[json_start..]) {
        Ok(p) => p,
        Err(_) => return PrHeadBranch::NotFound,
    };
    if !parsed.state.eq_ignore_ascii_case("open") {
        return PrHeadBranch::NotFound;
    }
    let same_repo = parsed
        .head
        .repo
        .as_ref()
        .and_then(|r| r.full_name.as_deref())
        .map(|full| full.eq_ignore_ascii_case(repo))
        .unwrap_or(false);
    if same_repo {
        PrHeadBranch::SameRepo(parsed.head.ref_name)
    } else {
        PrHeadBranch::Fork
    }
}

#[cfg(test)]
mod open_pr_head_ref_tests {
    // Codex cross-model review of PR #305: direct unit coverage of the pure
    // REST-parsing seam, using real GitHub REST PR-view JSON shapes — the
    // fake-Scm-level tests in dispatch.rs/tick_integration.rs prove the
    // CONTRACT (fork PR -> ForkFallback -> generated branch); these prove
    // the actual `gh api repos/{repo}/pulls/{pr}` JSON parsing that feeds it.
    use super::parse_open_pr_head_ref;
    use crate::tools::PrHeadBranch;

    #[test]
    fn open_same_repo_pr_resolves_same_repo_with_head_ref() {
        let json = r#"{"state":"open","head":{"ref":"factory/jleechan-xa99-r1","repo":{"full_name":"owner/repo"}}}"#;
        assert_eq!(
            parse_open_pr_head_ref(json, "owner/repo"),
            PrHeadBranch::SameRepo("factory/jleechan-xa99-r1".to_string())
        );
    }

    #[test]
    fn open_fork_pr_resolves_fork_not_same_repo_head_ref() {
        // The fork's OWN branch happens to be named identically to what a
        // generated branch would look like — proving the guard checks
        // `head.repo.full_name`, not just PR openness or the ref string.
        let json = r#"{"state":"open","head":{"ref":"factory/jleechan-xa99-r1","repo":{"full_name":"someone-else/repo"}}}"#;
        assert_eq!(parse_open_pr_head_ref(json, "owner/repo"), PrHeadBranch::Fork);
    }

    #[test]
    fn open_pr_with_case_different_same_repo_name_still_matches() {
        let json = r#"{"state":"OPEN","head":{"ref":"factory/x","repo":{"full_name":"Owner/Repo"}}}"#;
        assert_eq!(
            parse_open_pr_head_ref(json, "owner/repo"),
            PrHeadBranch::SameRepo("factory/x".to_string())
        );
    }

    #[test]
    fn open_pr_with_deleted_fork_head_repo_resolves_fork_not_same_repo() {
        // GitHub omits `head.repo` entirely once the source fork has been
        // deleted — must NOT default to "assume same repo".
        let json = r#"{"state":"open","head":{"ref":"factory/x","repo":null}}"#;
        assert_eq!(parse_open_pr_head_ref(json, "owner/repo"), PrHeadBranch::Fork);
    }

    #[test]
    fn closed_same_repo_pr_resolves_not_found() {
        let json = r#"{"state":"closed","head":{"ref":"factory/x","repo":{"full_name":"owner/repo"}}}"#;
        assert_eq!(parse_open_pr_head_ref(json, "owner/repo"), PrHeadBranch::NotFound);
    }

    #[test]
    fn malformed_json_resolves_not_found() {
        assert_eq!(parse_open_pr_head_ref("not json", "owner/repo"), PrHeadBranch::NotFound);
    }
}

/// Resolve `$DARK_FACTORY_HOLDOUTS` (or its default sibling-repo location) and
/// fail loudly if it does not exist on disk. Silently continuing with a
/// nonexistent path would build a sandbox-exec deny rule that can never
/// match anything real — the implementing-agent isolation guarantee this
/// repo's CRITICAL Agent Isolation section depends on would degrade to a
/// no-op without anyone noticing (bd portability audit).
fn resolve_holdouts_path_or_fail() -> Result<String, DaemonError> {
    let home = std::env::var("HOME").unwrap_or_default();
    let holdouts = std::env::var("DARK_FACTORY_HOLDOUTS")
        .unwrap_or_else(|_| format!("{}/projects/dark-factory-holdouts", home));
    if !std::path::Path::new(&holdouts).is_dir() {
        return Err(DaemonError::Config(format!(
            "Sealed holdouts repo not found at {holdouts}. The implementing-agent \
             sandbox's deny-list depends on this path existing — silently continuing \
             would run the agent with an ineffective (empty) deny rule. Set \
             DARK_FACTORY_HOLDOUTS to the sealed sibling repo's real location, or \
             clone it to the default path above."
        )));
    }
    Ok(holdouts)
}

#[cfg(test)]
mod resolve_holdouts_path_tests {
    use super::{gh_env_test_lock, resolve_holdouts_path_or_fail};

    #[test]
    fn fails_loud_when_no_env_and_default_missing() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev_holdouts = std::env::var("DARK_FACTORY_HOLDOUTS").ok();
        let prev_home = std::env::var("HOME").ok();
        std::env::remove_var("DARK_FACTORY_HOLDOUTS");
        std::env::set_var("HOME", "/nonexistent-home-for-holdouts-test");

        let result = resolve_holdouts_path_or_fail();

        if let Some(v) = prev_holdouts {
            std::env::set_var("DARK_FACTORY_HOLDOUTS", v);
        }
        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        }

        let err = result.expect_err("expected a fail-loud error, got Ok");
        assert!(
            err.to_string().contains("DARK_FACTORY_HOLDOUTS"),
            "error should mention DARK_FACTORY_HOLDOUTS, got: {err}"
        );
    }

    #[test]
    fn fails_loud_when_env_set_but_missing() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let prev = std::env::var("DARK_FACTORY_HOLDOUTS").ok();
        std::env::set_var(
            "DARK_FACTORY_HOLDOUTS",
            "/definitely/does/not/exist/holdouts",
        );

        let result = resolve_holdouts_path_or_fail();

        match prev {
            Some(v) => std::env::set_var("DARK_FACTORY_HOLDOUTS", v),
            None => std::env::remove_var("DARK_FACTORY_HOLDOUTS"),
        }

        let err = result.expect_err("expected a fail-loud error, got Ok");
        assert!(err.to_string().contains("DARK_FACTORY_HOLDOUTS"));
    }

    #[test]
    fn succeeds_when_env_points_at_real_dir() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp = std::env::temp_dir().join("resolve_holdouts_path_tests_real_dir");
        std::fs::create_dir_all(&tmp).unwrap();
        let prev = std::env::var("DARK_FACTORY_HOLDOUTS").ok();
        std::env::set_var("DARK_FACTORY_HOLDOUTS", &tmp);

        let result = resolve_holdouts_path_or_fail();

        match prev {
            Some(v) => std::env::set_var("DARK_FACTORY_HOLDOUTS", v),
            None => std::env::remove_var("DARK_FACTORY_HOLDOUTS"),
        }
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(result.unwrap(), tmp.to_string_lossy().to_string());
    }
}

/// AO v0.1.3 accepts the task prompt positionally but does not expose its
/// core `branch` field as a CLI flag. The preload bridge keeps the public CLI
/// argv valid while passing the exact factory branch to AO core before the
/// workspace or worker is created. `CARGO_MANIFEST_DIR` is embedded by the
/// build, so the systemd-installed binary resolves the bridge from the same
/// checkout it was built from rather than assuming a user-specific AO path.
fn ao_spawn_bridge_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("ao-spawn-v013-bridge.mjs")
}

fn ao_spawn_command_with_mode(
    agent: &str,
    spec: &SpawnSpec,
    diagnostic: bool,
) -> Result<Command, DaemonError> {
    let bridge = ao_spawn_bridge_path();
    if !bridge.is_file() {
        return Err(DaemonError::Config(format!(
            "AO v0.1.3 spawn bridge is missing at {}; rebuild/reinstall the daemon from a complete checkout",
            bridge.display()
        )));
    }
    let bridge_arg = format!("--import={}", bridge.display());
    if bridge_arg.chars().any(char::is_whitespace) {
        return Err(DaemonError::Config(format!(
            "AO v0.1.3 spawn bridge path contains whitespace and cannot be represented safely in NODE_OPTIONS: {}",
            bridge.display()
        )));
    }

    let mut cmd = if std::env::consts::OS == "macos" {
        let holdouts = resolve_holdouts_path_or_fail()?;
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n",
            holdouts, holdouts
        );
        let mut command = Command::new("sandbox-exec");
        command.arg("-p").arg(&profile).arg("ao");
        command
    } else {
        Command::new("ao")
    };

    // Bind every worker spawn to its routed target checkout. Without this, AO
    // inherits the daemon process cwd (normally the dark-factory checkout),
    // so even legacy single-repo routing can create a worker in the wrong
    // repository. Startup diagnostics are read-only and intentionally do not
    // require a worker checkout.
    if !diagnostic {
        let checkout = spec.local_checkout.as_ref().ok_or_else(|| {
            DaemonError::Config(format!(
                "AO worker spawn for repo {:?} has no target checkout; refusing to inherit daemon cwd",
                spec.repo
            ))
        })?;
        if !checkout.is_absolute() {
            return Err(DaemonError::Config(format!(
                "AO worker spawn target checkout must be absolute: {}",
                checkout.display()
            )));
        }
        let verified = if spec.managed_checkout {
            crate::target_worktree::ensure_managed_target_worktree(
                &spec.repo,
                checkout,
                spec.expected_revision.as_deref(),
            )?
        } else {
            crate::target_worktree::ensure_target_worktree(
                &spec.repo,
                checkout,
                spec.expected_revision.as_deref(),
            )?
        };
        if spec.managed_checkout {
            crate::target_worktree::ensure_managed_push_remote(
                &spec.repo,
                &verified,
                &spec.remote,
            )?;
        }
        cmd.current_dir(&verified)
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &verified);
        // Managed target worktrees are daemon-owned execution resources.  The
        // AO project registry may still point at the operator's source
        // checkout, so tell the preload bridge that this validated checkout
        // is authoritative for this spawn only.  For configured user-owned
        // checkouts, remove the marker so an inherited environment variable
        // cannot weaken the source-mismatch guard.
        if spec.managed_checkout {
            cmd.env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1");
        } else {
            cmd.env_remove("DARK_FACTORY_AO_MANAGED_CHECKOUT");
        }
        if let Some(expected_revision) = spec
            .expected_revision
            .as_deref()
            .filter(|revision| !revision.trim().is_empty())
        {
            cmd.env("DARK_FACTORY_AO_EXPECTED_REVISION", expected_revision);
        }
    }

    // This is the complete AO v0.1.3 public spawn argv: no --prompt,
    // --name, or --branch. The preload validates this shape independently.
    cmd.arg("spawn")
        .arg("--project")
        .arg(&spec.ao_project)
        .arg("--agent")
        .arg(agent);
    if diagnostic {
        // If the preload fails to execute, AO v0.1.3 rejects this unknown
        // option before dispatch. That makes the supposedly read-only probe
        // fail safe instead of accidentally creating a worker.
        cmd.arg("--dark-factory-read-only-diagnostic");
    }
    cmd.arg("--")
        .arg(&spec.prompt)
        .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
        .env("DARK_FACTORY_AO_SPAWN_BRANCH", &spec.branch);
    if diagnostic {
        cmd.env("DARK_FACTORY_AO_BRIDGE_DIAGNOSTIC", "1");
    }

    let node_options = std::env::var("NODE_OPTIONS").unwrap_or_default();
    let bridge_options = format!("--experimental-import-meta-resolve {bridge_arg}");
    let bridged_node_options = if node_options.trim().is_empty() {
        bridge_options
    } else {
        format!("{node_options} {bridge_options}")
    };
    cmd.env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", &node_options)
        .env("NODE_OPTIONS", bridged_node_options);

    for (key, _) in std::env::vars() {
        if key == "DARK_FACTORY_HOLDOUTS" || key.to_uppercase().contains("HOLDOUT") {
            cmd.env_remove(key);
        }
    }

    Ok(cmd)
}

fn ao_spawn_command(agent: &str, spec: &SpawnSpec) -> Result<Command, DaemonError> {
    ao_spawn_command_with_mode(agent, spec, false)
}

/// Maps legacy vendor aliases onto their canonical AO plugin names. The
/// runtime and startup paths consult this single source so a renamed plugin
/// never silently disappears from `--agent` argv or preflight checks.
///
/// `aow -> minimax` predates this PR (legacy minimax-by-AO alias).
/// `claudem -> minimax` is the bashrc MiniMax wrapper name.
/// `agy -> antigravity` covers AO main's 2026-07-18 rename of the
/// antigravity plugin; lane 222 burned a full spawn cycle before the
/// jleechan-agy-vendor-name-drift-9lvs regression exposed the mismatch.
pub fn canonical_for_alias(vendor: &str) -> Option<&'static str> {
    match vendor {
        "aow" | "claudem" => Some("minimax"),
        "agy" => Some("antigravity"),
        _ => None,
    }
}

/// Fail-closed preflight over the daemon's configured vendor set against
/// the bridge-reported AO plugin registry. This runs on EVERY startup —
/// skipping it because the registry is empty/missing/malformed is what let
/// the jleechan-9lvs drift class reach a per-bead spawn cycle in the first
/// place. Distinct error messages keep triage cheap:
///
/// * registry error → `VendorRegistryError` (registry reachable, list broken)
/// * empty registry → `VendorRegistryEmpty` (registry reachable, no plugins)
/// * missing canonical vendor → `VendorNotInstalled` (each missing name listed)
pub fn validate_configured_vendors(
    installed_plugins: Result<&[String], &str>,
    configured_vendors: &[String],
) -> Result<(), DaemonError> {
    let installed = match installed_plugins {
        Ok(list) => list,
        Err(message) => {
            return Err(DaemonError::Config(format!(
                "AO bridge reported a registry error while enumerating agent plugins ({}); refusing to start because the daemon cannot prove any configured vendor is installed",
                message
            )));
        }
    };
    if installed.is_empty() {
        return Err(DaemonError::Config(format!(
            "AO bridge reported zero installed agent plugins (configured vendors: {}); refusing to start because a factory with zero coder plugins cannot dispatch",
            configured_vendors.join(", ")
        )));
    }

    // Dedup configured_vendors by their canonical form so that e.g.
    // ['agy', 'antigravity'] is treated as a single vendor before we
    // ask the registry whether it's installed.
    let mut seen = std::collections::HashSet::new();
    let mut canonical_chain: Vec<String> = Vec::new();
    for vendor in configured_vendors {
        let canonical = canonical_for_alias(vendor)
            .map(str::to_string)
            .unwrap_or_else(|| vendor.clone());
        if canonical.is_empty() {
            continue;
        }
        if seen.insert(canonical.clone()) {
            canonical_chain.push(canonical);
        }
    }

    let mut missing: Vec<String> = Vec::new();
    for canonical in &canonical_chain {
        if !installed.iter().any(|name| name == canonical) {
            missing.push(canonical.clone());
        }
    }
    if !missing.is_empty() {
        return Err(DaemonError::Config(format!(
            "AO plugin registry is missing configured vendor(s) {} (installed: {}); refusing to start so a renamed plugin cannot burn a full spawn cycle",
            missing.join(", "),
            installed.join(", ")
        )));
    }
    Ok(())
}

/// Runs the AO v0.1.3 preload in its read-only diagnostic mode. This checks
/// the actual `ao` executable selected by the daemon's production PATH, its
/// Node major version, package version, core APIs, configured project, and
/// plugin resolution without performing preflight side effects, acquiring a
/// spawn lock, creating a workspace, or launching a worker.
///
/// `configured_vendors` is the daemon's full deduped canonical vendor list
/// (default + fallback chain, after alias resolution). The bridge diagnostic
/// payload distinguishes three registry states via distinct JSON keys:
/// `agentPlugins` (sorted array of installed plugin names) when the registry
/// answered, `agentPluginsError` (string message) when the registry threw.
/// An absent key is treated as a malformed payload and rejected.
pub fn verify_ao_bridge_compatibility(
    ao_project: &str,
    agent: &str,
    configured_vendors: &[String],
) -> Result<(), DaemonError> {
    let spec = SpawnSpec {
        bead_id: "daemon-startup-diagnostic".to_string(),
        branch: "factory/daemon-startup-diagnostic".to_string(),
        prompt: "dark-factory AO v0.1.3 read-only compatibility diagnostic".to_string(),
        repo: String::new(),
        ao_project: ao_project.to_string(),
        remote: String::new(),
        local_checkout: None,
        expected_revision: None,
        managed_checkout: false,
        expected_cwd: None,
    };
    let mut command = ao_spawn_command_with_mode(agent, &spec, true)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|error| DaemonError::Tool {
        tool: "ao bridge compatibility diagnostic".to_string(),
        rc: -1,
        stderr: format!("execution failed: {error}"),
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(DaemonError::Tool {
            tool: "ao bridge compatibility diagnostic".to_string(),
            rc: output.status.code().unwrap_or(-1),
            stderr,
        });
    }
    let payload = stdout
        .lines()
        .find_map(|line| line.strip_prefix("AO_BRIDGE_DIAGNOSTIC="))
        .ok_or_else(|| {
            DaemonError::Parse(format!(
                "AO bridge compatibility diagnostic returned success without its marker: {stdout}"
            ))
        })?;
    let diagnostic: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        DaemonError::Parse(format!(
            "AO bridge compatibility diagnostic marker was invalid JSON: {error}"
        ))
    })?;
    if diagnostic.get("cliVersion").and_then(|value| value.as_str()) != Some("0.1.3")
        || !diagnostic
            .get("nodeVersion")
            .and_then(|value| value.as_str())
            .is_some_and(|version| version.starts_with("22."))
    {
        return Err(DaemonError::Config(format!(
            "AO bridge compatibility diagnostic reported an incompatible runtime: {diagnostic}"
        )));
    }
    // Three mutually-exclusive registry states, distinguished by separate
    // JSON keys so the daemon cannot confuse "registry reachable but empty"
    // (hard failure: factory cannot dispatch) with "registry threw" (also a
    // hard failure, but the message names the underlying exception so the
    // operator knows to fix the AO install, not the daemon config).
    let registry_state: Result<Vec<String>, String> = match (
        diagnostic.get("agentPlugins"),
        diagnostic.get("agentPluginsError"),
    ) {
        (Some(_), Some(_)) => Err(
            "AO bridge diagnostic carried BOTH agentPlugins and agentPluginsError; payload is malformed"
                .to_string(),
        ),
        (Some(value), None) => match value.as_array() {
            Some(array) => {
                let mut names: Vec<String> = array
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect();
                names.sort();
                names.dedup();
                Ok(names)
            }
            None => Err(
                "AO bridge diagnostic agentPlugins is not a JSON array".to_string(),
            ),
        },
        (None, Some(value)) => Err(value
            .as_str()
            .unwrap_or("non-string agentPluginsError in bridge diagnostic")
            .to_string()),
        (None, None) => Err(
            "AO bridge diagnostic payload omitted both agentPlugins and agentPluginsError; \
             refusing to start because the daemon cannot prove any configured vendor is installed"
                .to_string(),
        ),
    };
    let installed_slice: Result<&[String], &str> = match &registry_state {
        Ok(list) => Ok(list.as_slice()),
        Err(message) => Err(message.as_str()),
    };
    validate_configured_vendors(installed_slice, configured_vendors).map_err(|error| {
        match error {
            DaemonError::Config(message) => DaemonError::Config(format!(
                "AO bridge compatibility preflight failed: {message}"
            )),
            other => other,
        }
    })?;
    Ok(())
}

pub struct CliSessions {
    pub project: String,
    pub agent: String,
    spawned_worktrees:
        std::sync::Mutex<std::collections::HashMap<(String, String), std::path::PathBuf>>,
    spawned_session_worktrees: std::sync::Mutex<
        std::collections::HashMap<String, (String, std::path::PathBuf)>,
    >,
}

impl CliSessions {
    pub fn new(repo: &str, agent: &str) -> Self {
        let mut project = repo.split('/').next_back().unwrap_or(repo).to_string();
        if project == "worldarchitect.ai" {
            project = "worldarchitect".to_string();
        }
        Self {
            project,
            agent: agent.to_string(),
            spawned_worktrees: std::sync::Mutex::new(std::collections::HashMap::new()),
            spawned_session_worktrees: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn run_spawn_process(&self, agent: &str, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        // jleechan-bqdv Stage C: spawn into `spec.ao_project` (resolved per
        // bead by `Config::resolve_repo`, Stage B), not `self.project` (the
        // daemon's single global project bound once at `CliSessions::new`
        // construction time). For every pre-existing single-repo config
        // these are identical — `Config::resolve_repo`'s legacy fallback
        // path derives `ao_project` via the exact same last-path-segment
        // rule `CliSessions::new` uses — so this is behavior-preserving for
        // today's only configured repo, while actually making a bead whose
        // `target_repo` names a DIFFERENT `[repos.*]` entry spawn into ITS
        // project instead of silently landing in the global one (the
        // jleechan-9sh5 root cause).
        let mut cmd = ao_spawn_command(agent, spec)?;
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().map_err(|e| DaemonError::Tool {
            tool: if std::env::consts::OS == "macos" { "sandbox-exec".to_string() } else { "ao".to_string() },
            rc: -1,
            stderr: format!("execution failed: {e}"),
        })?;

        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        let err_msg = String::from_utf8_lossy(&output.stderr).into_owned();
        let session = Self::classify_spawn_output(
            agent,
            output.status.success(),
            output.status.code(),
            &out,
            &err_msg,
        )?;
        let workspace = Self::spawn_workspace_path(&out);
        let observed_branch = Self::spawn_branch(&out);
        let spawn_error = match workspace.as_ref() {
            None => Some(DaemonError::Parse(format!(
                "ao spawn --agent {agent} returned session {} without an absolute Worktree path; refusing to dispatch without remote verification",
                session.0
            ))),
            Some(workspace_path) => {
                if observed_branch.as_deref() != Some(spec.branch.as_str()) {
                    Some(DaemonError::Parse(format!(
                        "ao spawn --agent {agent} returned session {} with branch {:?}, expected {:?}; refusing to dispatch a branch-mismatched worker",
                        session.0, observed_branch, spec.branch
                    )))
                } else if let Some(expected_revision) = spec.expected_revision.as_deref() {
                    match crate::target_worktree::validate_existing_target_worktree(
                        &spec.repo,
                        workspace_path,
                        Some(expected_revision),
                    ) {
                        Ok(_) => None,
                        Err(error) => Some(DaemonError::Config(format!(
                            "AO worker workspace for session {} is not bound to repo {} at expected revision {}: {error}",
                            session.0, spec.repo, expected_revision
                        ))),
                    }
                } else {
                    None
                }
            }
        };
        if let Some(spawn_error) = spawn_error {
            return match run_tool("ao", &["session", "kill", &session.0], 30) {
                Ok(_) => Err(spawn_error),
                Err(cleanup_error) => Err(DaemonError::SpawnCleanupFailed {
                    session: session.0,
                    spawn_error: Box::new(spawn_error),
                    cleanup_error: Box::new(cleanup_error),
                }),
            };
        }
        let workspace = workspace.ok_or_else(|| {
            DaemonError::Config(format!(
                "AO worker workspace for session {} is missing workspace path",
                session.0
            ))
        })?;
        self.spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (spec.ao_project.clone(), spec.branch.clone()),
                workspace.clone(),
            );
        self.spawned_session_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.0.clone(), (spec.branch.clone(), workspace));
        Ok(session)
    }

    fn spawn_workspace_path(out: &str) -> Option<std::path::PathBuf> {
        out.lines().find_map(|line| {
            let value = line.trim().strip_prefix("Worktree:")?.trim();
            let path = std::path::PathBuf::from(value);
            (path.is_absolute() && value != "-").then_some(path)
        })
    }

    fn spawn_branch(out: &str) -> Option<String> {
        out.lines().find_map(|line| {
            let value = line.trim().strip_prefix("Branch:")?.trim();
            (!value.is_empty() && value != "-").then(|| value.to_string())
        })
    }

    /// Pure classification of `ao spawn`'s exit status + stdout/stderr into a
    /// `SessionId` or a `DaemonError`, split out of `run_spawn_process` so the
    /// decision logic is unit-testable without shelling out to a real `ao`
    /// binary.
    fn classify_spawn_output(
        agent: &str,
        success: bool,
        code: Option<i32>,
        out: &str,
        err_msg: &str,
    ) -> Result<SessionId, DaemonError> {
        if !success {
            // jleechan-2s0h / jleechan-la67: AO's spawn-queue admission
            // control (`packages/core/src/spawn-queue.ts`'s own
            // `MAX_PENDING_REQUESTS` cap — a DIFFERENT layer than the
            // "active sessions >= session cap" REQUEST= deferral handled
            // below) throws a plain `Error` in the CLI that re-propagates
            // through commander's default handler. That means it surfaces
            // as a genuine nonzero exit (rc=1, Tool-shaped: stderr contains
            // "Spawn queue is full for project '<id>' (<n> pending
            // requests)"), NOT the `REQUEST=` stdout success-with-deferral
            // pattern. Live incident jleechan-la67 (2026-07-08):
            // spawn-queue-worldarchitect.json hit its 100-pending cap and
            // every subsequent `ao spawn` failed rc=1 with this message;
            // the daemon classified it as `DaemonError::Tool`, a genuine
            // transient failure that increments `spawn_failure_count`,
            // eventually parking beads `HumanHeld` purely from admission
            // backpressure that has nothing to do with the bead itself.
            // This is provably safe to retry for the same reason as the
            // REQUEST= case: `enqueueSpawnRequest` threw before any
            // worktree, branch, or process was created, so nothing is
            // leaked or double-spawned by requeuing and trying again next
            // tick.
            // Anchored on "...is full for project" (not just "spawn queue is
            // full") to shrink the false-positive surface: this factory's
            // bead prompts are themselves LLM-generated coding-task text, so
            // a bead literally about fixing this handling (or quoting this
            // very error string) could otherwise cause an unrelated spawn
            // failure whose stderr echoes the bare phrase to be misclassified
            // as retry-safe backpressure. The longer anchor still tolerates
            // the variable `'<project-id>'` in the AO-thrown message.
            if err_msg.to_lowercase().contains("spawn queue is full for project") {
                return Err(DaemonError::Deferred(format!(
                    "ao spawn --agent {agent} rejected: admission queue full ({})",
                    err_msg.trim()
                )));
            }
            return Err(DaemonError::Tool {
                tool: format!("ao spawn --agent {agent}"),
                rc: code.unwrap_or(-1),
                stderr: err_msg.to_string(),
            });
        }

        let mut sess_name = None;
        for line in out.lines() {
            if line.starts_with("SESSION=") {
                sess_name = Some(line.split('=').nth(1).unwrap_or("").trim().to_string());
            } else if line.starts_with("spawned session ") {
                let parts: Vec<&str> = line.strip_prefix("spawned session ").unwrap_or("").split_whitespace().collect();
                if !parts.is_empty() {
                    sess_name = Some(parts[0].to_string());
                }
            }
        }

        if let Some(name) = sess_name {
            return Ok(SessionId(name));
        }

        // jleechan-5ia2: `ao spawn` has its own internal admission-control
        // queue (packages/cli/src/commands/spawn.ts in agent-orchestrator) —
        // when a project's active-session count is at/above AO's configured
        // cap, `ao spawn` does NOT create a session at all. It enqueues a
        // deferred `SpawnRequest` and prints `REQUEST=<id>` instead of
        // `SESSION=<id>`, exiting 0 (success). No worktree, branch, or
        // process was ever created for this call, so unlike a genuine parse
        // failure it is always safe to retry. Before this fix that case fell
        // through to `DaemonError::Parse` below, which `is_transient()`
        // classifies as fatal — `dispatch_ready`'s `?` on `sessions.spawn`
        // then propagated it all the way to `main.rs`, which calls
        // `std::process::exit(1)` on any non-transient tick error. Live
        // evidence: `rust-daemon.err.log` showed the exact string "ao spawn
        // produced no session name: Reason: N active sessions >= cap N" and
        // 18 systemd restarts in ~15 minutes while `worldarchitect` sat at
        // its cap.
        if let Some(request_line) = out.lines().find(|l| l.starts_with("REQUEST=")) {
            return Err(DaemonError::Deferred(format!(
                "ao spawn --agent {agent} queued instead of spawning ({request_line})"
            )));
        }

        Err(DaemonError::Parse(format!(
            "ao spawn --agent {agent} produced no SESSION= line: {out}"
        )))
    }

    fn spawn_with_fallback(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        let fallback_str = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN")
            .unwrap_or_else(|_| "agy->claudem".to_string());
        let fallback_agents = build_runtime_fallback_chain(&self.agent, &fallback_str);
        fallback_spawn(&fallback_agents, |agent| self.run_spawn_process(agent, spec))
    }
}

/// Pure helper used by `CliSessions::spawn_with_fallback` (and unit-tested
/// directly) that canonicalizes the runtime vendor chain through the same
/// `canonical_for_alias` map the startup preflight uses. Dedup is by
/// canonical form so a config that names both `agy` and `antigravity`
/// doesn't try the same plugin twice.
fn build_runtime_fallback_chain(default_agent: &str, fallback_str: &str) -> Vec<String> {
    let canonicalize = |vendor: &str| -> String {
        canonical_for_alias(vendor)
            .map(str::to_string)
            .unwrap_or_else(|| vendor.to_string())
    };
    let mut chain: Vec<String> = Vec::new();
    let default_canonical = canonicalize(default_agent);
    if !default_canonical.is_empty() {
        chain.push(default_canonical);
    }
    for part in fallback_str.split("->") {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let canonical = canonicalize(trimmed);
        if !canonical.is_empty() && !chain.contains(&canonical) {
            chain.push(canonical);
        }
    }
    chain
}

/// Walks `agents` in order, calling `attempt_spawn` for each until one
/// succeeds. Pulled out of `spawn_with_fallback` into a free function taking
/// an injectable closure so the fallback-chain-exhaustion aggregation logic
/// is unit-testable (see `spawn_fallback_tests` below) without shelling out
/// to a real `ao` binary via `run_spawn_process`.
///
/// jleechan-r56m: each iteration used to overwrite a single `last_err`
/// variable, so once every agent in `agents` had failed, only the LAST
/// agent's error survived into the returned `Err`. This discarded every
/// earlier vendor's specific failure reason, which is exactly what made bead
/// jleechan-93ft attempt 3's `PARKED_HUMAN_HELD` telemetry (2026-07-10) look
/// like an "agy plugin missing" problem when agy was merely the last vendor
/// tried in the chain.
fn fallback_spawn<F>(agents: &[String], mut attempt_spawn: F) -> Result<SessionId, DaemonError>
where
    F: FnMut(&str) -> Result<SessionId, DaemonError>,
{
    let mut attempts: Vec<(String, DaemonError)> = Vec::new();
    for agent in agents {
        match attempt_spawn(agent) {
            Ok(sess) => return Ok(sess),
            Err(error @ DaemonError::SpawnCleanupFailed { .. }) => return Err(error),
            Err(e) => {
                attempts.push((agent.clone(), e));
            }
        }
    }

    Err(if attempts.is_empty() {
        DaemonError::Config("No agents in fallback chain could be run".into())
    } else {
        DaemonError::SpawnFallbackExhausted(attempts)
    })
}

#[cfg(test)]
mod spawn_fallback_tests {
    use super::{build_runtime_fallback_chain, fallback_spawn};
    use crate::errors::DaemonError;
    use crate::tools::SessionId;

    // jleechan-9lvs r2 (CodeRabbit blocker on PR #362): the runtime --agent
    // argv must canonicalize through the SAME alias map the startup
    // preflight uses, otherwise preflight passes for `agy`->`antigravity`
    // and runtime still spawns `--agent agy` and reproduces the failure
    // mode this PR is meant to eliminate. This test exercises the helper
    // directly so we never have to spin up a subprocess to assert
    // argv-shape behavior.
    #[test]
    fn runtime_fallback_chain_never_emits_legacy_alias_after_agy_rename() {
        // Default `agy` (legacy alias) plus a fallback chain that names
        // both `agy` and `antigravity`. After canonicalization + dedup the
        // chain must contain ONLY canonical plugin names; no literal
        // `agy` or `aow` may survive.
        let chain = build_runtime_fallback_chain("agy", "antigravity->agy->aow->minimax");

        assert!(
            !chain.iter().any(|v| v == "agy"),
            "legacy alias `agy` must be canonicalized to `antigravity`; got: {chain:?}"
        );
        assert!(
            !chain.iter().any(|v| v == "aow"),
            "legacy alias `aow` must be canonicalized to `minimax`; got: {chain:?}"
        );
        // Both canonical plugins appear, order preserved (default first).
        assert_eq!(
            chain,
            vec!["antigravity".to_string(), "minimax".to_string()]
        );
    }

    #[test]
    fn runtime_fallback_chain_preserves_passthrough_when_no_alias_matches() {
        let chain = build_runtime_fallback_chain(
            "minimax",
            "claude-code->antigravity->agy",
        );
        assert_eq!(
            chain,
            vec![
                "minimax".to_string(),
                "claude-code".to_string(),
                "antigravity".to_string(),
            ]
        );
    }

    /// jleechan-r56m red proof: simulate all 3 vendors in a fallback chain
    /// failing with DIFFERENT, distinguishable errors. Today's aggregation
    /// (`last_err` overwritten each iteration) only surfaces the LAST
    /// vendor's error, so the first two vendors' distinguishing markers must
    /// NOT appear in the final error's `Display` output -- proving the bug
    /// bead jleechan-r56m describes: triage sees only "agy" and never learns
    /// minimax/claude-code failed for a different, possibly more important,
    /// reason.
    #[test]
    fn all_vendors_failing_must_surface_every_vendors_error_not_just_the_last() {
        let agents = vec![
            "minimax".to_string(),
            "claude-code".to_string(),
            "agy".to_string(),
        ];

        let err = fallback_spawn(&agents, |agent| {
            Err(match agent {
                "minimax" => DaemonError::Tool {
                    tool: "ao spawn --agent minimax".to_string(),
                    rc: 1,
                    stderr: "MINIMAX_AUTH_FAILURE_MARKER".to_string(),
                },
                "claude-code" => DaemonError::Tool {
                    tool: "ao spawn --agent claude-code".to_string(),
                    rc: 1,
                    stderr: "CLAUDE_CODE_SESSION_CAP_MARKER".to_string(),
                },
                "agy" => DaemonError::Tool {
                    tool: "ao spawn --agent agy".to_string(),
                    rc: 1,
                    stderr: "Agent plugin agy not found".to_string(),
                },
                other => panic!("unexpected agent {other}"),
            })
        })
        .expect_err("all three vendors fail in this scenario");

        let rendered = err.to_string();

        assert!(
            rendered.contains("MINIMAX_AUTH_FAILURE_MARKER"),
            "minimax's specific error must be visible in the aggregated error, got: {rendered}"
        );
        assert!(
            rendered.contains("CLAUDE_CODE_SESSION_CAP_MARKER"),
            "claude-code's specific error must be visible in the aggregated error, got: {rendered}"
        );
        assert!(
            rendered.contains("Agent plugin agy not found"),
            "agy's specific error must still be visible in the aggregated error, got: {rendered}"
        );
    }

    #[test]
    fn claude_auth_failure_falls_back_to_minimax() {
        let agents = vec![
            "claude-code".to_string(),
            "minimax".to_string(),
            "antigravity".to_string(),
        ];

        let session = fallback_spawn(&agents, |agent| {
            match agent {
                "claude-code" => Err(DaemonError::Tool {
                    tool: "ao spawn --agent claude-code".to_string(),
                    rc: 1,
                    stderr: "Failed to authenticate: OAuth session expired and could not be refreshed".to_string(),
                }),
                "minimax" => Ok(SessionId("wa-minimax-success".to_string())),
                other => panic!("unexpected agent {other}"),
            }
        })
        .expect("fallback to minimax must succeed when claude-code fails auth");

        assert_eq!(session, SessionId("wa-minimax-success".to_string()));
    }
}

#[cfg(test)]
mod spawn_classification_tests {
    use super::CliSessions;
    use crate::errors::DaemonError;

    /// jleechan-la67 live incident (2026-07-08): once
    /// `spawn-queue-worldarchitect.json` hit its 100-pending admission cap,
    /// `ao spawn` began failing rc=1 with stderr containing "Spawn queue is
    /// full for project 'worldarchitect' (100 pending requests)" — a plain
    /// thrown `Error` from `enqueueSpawnRequest` (packages/core/src/spawn-queue.ts)
    /// that re-propagates through commander's default handler. Before this
    /// fix, `classify_spawn_output` treated ANY nonzero exit as a genuine
    /// `DaemonError::Tool` vendor failure, indistinguishable from a real
    /// crash — which increments `spawn_failure_count` and, after
    /// `MAX_TRANSIENT_SPAWN_RETRY` cycles, parks the bead `HumanHeld` purely
    /// from admission backpressure that has nothing to do with the bead
    /// itself. This must classify as `Deferred` instead, exactly like the
    /// existing `REQUEST=` "at session cap" case, since no worktree/branch/
    /// process was ever created either way.
    #[test]
    fn queue_full_stderr_on_nonzero_exit_is_deferred_not_tool() {
        let err = CliSessions::classify_spawn_output(
            "claude-code",
            false,
            Some(1),
            "",
            "Error: Spawn queue is full for project 'worldarchitect' (100 pending requests)\n",
        )
        .unwrap_err();

        assert!(
            matches!(err, DaemonError::Deferred(_)),
            "queue-full rc=1 must classify as Deferred (retry-later), not a vendor Tool failure: {err:?}"
        );
        assert!(
            err.is_transient(),
            "Deferred must remain transient so dispatch_ready still requeues it"
        );
    }

    /// Matching is case-insensitive since the exact casing of AO's thrown
    /// error message is not a contract the daemon should be brittle to.
    #[test]
    fn queue_full_stderr_matching_is_case_insensitive() {
        let err = CliSessions::classify_spawn_output(
            "claude-code",
            false,
            Some(1),
            "",
            "SPAWN QUEUE IS FULL for project 'worldarchitect' (100 pending requests)",
        )
        .unwrap_err();

        assert!(matches!(err, DaemonError::Deferred(_)));
    }

    /// A genuine crash (e.g. `ao` binary missing, auth failure, unrelated
    /// CLI error) on a nonzero exit must still classify as a real `Tool`
    /// vendor failure — this fix must not blanket-suppress all spawn
    /// failures, only the specific admission-queue-full shape.
    #[test]
    fn unrelated_nonzero_exit_is_still_tool_failure() {
        let err = CliSessions::classify_spawn_output(
            "claude-code",
            false,
            Some(127),
            "",
            "ao: command not found",
        )
        .unwrap_err();

        match err {
            DaemonError::Tool { rc, stderr, .. } => {
                assert_eq!(rc, 127);
                assert!(stderr.contains("command not found"));
            }
            other => panic!("expected DaemonError::Tool for an unrelated failure, got {other:?}"),
        }
    }

    /// Existing REQUEST= behavior (jleechan-5ia2) must be unaffected by this
    /// refactor: a successful exit with no SESSION= line but a REQUEST= line
    /// still classifies as Deferred.
    #[test]
    fn request_line_on_success_exit_is_still_deferred() {
        let err = CliSessions::classify_spawn_output(
            "claude-code",
            true,
            Some(0),
            "REQUEST=sq-abc123\n",
            "",
        )
        .unwrap_err();

        assert!(matches!(err, DaemonError::Deferred(_)));
    }

    /// Existing SESSION= happy path must be unaffected by this refactor.
    #[test]
    fn session_line_on_success_exit_is_ok() {
        let session = CliSessions::classify_spawn_output(
            "claude-code",
            true,
            Some(0),
            "SESSION=abc-123\n",
            "",
        )
        .unwrap();

        assert_eq!(session.0, "abc-123");
    }

    /// Regression guard for the pre-existing fallthrough (unchanged by this
    /// refactor): a successful exit with neither a `SESSION=` nor a
    /// `REQUEST=` line is a genuine parse failure, not retry-safe.
    #[test]
    fn success_exit_with_no_session_or_request_line_is_parse_error() {
        let err = CliSessions::classify_spawn_output(
            "claude-code",
            true,
            Some(0),
            "some unexpected stdout with no marker line\n",
            "",
        )
        .unwrap_err();

        assert!(
            matches!(err, DaemonError::Parse(_)),
            "expected DaemonError::Parse for a success exit with no SESSION=/REQUEST= line, got {err:?}"
        );
    }
}

#[cfg(test)]
mod ao_spawn_contract_tests {
    use super::{ao_spawn_bridge_path, gh_env_test_lock, CliSessions};
    use crate::errors::DaemonError;
    use crate::tools::{Sessions, SpawnSpec};
    use std::os::unix::fs::PermissionsExt;

    fn spec(prompt: &str, branch: &str) -> SpawnSpec {
        SpawnSpec {
            bead_id: "jleechan-contract-test".to_string(),
            branch: branch.to_string(),
            prompt: prompt.to_string(),
            repo: "jleechanorg/dark-factory".to_string(),
            ao_project: "dark-factory".to_string(),
            remote: "origin".to_string(),
            local_checkout: Some(std::env::current_dir().unwrap()),
            expected_revision: None,
            managed_checkout: false,
            expected_cwd: None,
        }
    }

    fn bridge_test_node() -> std::path::PathBuf {
        if let Ok(node) = std::env::var("DARK_FACTORY_AO_BRIDGE_TEST_NODE") {
            return std::path::PathBuf::from(node);
        }
        let node22 = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
            .join(".nvm/versions/node/v22.22.0/bin/node");
        if node22.is_file() {
            node22
        } else {
            std::path::PathBuf::from("node")
        }
    }

    #[test]
    fn bridge_rejects_non_node22_even_with_legacy_test_bypass_environment() {
        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .args([
                "--input-type=module",
                "--eval",
                "Object.defineProperty(process.versions, 'node', {value: '24.15.0'}); const {pathToFileURL} = await import('node:url'); await import(pathToFileURL(process.argv[1]).href);",
            ])
            .arg(&bridge)
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env("NODE_ENV", "test")
            .env("DARK_FACTORY_AO_BRIDGE_ALLOW_TEST_NODE", "1")
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "legacy bypass unexpectedly worked");
        assert!(stderr.contains("requires Node 22, got 24.15.0"), "{stderr}");
    }

    fn fake_ao_dir(test_name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "afd_ao_spawn_contract_{test_name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("ao");
        std::fs::write(
            &fake,
            r#"#!/usr/bin/env python3
import json
import os
import sys

args = sys.argv[1:]
if args[:2] == ["session", "kill"] and len(args) == 3:
    with open(os.environ["AO_FAKE_LOG"], "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"kind": "kill", "args": args, "session": args[2]}) + "\n")
    if os.environ.get("AO_FAKE_KILL_FAIL") == "1":
        print("scripted batch cleanup failure", file=sys.stderr)
        raise SystemExit(8)
    raise SystemExit(0)
assert len(args) == 7, args
assert args[:5] == ["spawn", "--project", "dark-factory", "--agent", "minimax"], args
assert not {"--prompt", "--name", "--branch"}.intersection(args), args
assert args[5] == "--", args
prompt = args[6]
bindings = json.loads(os.environ["AO_FAKE_EXPECTED_BINDINGS"])
assert bindings[prompt] == os.environ["DARK_FACTORY_AO_SPAWN_BRANCH"]
assert os.environ["DARK_FACTORY_AO_V013_BRIDGE"] == "1"
assert "ao-spawn-v013-bridge.mjs" in os.environ["NODE_OPTIONS"]
with open(os.environ["AO_FAKE_LOG"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps({"kind": "spawn", "args": args, "branch": os.environ["DARK_FACTORY_AO_SPAWN_BRANCH"], "cwd": os.getcwd()}) + "\n")
if prompt == os.environ.get("AO_FAKE_FAIL_PROMPT"):
    print("scripted second spawn failure", file=sys.stderr)
    raise SystemExit(7)
print("SESSION=fake-" + str(abs(hash(prompt))))
print("  Worktree: " + os.environ.get("AO_FAKE_WORKTREE", "/tmp/fake-ao-worktree"))
print("  Branch:   " + os.environ.get("AO_FAKE_RETURN_BRANCH", os.environ["DARK_FACTORY_AO_SPAWN_BRANCH"]))
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake, permissions).unwrap();
        dir
    }

    fn with_fake_ao<T>(
        test_name: &str,
        bindings: serde_json::Value,
        run: impl FnOnce(&std::path::Path) -> T,
    ) -> T {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = fake_ao_dir(test_name);
        let log = dir.join("calls.jsonl");
        let old_path = std::env::var("PATH").unwrap_or_default();
        let old_bindings = std::env::var("AO_FAKE_EXPECTED_BINDINGS").ok();
        let old_log = std::env::var("AO_FAKE_LOG").ok();
        std::env::set_var("PATH", format!("{}:{old_path}", dir.display()));
        std::env::set_var("AO_FAKE_EXPECTED_BINDINGS", bindings.to_string());
        std::env::set_var("AO_FAKE_LOG", &log);

        let result = run(&log);

        std::env::set_var("PATH", old_path);
        match old_bindings {
            Some(value) => std::env::set_var("AO_FAKE_EXPECTED_BINDINGS", value),
            None => std::env::remove_var("AO_FAKE_EXPECTED_BINDINGS"),
        }
        match old_log {
            Some(value) => std::env::set_var("AO_FAKE_LOG", value),
            None => std::env::remove_var("AO_FAKE_LOG"),
        }
        let _ = std::fs::remove_dir_all(dir);
        result
    }

    fn run_bridge_with_registered_source(
        test_name: &str,
        managed_checkout: bool,
    ) -> (std::process::Output, String) {
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_managed_checkout_{test_name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let configured = root.join("registered-source");
        let target = root.join("managed-target");
        let calls = root.join("calls.log");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                "managed target",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let expected_revision = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&target)
            .output()
            .unwrap();
        let expected_revision = String::from_utf8(expected_revision.stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        let registered_source = configured.to_string_lossy();
        let target_source = target.to_string_lossy();
        std::fs::write(
            core.join("dist/index.js"),
            format!(
                r#"import {{appendFileSync}} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, value + '\n');
export const loadConfig = () => ({{configPath: '/tmp/fake-ao.yaml', projects: {{'worldarchitect': {{path: '{registered_source}'}}}}}});
export const createPluginRegistry = () => ({{loadFromConfig: async () => {{}}}});
export const createSessionManager = ({{config}}) => ({{
  list: async () => [],
  spawn: async (spec) => {{log(JSON.stringify({{path: config.projects.worldarchitect.path, branch: spec.branch}})); return {{id: 'managed-session', branch: spec.branch, workspacePath: '{target_source}'}}; }},
}});
export const acquireSpawnLock = () => ({{acquired: true, release() {{}}}});
export const resolveSpawnQueueConfig = () => ({{enabled: true, maxActiveSessions: 2}});
export const isTerminalSession = () => false;
"#
            ),
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['worldarchitect']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let mut command = std::process::Command::new(bridge_test_node());
        command
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "managed checkout probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env("DARK_FACTORY_AO_SPAWN_BRANCH", "factory/managed-checkout-r1")
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &expected_revision)
            .env("AO_FAKE_CALLS", &calls);
        if managed_checkout {
            command.env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1");
        }
        let output = command.output().unwrap();
        let logged = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(root);
        (output, logged)
    }

    #[test]
    fn managed_target_checkout_overrides_registered_ao_source() {
        let (output, calls) = run_bridge_with_registered_source("managed", true);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "managed checkout bridge failed; stdout={stdout}; stderr={stderr}"
        );
        assert!(stdout.contains("SESSION=managed-session"), "{stdout}");
        assert!(
            calls.contains("managed-target"),
            "AO must receive the validated managed target path: {calls}"
        );
        assert!(
            !calls.contains("registered-source"),
            "AO must not use its stale registered source path: {calls}"
        );
    }

    #[test]
    fn configured_checkout_source_mismatch_still_fails_closed() {
        let (output, calls) = run_bridge_with_registered_source("configured", false);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "mismatched configured checkout unexpectedly spawned");
        assert!(
            stderr.contains("does not match validated target checkout"),
            "stdout={stdout}; stderr={stderr}"
        );
        assert!(calls.is_empty(), "AO must not be invoked on a source mismatch: {calls}");
    }

    /// Regression for bead dark-factory-ik0v / incident jleechan-j9id.
    ///
    /// AO's `workspace-worktree.create` always pins `baseRef =
    /// origin/${project.defaultBranch}` and runs
    /// `git worktree add -b <branch> <path> <baseRef>`. For an adopted
    /// remediation spawn — where the contributor's branch already lives on
    /// origin and the daemon has a validated `expected_revision` (= PR head)
    /// — that creates the local branch at the wrong commit
    /// (`origin/<defaultBranch>`) instead of the PR head.
    ///
    /// The bridge must detect the adopted case (`origin/<branch>` exists at
    /// `expected_revision`) and rebind `projectConfig.defaultBranch = branch`
    /// so AO's `git worktree add -b <branch> <path> origin/<branch>` lands
    /// the worktree at the PR head.
    fn run_bridge_with_adopted_target(test_name: &str) -> (std::process::Output, String) {
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_adopted_target_{test_name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let remote = root.join("remote.git");
        let source = root.join("source");
        let target = root.join("adopted-target");
        let calls = root.join("calls.log");
        let pr_branch = format!("alice/{test_name}-pr");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_str = source.to_string_lossy().into_owned();
        let target_str = target.to_string_lossy().into_owned();

        // Bare remote.
        std::process::Command::new("git")
            .args(["init", "-q", "--bare", remote.to_str().unwrap()])
            .status()
            .unwrap();

        // Source repo: two commits. main at A, contributor branch at B.
        for (cmd_dir, _) in [(&source, &source as &std::path::Path)] {
            let _ = cmd_dir;
        }
        let commit_a = || -> String {
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&source)
                .status()
                .unwrap();
            std::process::Command::new("git")
                .args(["remote", "add", "origin", remote.to_str().unwrap()])
                .current_dir(&source)
                .status()
                .unwrap();
            std::process::Command::new("git")
                .args([
                    "-c", "user.email=test@example.invalid",
                    "-c", "user.name=Test",
                    "commit", "--allow-empty", "-m", "A",
                ])
                .current_dir(&source)
                .status()
                .unwrap();
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let sha_a = commit_a();
        std::process::Command::new("git")
            .args(["checkout", "-q", "-b", "main"])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-q", "origin", "main:main"])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["checkout", "-q", "-b", &pr_branch])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c", "user.email=test@example.invalid",
                "-c", "user.name=Test",
                "commit", "--allow-empty", "-m", "B",
            ])
            .current_dir(&source)
            .status()
            .unwrap();
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap();
        let sha_b = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_ne!(sha_a, sha_b);
        std::process::Command::new("git")
            .args(["push", "-q", "origin", &format!("{pr_branch}:{pr_branch}")])
            .current_dir(&source)
            .status()
            .unwrap();

        // Target clone, then detach HEAD at sha_b (= daemon-validated PR head).
        std::process::Command::new("git")
            .args(["clone", "-q", remote.to_str().unwrap(), target.to_str().unwrap()])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["checkout", "-q", "--detach", &sha_b])
            .current_dir(&target)
            .status()
            .unwrap();

        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            format!(
                r#"import {{appendFileSync}} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, value + '\n');
const project = 'worldarchitect';
export const loadConfig = () => ({{configPath: '/tmp/fake-ao.yaml', projects: {{[project]: {{path: '{source_str}', defaultBranch: 'main'}}}}}});
export const createPluginRegistry = () => ({{loadFromConfig: async () => {{}}}});
export const createSessionManager = ({{config}}) => {{
  log('config-defaultBranch=' + config.projects[project].defaultBranch);
  return {{
    list: async () => [],
    spawn: async (spec) => {{
      log(JSON.stringify({{defaultBranch: config.projects[project].defaultBranch, branch: spec.branch, projectId: spec.projectId}}));
      return {{id: 'adopted-session', branch: spec.branch, workspacePath: '{target_str}'}};
    }},
  }};
}};
export const acquireSpawnLock = () => ({{acquired: true, release() {{}}}});
export const resolveSpawnQueueConfig = () => ({{enabled: true, maxActiveSessions: 2}});
export const isTerminalSession = () => false;
"#
            ),
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['worldarchitect']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let mut command = std::process::Command::new(bridge_test_node());
        command
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "adopted-pr probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env("DARK_FACTORY_AO_SPAWN_BRANCH", &pr_branch)
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &sha_b)
            .env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1")
            .env("AO_FAKE_CALLS", &calls);
        let output = command.output().unwrap();
        let logged = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(root);
        (output, logged)
    }

    #[test]
    fn bridge_pins_default_branch_to_adopted_pr_head_when_origin_ref_matches_expected_revision()
     {
        let (output, calls) = run_bridge_with_adopted_target("adopted_match");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "adopted-PR bridge failed; stdout={stdout}; stderr={stderr}; calls={calls}"
        );
        assert!(stdout.contains("SESSION=adopted-session"), "{stdout}");
        // The bridge must rebind `projectConfig.defaultBranch` so AO's
        // `origin/${defaultBranch}` resolves to the PR head (origin/<branch>),
        // NOT the repo's default `main`.
        let spawn_line = calls
            .lines()
            .find_map(|line| {
                let value = line.trim();
                if value.starts_with('{') && value.contains("\"branch\"") {
                    Some(value)
                } else {
                    None
                }
            })
            .expect("spawn call was not recorded");
        let observed: serde_json::Value =
            serde_json::from_str(spawn_line).expect("spawn log line must be JSON");
        assert_eq!(
            observed["defaultBranch"].as_str(),
            Some("alice/adopted_match-pr"),
            "AO must receive the PR branch as defaultBranch so origin/<defaultBranch> \
             resolves to the PR head; calls={calls}"
        );
        assert_eq!(observed["branch"].as_str(), Some("alice/adopted_match-pr"));
        assert_eq!(observed["projectId"].as_str(), Some("worldarchitect"));
    }

    #[test]
    fn bridge_fails_closed_when_adopted_origin_ref_head_diverges_from_expected_revision() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Build a remote whose `alice/<branch>` is at SHA X, but pass
        // expected_revision = Y (X != Y). The bridge must refuse to spawn:
        // rebinding defaultBranch would still land AO at X (the stale
        // remote ref), which is the exact drift this fix prevents.
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_adopted_target_diverged_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let remote = root.join("remote.git");
        let source = root.join("source");
        let target = root.join("adopted-target");
        let calls = root.join("calls.log");
        let pr_branch = "alice/diverged-pr";
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source_str = source.to_string_lossy().into_owned();
        let target_str = target.to_string_lossy().into_owned();

        std::process::Command::new("git")
            .args(["init", "-q", "--bare", remote.to_str().unwrap()])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", remote.to_str().unwrap()])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c", "user.email=test@example.invalid",
                "-c", "user.name=Test",
                "commit", "--allow-empty", "-m", "X",
            ])
            .current_dir(&source)
            .status()
            .unwrap();
        let sha_x = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::process::Command::new("git")
            .args(["checkout", "-q", "-b", "main"])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-q", "origin", "main:main"])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["checkout", "-q", "-b", pr_branch])
            .current_dir(&source)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-q", "origin", &format!("{pr_branch}:{pr_branch}")])
            .current_dir(&source)
            .status()
            .unwrap();

        // Target clone, then create an UNRELATED commit Y on the same branch.
        std::process::Command::new("git")
            .args(["clone", "-q", remote.to_str().unwrap(), target.to_str().unwrap()])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["checkout", "-q", "-b", pr_branch])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c", "user.email=test@example.invalid",
                "-c", "user.name=Test",
                "commit", "--allow-empty", "-m", "Y",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let sha_y = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&target)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert_ne!(sha_x, sha_y, "test setup must produce two distinct SHAs");

        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            format!(
                r#"import {{appendFileSync}} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, value + '\n');
export const loadConfig = () => ({{configPath: '/tmp/fake-ao.yaml', projects: {{'worldarchitect': {{path: '{source_str}', defaultBranch: 'main'}}}}}});
export const createPluginRegistry = () => ({{loadFromConfig: async () => {{}}}});
export const createSessionManager = () => ({{
  list: async () => [],
  spawn: async (spec) => {{log('SPAWN_CALLED'); return {{id: 'never', branch: spec.branch, workspacePath: '{target_str}'}}; }},
}});
export const acquireSpawnLock = () => ({{acquired: true, release() {{}}}});
export const resolveSpawnQueueConfig = () => ({{enabled: true, maxActiveSessions: 2}});
export const isTerminalSession = () => false;
"#
            ),
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['worldarchitect']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "diverged probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env("DARK_FACTORY_AO_SPAWN_BRANCH", pr_branch)
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &sha_y)
            .env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1")
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();
        let logged = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(root);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "diverged adopted-PR spawn unexpectedly succeeded; stderr={stderr}; calls={logged}"
        );
        assert!(
            stderr.contains("origin/") || stderr.contains("PR head") || stderr.contains("head"),
            "diverged adopted-PR must fail closed with a clear origin/head diagnostic; \
             stderr={stderr}; calls={logged}"
        );
        assert!(
            !logged.contains("SPAWN_CALLED"),
            "AO must not be invoked when origin ref diverges from expected revision: {logged}"
        );
    }

    #[test]
    fn single_spawn_uses_v013_positional_prompt_and_exact_branch_binding() {
        let prompt = "single prompt with spaces\nand a second line";
        let branch = "factory/jleechan-contract-single-r1";
        let bindings = serde_json::json!({ prompt: branch });

        let (spawn_result, calls) = with_fake_ao("single", bindings, |log| {
            let saved = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn(&spec(prompt, branch));
            match saved {
                Some(val) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", val),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
            }
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        assert!(
            spawn_result.is_ok(),
            "single spawn failed: {spawn_result:?}"
        );
        let rows: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 1, "expected exactly one AO invocation: {calls}");
        assert_eq!(rows[0]["args"][5], "--");
        assert_eq!(rows[0]["args"][6], prompt);
        assert_eq!(rows[0]["branch"], branch);
    }

    #[test]
    fn worker_spawn_without_checkout_fails_closed_before_ao() {
        let prompt = "missing checkout prompt";
        let branch = "factory/jleechan-contract-missing-checkout-r1";
        let (spawn_result, calls) = with_fake_ao(
            "missing_checkout",
            serde_json::json!({prompt: branch}),
            |log| {
                let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
                let mut missing = spec(prompt, branch);
                missing.local_checkout = None;
                let result = sessions.spawn(&missing);
                let calls = std::fs::read_to_string(log).unwrap_or_default();
                (result, calls)
            },
        );
        let error = spawn_result.expect_err("missing checkout must fail closed");
        assert!(error.to_string().contains("no target checkout"), "{error}");
        assert!(calls.is_empty(), "AO must not be invoked without a checkout");
    }

    #[test]
    fn worker_spawn_rejects_stale_expected_revision_before_ao() {
        let prompt = "stale revision prompt";
        let branch = "factory/jleechan-contract-stale-revision-r1";
        let (spawn_result, calls) = with_fake_ao(
            "stale_revision",
            serde_json::json!({prompt: branch}),
            |log| {
                let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
                let mut stale = spec(prompt, branch);
                stale.expected_revision = Some("0000000000000000000000000000000000000000".into());
                let result = sessions.spawn(&stale);
                let calls = std::fs::read_to_string(log).unwrap_or_default();
                (result, calls)
            },
        );
        let error = spawn_result.expect_err("stale checkout must fail closed");
        assert!(error.to_string().contains("expected snapshot"), "{error}");
        assert!(calls.is_empty(), "AO must not be invoked on a stale checkout");
    }

    #[test]
    fn worker_spawn_rejects_same_origin_stale_ao_workspace_after_spawn() {
        let prompt = "same origin stale AO workspace";
        let branch = "factory/jleechan-contract-stale-ao-workspace-r1";
        let checkout = std::env::current_dir().unwrap();
        let expected_revision = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&checkout)
            .output()
            .unwrap();
        let expected_revision = String::from_utf8(expected_revision.stdout)
            .unwrap()
            .trim()
            .to_string();
        let workspace = std::env::temp_dir().join(format!(
            "afd_stale_ao_workspace_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&workspace)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/jleechanorg/dark-factory.git",
            ])
            .current_dir(&workspace)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "stale",
            ])
            .current_dir(&workspace)
            .status()
            .unwrap();

        let (spawn_result, calls) = with_fake_ao(
            "stale_ao_workspace",
            serde_json::json!({prompt: branch}),
            |log| {
                let previous = std::env::var("AO_FAKE_WORKTREE").ok();
                std::env::set_var("AO_FAKE_WORKTREE", &workspace);
                let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
                let mut routed = spec(prompt, branch);
                routed.local_checkout = Some(checkout.clone());
                routed.expected_revision = Some(expected_revision.clone());
                let result = sessions.spawn(&routed);
                match previous {
                    Some(value) => std::env::set_var("AO_FAKE_WORKTREE", value),
                    None => std::env::remove_var("AO_FAKE_WORKTREE"),
                }
                (result, std::fs::read_to_string(log).unwrap_or_default())
            },
        );
        let _ = std::fs::remove_dir_all(&workspace);

        let error = spawn_result.expect_err("same-origin stale AO workspace must fail closed");
        assert!(error.to_string().contains("expected snapshot"), "{error}");
        let rows: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.iter().filter(|row| row["kind"] == "spawn").count(), 1);
        assert_eq!(rows.iter().filter(|row| row["kind"] == "kill").count(), 1);
    }

    #[test]
    fn routed_spawn_uses_target_checkout_as_ao_cwd() {
        let prompt = "routed checkout prompt";
        let branch = "factory/jleechan-contract-checkout-r1";
        let checkout = std::env::temp_dir().join(format!(
            "afd_target_checkout_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&checkout).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://github.com/otherorg/other-repo.git"])
            .current_dir(&checkout)
            .status()
            .unwrap();

        let (spawn_result, calls) = with_fake_ao("target_checkout", serde_json::json!({prompt: branch}), |log| {
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let mut routed = spec(prompt, branch);
            routed.repo = "otherorg/other-repo".to_string();
            routed.local_checkout = Some(checkout.clone());
            let result = sessions.spawn(&routed);
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });
        assert!(spawn_result.is_ok(), "routed spawn failed: {spawn_result:?}");
        let row: serde_json::Value = calls.lines().next().unwrap().parse().unwrap();
        assert_eq!(row["cwd"], checkout.canonicalize().unwrap().to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(&checkout);
    }

    #[test]
    fn bridge_branch_mismatch_is_killed_and_never_returned_as_a_session() {
        let prompt = "branch mismatch prompt";
        let branch = "factory/jleechan-contract-branch-mismatch-r1";
        let bindings = serde_json::json!({ prompt: branch });

        let (spawn_result, calls) = with_fake_ao("branch_mismatch", bindings, |log| {
            let saved = [
                ("AO_FAKE_RETURN_BRANCH", std::env::var("AO_FAKE_RETURN_BRANCH").ok()),
                (
                    "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN",
                    std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok(),
                ),
            ];
            std::env::set_var("AO_FAKE_RETURN_BRANCH", "factory/unexpected-r1");
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn(&spec(prompt, branch));
            for (key, value) in saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        let error = spawn_result.expect_err("a mismatched Branch echo must fail closed");
        assert!(error.to_string().contains("branch-mismatched worker"));
        let rows: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.iter().filter(|row| row["kind"] == "spawn").count(),
            1
        );
        assert_eq!(
            rows.iter().filter(|row| row["kind"] == "kill").count(),
            1
        );
    }

    #[test]
    fn batch_spawn_uses_v013_contract_for_every_item() {
        let first = spec(
            "batch prompt alpha",
            "factory/jleechan-contract-batch-alpha-r1",
        );
        let second = spec(
            "batch prompt beta",
            "factory/jleechan-contract-batch-beta-r1",
        );
        let bindings = serde_json::json!({
            first.prompt.clone(): first.branch.clone(),
            second.prompt.clone(): second.branch.clone(),
        });

        let (spawn_result, calls) = with_fake_ao("batch", bindings, |log| {
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn_batch(&[first.clone(), second.clone()]);
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        assert!(spawn_result.is_ok(), "batch spawn failed: {spawn_result:?}");
        let rows: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.len(),
            2,
            "expected one AO invocation per spec: {calls}"
        );
        let observed: std::collections::HashMap<&str, &str> = rows
            .iter()
            .map(|row| {
                (
                    row["args"][6].as_str().unwrap(),
                    row["branch"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            observed.get(first.prompt.as_str()),
            Some(&first.branch.as_str())
        );
        assert_eq!(
            observed.get(second.prompt.as_str()),
            Some(&second.branch.as_str())
        );
    }

    #[test]
    fn batch_spawn_failure_kills_every_prior_session() {
        let first = spec(
            "batch succeeds first",
            "factory/jleechan-contract-batch-cleanup-first-r1",
        );
        let second = spec(
            "batch fails second",
            "factory/jleechan-contract-batch-cleanup-second-r1",
        );
        let bindings = serde_json::json!({
            first.prompt.clone(): first.branch.clone(),
            second.prompt.clone(): second.branch.clone(),
        });

        let (batch_result, calls) = with_fake_ao("batch_cleanup", bindings, |log| {
            let old_fail_prompt = std::env::var("AO_FAKE_FAIL_PROMPT").ok();
            let old_fallback = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
            std::env::set_var("AO_FAKE_FAIL_PROMPT", &second.prompt);
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn_batch(&[first.clone(), second.clone()]);
            match old_fail_prompt {
                Some(value) => std::env::set_var("AO_FAKE_FAIL_PROMPT", value),
                None => std::env::remove_var("AO_FAKE_FAIL_PROMPT"),
            }
            match old_fallback {
                Some(value) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", value),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
            }
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        assert!(batch_result.is_err(), "second spawn must fail");
        let rows: Vec<serde_json::Value> = calls.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let successful_spawns: Vec<&serde_json::Value> = rows.iter()
            .filter(|row| row["kind"] == "spawn" && row["args"][6] == first.prompt)
            .collect();
        let kills: Vec<&serde_json::Value> = rows.iter()
            .filter(|row| row["kind"] == "kill")
            .collect();
        assert_eq!(successful_spawns.len(), 1, "calls={calls}");
        assert_eq!(kills.len(), successful_spawns.len(), "calls={calls}");
        assert_eq!(
            kills[0]["args"],
            serde_json::json!(["session", "kill", kills[0]["session"]])
        );
    }

    #[test]
    fn batch_spawn_reports_root_and_cleanup_failures() {
        let first = spec(
            "batch cleanup failure first",
            "factory/jleechan-contract-batch-cleanup-error-first-r1",
        );
        let second = spec(
            "batch cleanup failure second",
            "factory/jleechan-contract-batch-cleanup-error-second-r1",
        );
        let bindings = serde_json::json!({
            first.prompt.clone(): first.branch.clone(),
            second.prompt.clone(): second.branch.clone(),
        });

        let (batch_result, calls) = with_fake_ao("batch_cleanup_error", bindings, |log| {
            let old_fail_prompt = std::env::var("AO_FAKE_FAIL_PROMPT").ok();
            let old_kill_fail = std::env::var("AO_FAKE_KILL_FAIL").ok();
            let old_fallback = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
            std::env::set_var("AO_FAKE_FAIL_PROMPT", &second.prompt);
            std::env::set_var("AO_FAKE_KILL_FAIL", "1");
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn_batch(&[first.clone(), second.clone()]);
            for (key, old) in [
                ("AO_FAKE_FAIL_PROMPT", old_fail_prompt),
                ("AO_FAKE_KILL_FAIL", old_kill_fail),
                ("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", old_fallback),
            ] {
                match old {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        let error = batch_result.expect_err("second spawn and cleanup must fail");
        let rendered = error.to_string();
        let cleanup_errors = match &error {
            DaemonError::SpawnBatchCleanupFailed { cleanup_errors, .. } => cleanup_errors,
            other => panic!("expected typed batch cleanup failure, got {other:?}"),
        };
        assert_eq!(cleanup_errors.len(), 1);
        assert_eq!(cleanup_errors[0].bead_id, first.bead_id);
        assert_eq!(cleanup_errors[0].branch, first.branch);
        assert!(rendered.contains("scripted second spawn failure"), "{rendered}");
        assert!(rendered.contains("scripted batch cleanup failure"), "{rendered}");
        assert!(rendered.contains("jleechan-contract-test"), "{rendered}");
        assert!(
            rendered.contains("factory/jleechan-contract-batch-cleanup-error-first-r1"),
            "{rendered}"
        );
        let rows: Vec<serde_json::Value> = calls.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.iter().filter(|row| row["kind"] == "kill").count(),
            1,
            "calls={calls}"
        );
    }

    #[test]
    fn prompt_beginning_with_dash_remains_positional() {
        let prompt = "--not-an-ao-option preserve this prompt verbatim";
        let branch = "factory/jleechan-contract-dash-prompt-r1";
        let bindings = serde_json::json!({ prompt: branch });

        let (spawn_result, calls) = with_fake_ao("dash_prompt", bindings, |log| {
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn(&spec(prompt, branch));
            let calls = std::fs::read_to_string(log).unwrap_or_default();
            (result, calls)
        });

        assert!(spawn_result.is_ok(), "dash prompt failed: {spawn_result:?}");
        let row: serde_json::Value = serde_json::from_str(calls.lines().next().unwrap()).unwrap();
        assert_eq!(row["args"][5], "--");
        assert_eq!(row["args"][6], prompt);
        assert_eq!(row["branch"], branch);
    }

    #[test]
    fn missing_worktree_uses_session_kill_cleanup_contract() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_missing_worktree_cleanup_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("calls.jsonl");
        let fake_ao = root.join("ao");
        std::fs::write(
            &fake_ao,
            r#"#!/usr/bin/env python3
import json
import os
import sys
with open(os.environ["AO_FAKE_CLEANUP_LOG"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(sys.argv[1:]) + "\n")
if sys.argv[1] == "spawn":
    print("SESSION=missing-worktree-session")
    raise SystemExit(0)
if sys.argv[1:] == ["session", "kill", "missing-worktree-session"]:
    raise SystemExit(0)
raise SystemExit(9)
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_ao).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_ao, permissions).unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        let old_log = std::env::var("AO_FAKE_CLEANUP_LOG").ok();
        let old_fallback = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
        std::env::set_var("PATH", format!("{}:{old_path}", root.display()));
        std::env::set_var("AO_FAKE_CLEANUP_LOG", &log);
        std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");

        let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
        let result = sessions.spawn(&spec(
            "missing workspace cleanup",
            "factory/missing-worktree-cleanup-r1",
        ));
        std::env::set_var("PATH", old_path);
        match old_log {
            Some(value) => std::env::set_var("AO_FAKE_CLEANUP_LOG", value),
            None => std::env::remove_var("AO_FAKE_CLEANUP_LOG"),
        }
        match old_fallback {
            Some(value) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", value),
            None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
        }
        let calls: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let _ = std::fs::remove_dir_all(&root);

        assert!(result.is_err(), "missing Worktree must fail closed");
        assert_eq!(
            calls.last().unwrap(),
            &serde_json::json!(["session", "kill", "missing-worktree-session"])
        );
        assert!(!calls.iter().any(|call| call == &serde_json::json!(["stop", "missing-worktree-session"])));
    }

    #[test]
    fn missing_worktree_kill_failure_does_not_spawn_fallback_vendor() {
        let _guard = gh_env_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_missing_worktree_kill_failure_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("calls.jsonl");
        let fake_ao = root.join("ao");
        std::fs::write(
            &fake_ao,
            r#"#!/usr/bin/env python3
import json
import os
import sys
with open(os.environ["AO_FAKE_CLEANUP_LOG"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(sys.argv[1:]) + "\n")
if sys.argv[1] == "spawn":
    print("SESSION=untracked-session")
    raise SystemExit(0)
if sys.argv[1:] == ["session", "kill", "untracked-session"]:
    print("scripted kill failure", file=sys.stderr)
    raise SystemExit(8)
raise SystemExit(9)
"#,
        ).unwrap();
        let mut permissions = std::fs::metadata(&fake_ao).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_ao, permissions).unwrap();
        let old_path = std::env::var("PATH").unwrap_or_default();
        let old_log = std::env::var("AO_FAKE_CLEANUP_LOG").ok();
        let old_fallback = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
        std::env::set_var("PATH", format!("{}:{old_path}", root.display()));
        std::env::set_var("AO_FAKE_CLEANUP_LOG", &log);
        std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax->claude-code");

        let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
        let result = sessions.spawn(&spec(
            "missing workspace kill failure",
            "factory/missing-worktree-kill-failure-r1",
        ));
        std::env::set_var("PATH", old_path);
        match old_log {
            Some(value) => std::env::set_var("AO_FAKE_CLEANUP_LOG", value),
            None => std::env::remove_var("AO_FAKE_CLEANUP_LOG"),
        }
        match old_fallback {
            Some(value) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", value),
            None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
        }
        let calls: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let _ = std::fs::remove_dir_all(&root);

        assert!(matches!(result, Err(DaemonError::SpawnCleanupFailed { .. })));
        assert_eq!(
            calls.iter().filter(|call| call[0] == "spawn").count(),
            1,
            "cleanup failure must stop fallback dispatch: {calls:?}"
        );
        assert_eq!(
            calls.last().unwrap(),
            &serde_json::json!(["session", "kill", "untracked-session"])
        );
    }

    #[test]
    fn spawned_opaque_workspace_is_used_for_remote_validation() {
        let prompt = "opaque workspace validation prompt";
        let branch = "factory/jleechan-contract-opaque-r1";
        let bindings = serde_json::json!({ prompt: branch });
        let (spawn_result, remote_result) = with_fake_ao("opaque", bindings, |log| {
            let saved_chain = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax");
            let workspace = log.parent().unwrap().join("df-opaque-134");
            std::fs::create_dir_all(&workspace).unwrap();
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&workspace)
                .status()
                .unwrap();
            std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/wrong-owner/wrong-repo.git",
                ])
                .current_dir(&workspace)
                .status()
                .unwrap();
            let previous_workspace = std::env::var("AO_FAKE_WORKTREE").ok();
            std::env::set_var("AO_FAKE_WORKTREE", &workspace);
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let spawn_result = sessions.spawn(&spec(prompt, branch));
            let remote_result =
                sessions.worktree_remote_url("dark-factory", branch, "origin");
            match previous_workspace {
                Some(value) => std::env::set_var("AO_FAKE_WORKTREE", value),
                None => std::env::remove_var("AO_FAKE_WORKTREE"),
            }
            match saved_chain {
                Some(val) => std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", val),
                None => std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"),
            }
            (spawn_result, remote_result)
        });

        assert!(spawn_result.is_ok(), "spawn failed: {spawn_result:?}");
        assert_eq!(
            remote_result.unwrap().as_deref(),
            Some("https://github.com/wrong-owner/wrong-repo.git")
        );
    }

    #[test]
    fn bridge_resolves_import_only_ao_core_from_hoisted_node_modules() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_hoisted_resolution_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            r#"export const loadConfig = () => ({configPath: "/tmp/fake-ao.yaml", projects: {"dark-factory": {path: "/tmp/fake-project"}}});
export const createPluginRegistry = () => ({loadFromConfig: async () => {}});
export const createSessionManager = () => ({});
export const acquireSpawnLock = () => ({acquired: true, release() {}});
export const resolveSpawnQueueConfig = () => ({enabled: true, maxActiveSessions: 20});
export const isTerminalSession = () => false;
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['dark-factory']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "dark-factory",
                "--agent",
                "minimax",
                "--dark-factory-read-only-diagnostic",
                "--",
                "hoisted resolution probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env("DARK_FACTORY_AO_BRIDGE_DIAGNOSTIC", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "factory/hoisted-resolution-r1",
            )
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(root);

        assert!(
            output.status.success(),
            "bridge failed to resolve import-only hoisted AO core; stdout={stdout}; stderr={stderr}"
        );
        assert!(stdout.contains("AO_BRIDGE_DIAGNOSTIC="), "stdout={stdout}");
    }

    #[test]
    fn bridge_runs_v013_guards_and_defers_at_active_cap_without_spawning() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_admission_semantics_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let calls = root.join("calls.log");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            r#"import {appendFileSync} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, value + '\n');
export const loadConfig = () => ({configPath: '/tmp/fake-ao.yaml', defaults: {runtime: 'tmux'}, projects: {'dark-factory': {path: '/tmp/fake-project', tracker: {plugin: 'github'}}}});
export const createPluginRegistry = () => ({loadFromConfig: async () => {}});
export const createSessionManager = () => ({
  list: async () => {log('list'); return [{status: 'working'}];},
  spawn: async () => {log('SPAWN_CALLED'); throw new Error('spawn must not run at cap');},
});
export const acquireSpawnLock = () => {log('lock'); return {acquired: true, release() {log('release');}}};
export const resolveSpawnQueueConfig = () => ({enabled: true, maxActiveSessions: 1});
export const isTerminalSession = () => false;
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            r#"import {appendFileSync} from 'node:fs';
const log = (v) => appendFileSync(process.env.AO_FAKE_CALLS, v + '\n');
export const preflight = {checkTmux: async () => log('tmux'), checkGhAuth: async () => log('gh')};
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            r#"import {appendFileSync} from 'node:fs';
export const getRunning = async () => {appendFileSync(process.env.AO_FAKE_CALLS, 'running\n'); return {projects: ['dark-factory']}};
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            r#"import {appendFileSync} from 'node:fs';
export const ensureLifecycleWorker = async () => appendFileSync(process.env.AO_FAKE_CALLS, 'lifecycle\n');
"#,
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "dark-factory",
                "--agent",
                "minimax",
                "--",
                "admission probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "factory/admission-probe-r1",
            )
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let calls = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(root);

        assert!(
            output.status.success(),
            "cap deferral failed; stdout={stdout}; stderr={stderr}; calls={calls}"
        );
        assert!(stdout.contains("REQUEST=dark-factory-exact-branch-dark-factory"));
        for expected in ["tmux", "gh", "running", "lifecycle", "lock", "list", "release"] {
            assert!(calls.lines().any(|line| line == expected), "missing {expected}: {calls}");
        }
        assert!(!calls.contains("SPAWN_CALLED"), "{calls}");
    }

    #[test]
    fn bridge_sanitizes_prompt_preserves_branch_and_cleans_worker_environment() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_spawn_semantics_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let calls = root.join("calls.log");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            r#"import {appendFileSync} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, value + '\n');
export const loadConfig = () => ({configPath: '/tmp/fake-ao.yaml', defaults: {runtime: 'tmux'}, projects: {'dark-factory': {path: '/tmp/fake-project', tracker: {plugin: 'github'}}}});
export const createPluginRegistry = () => ({loadFromConfig: async () => {}});
export const createSessionManager = () => ({
  list: async () => {log('list'); return [];},
  spawn: async (spec) => {
    log('spawn=' + JSON.stringify({
      spec,
      nodeOptions: process.env.NODE_OPTIONS ?? null,
      bridge: process.env.DARK_FACTORY_AO_V013_BRIDGE ?? null,
      branchEnv: process.env.DARK_FACTORY_AO_SPAWN_BRANCH ?? null,
      parentNodeOptions: process.env.DARK_FACTORY_AO_PARENT_NODE_OPTIONS ?? null,
    }));
    return {id: 'spawn-semantic-session', branch: spec.branch, workspacePath: '/tmp/fake-worktree'};
  },
});
export const acquireSpawnLock = () => {log('lock'); return {acquired: true, release() {log('release');}}};
export const resolveSpawnQueueConfig = () => ({enabled: true, maxActiveSessions: 2});
export const isTerminalSession = () => false;
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            r#"export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['dark-factory']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "dark-factory",
                "--agent",
                "minimax",
                "--",
                "  hello\r\nworld  ",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--trace-warnings --experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "--trace-warnings")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "factory/spawn-semantics-r1",
            )
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let calls = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(root);

        assert!(
            output.status.success(),
            "below-cap spawn failed; stdout={stdout}; stderr={stderr}; calls={calls}"
        );
        assert!(stdout.contains("SESSION=spawn-semantic-session"), "{stdout}");
        let spawn_line = calls
            .lines()
            .find_map(|line| line.strip_prefix("spawn="))
            .expect("spawn call was not recorded");
        let observed: serde_json::Value = serde_json::from_str(spawn_line).unwrap();
        assert_eq!(observed["spec"]["prompt"], "hello  world");
        assert_eq!(observed["spec"]["branch"], "factory/spawn-semantics-r1");
        assert_eq!(observed["spec"]["projectId"], "dark-factory");
        assert_eq!(observed["spec"]["agent"], "minimax");
        assert_eq!(observed["nodeOptions"], "--trace-warnings");
        assert!(observed["bridge"].is_null());
        assert!(observed["branchEnv"].is_null());
        assert!(observed["parentNodeOptions"].is_null());
        assert_eq!(calls.lines().filter(|line| *line == "release").count(), 1);
    }

    #[test]
    fn bridge_backed_batch_and_invalid_workspace_cleanup_are_fail_closed() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_batch_semantics_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let calls = root.join("calls.jsonl");
        let lock = root.join("spawn.lock");
        let first_workspace = root.join("df-batch-alpha");
        let second_workspace = root.join("df-batch-beta");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        for workspace in [&first_workspace, &second_workspace] {
            std::fs::create_dir_all(workspace).unwrap();
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(workspace)
                .status()
                .unwrap();
            std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/jleechanorg/dark-factory.git",
                ])
                .current_dir(workspace)
                .status()
                .unwrap();
        }
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();
        std::fs::write(
            core.join("dist/index.js"),
            r#"import {appendFileSync, closeSync, openSync, rmSync} from 'node:fs';
const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, JSON.stringify(value) + '\n');
export const loadConfig = () => ({configPath: '/tmp/fake-ao.yaml', defaults: {runtime: 'tmux'}, projects: {'dark-factory': {path: '/tmp/fake-project'}}});
export const createPluginRegistry = () => ({loadFromConfig: async () => {}});
export const createSessionManager = () => ({
  list: async () => [],
  spawn: async (spec) => {
    log({kind: 'spawn', agent: spec.agent, branch: spec.branch, prompt: spec.prompt});
    await new Promise((resolve) => setTimeout(resolve, 200));
    const workspaces = JSON.parse(process.env.AO_FAKE_WORKSPACES);
    const workspacePath = spec.prompt === process.env.AO_FAKE_INVALID_WORKSPACE_PROMPT
      ? 'relative-worktree'
      : workspaces[spec.branch];
    return {id: 'session-' + spec.branch.split('/').pop(), branch: spec.branch, workspacePath};
  },
  kill: async () => {},
});
export const acquireSpawnLock = () => {
  try {
    closeSync(openSync(process.env.AO_FAKE_LOCK, 'wx'));
    return {acquired: true, release() {rmSync(process.env.AO_FAKE_LOCK, {force: true});}};
  } catch {
    return {acquired: false};
  }
};
export const resolveSpawnQueueConfig = () => ({enabled: true, maxActiveSessions: 10});
export const isTerminalSession = () => false;
"#,
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['dark-factory']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();
        let fake_ao = bin.join("ao");
        std::fs::write(
            &fake_ao,
            r#"#!/usr/bin/env python3
import json
import os
import sys
if sys.argv[1:3] == ["session", "kill"]:
    with open(os.environ["AO_FAKE_CALLS"], "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"kind": "kill", "args": sys.argv[1:]}) + "\n")
    print("scripted bridge-backed kill failure", file=sys.stderr)
    raise SystemExit(8)
os.execvp(os.environ["AO_FAKE_NODE"], [os.environ["AO_FAKE_NODE"], os.environ["AO_FAKE_CLI_ENTRY"], *sys.argv[1:]])
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_ao).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_ao, permissions).unwrap();

        let first = spec("batch alpha", "factory/batch-alpha-r1");
        let second = spec("batch beta", "factory/batch-beta-r1");
        let invalid = spec(
            "bridge invalid workspace",
            "factory/bridge-invalid-workspace-r1",
        );
        let saved = [
            ("PATH", std::env::var("PATH").ok()),
            ("AO_FAKE_NODE", std::env::var("AO_FAKE_NODE").ok()),
            ("AO_FAKE_CLI_ENTRY", std::env::var("AO_FAKE_CLI_ENTRY").ok()),
            ("AO_FAKE_CALLS", std::env::var("AO_FAKE_CALLS").ok()),
            ("AO_FAKE_LOCK", std::env::var("AO_FAKE_LOCK").ok()),
            ("AO_FAKE_WORKSPACES", std::env::var("AO_FAKE_WORKSPACES").ok()),
            (
                "AO_FAKE_INVALID_WORKSPACE_PROMPT",
                std::env::var("AO_FAKE_INVALID_WORKSPACE_PROMPT").ok(),
            ),
            (
                "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN",
                std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok(),
            ),
        ];
        let old_path = saved[0].1.clone().unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        std::env::set_var("AO_FAKE_NODE", bridge_test_node());
        std::env::set_var("AO_FAKE_CLI_ENTRY", cli.join("dist/index.js"));
        std::env::set_var("AO_FAKE_CALLS", &calls);
        std::env::set_var("AO_FAKE_LOCK", &lock);
        std::env::set_var(
            "AO_FAKE_WORKSPACES",
            serde_json::json!({
                first.branch.clone(): first_workspace,
                second.branch.clone(): second_workspace,
            })
            .to_string(),
        );
        std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax->claude-code");

        let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
        let batch_result = sessions.spawn_batch(&[first.clone(), second.clone()]);
        let first_remote = sessions.worktree_remote_url("dark-factory", &first.branch, "origin");
        let second_remote =
            sessions.worktree_remote_url("dark-factory", &second.branch, "origin");
        std::env::set_var("AO_FAKE_INVALID_WORKSPACE_PROMPT", &invalid.prompt);
        let cleanup_failure = sessions.spawn(&invalid);
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let calls = std::fs::read_to_string(&calls).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&root);

        assert!(batch_result.is_ok(), "batch failed: {batch_result:?}; calls={calls}");
        let rows: Vec<serde_json::Value> = calls
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let spawn_rows: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|row| row["kind"] == "spawn")
            .collect();
        assert_eq!(spawn_rows.len(), 3, "each spec must spawn exactly once: {calls}");
        assert!(spawn_rows.iter().all(|row| row["agent"] == "minimax"), "{calls}");
        assert_eq!(
            spawn_rows
                .iter()
                .filter(|row| row["prompt"] == invalid.prompt)
                .count(),
            1,
            "fatal cleanup failure must not advance to a fallback vendor: {calls}"
        );
        assert!(
            matches!(cleanup_failure, Err(DaemonError::SpawnCleanupFailed { .. })),
            "cleanup_failure={cleanup_failure:?}; calls={calls}"
        );
        assert_eq!(
            rows.iter().filter(|row| row["kind"] == "kill").count(),
            1,
            "calls={calls}"
        );
        assert_eq!(
            first_remote.unwrap().as_deref(),
            Some("https://github.com/jleechanorg/dark-factory.git")
        );
        assert_eq!(
            second_remote.unwrap().as_deref(),
            Some("https://github.com/jleechanorg/dark-factory.git")
        );
    }

    #[test]
    fn bridge_creates_worktree_at_expected_revision_for_adopted_pr() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_adopted_pr_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let target = root.join("managed-target");
        let worktree_dir = root.join("worktrees");
        let calls = root.join("calls.log");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&worktree_dir).unwrap();

        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                "base main commit",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let main_sha = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&target)
            .output()
            .unwrap();
        let main_sha = String::from_utf8(main_sha.stdout)
            .unwrap()
            .trim()
            .to_string();

        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                "adopted PR commit",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let pr_sha = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&target)
            .output()
            .unwrap();
        let pr_sha = String::from_utf8(pr_sha.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_ne!(main_sha, pr_sha);

        std::process::Command::new("git")
            .args(["remote", "add", "origin", target.to_str().unwrap()])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["update-ref", "refs/remotes/origin/main", &main_sha])
            .current_dir(&target)
            .status()
            .unwrap();

        std::process::Command::new("git")
            .args(["checkout", "--detach", &pr_sha])
            .current_dir(&target)
            .status()
            .unwrap();

        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();

        let target_str = target.to_string_lossy();
        let worktree_dir_str = worktree_dir.to_string_lossy();

        std::fs::write(
            core.join("dist/index.js"),
            format!(
                r#"import {{appendFileSync, mkdirSync}} from 'node:fs';
import {{execFileSync}} from 'node:child_process';
import {{join}} from 'node:path';

const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, JSON.stringify(value) + '\n');
export const loadConfig = () => ({{
  configPath: '/tmp/fake-ao.yaml',
  plugins: {{
    'workspace-worktree': {{ worktreeDir: '{worktree_dir_str}' }}
  }},
  defaults: {{ workspace: 'worktree' }},
  projects: {{
    'worldarchitect': {{ path: '{target_str}', defaultBranch: 'main' }}
  }}
}});

class MockWorktreePlugin {{
  constructor() {{
    this.name = 'worktree';
  }}
  async create(cfg) {{
    const wtPath = join('{worktree_dir_str}', cfg.projectId, cfg.sessionId);
    mkdirSync(join('{worktree_dir_str}', cfg.projectId), {{ recursive: true }});
    execFileSync('git', ['-C', '{target_str}', 'worktree', 'add', '-b', cfg.branch, wtPath, 'origin/main']);
    return {{ path: wtPath, branch: cfg.branch, sessionId: cfg.sessionId, projectId: cfg.projectId }};
  }}
}}

export const createPluginRegistry = () => {{
  const plugins = new Map();
  const pluginInstance = new MockWorktreePlugin();
  plugins.set('workspace:worktree', {{ manifest: {{ slot: 'workspace', name: 'worktree' }}, instance: pluginInstance }});
  return {{
    register: (mod) => {{}},
    get: (slot, name) => plugins.get(slot + ':' + name)?.instance || null,
    list: (slot) => [{{ slot: 'workspace', name: 'worktree' }}],
    loadFromConfig: async () => {{}},
  }};
}};

export const createSessionManager = ({{config, registry}}) => ({{
  list: async () => [],
  spawn: async (spec) => {{
    const ws = registry.get('workspace', 'worktree');
    const wsInfo = await ws.create({{ projectId: spec.projectId, sessionId: 'wa-3425', branch: spec.branch, project: config.projects[spec.projectId] }});
    log({{ kind: 'spawn', branch: spec.branch, workspacePath: wsInfo.path }});
    return {{ id: 'wa-3425', branch: spec.branch, workspacePath: wsInfo.path }};
  }},
}});

export const acquireSpawnLock = () => ({{ acquired: true, release() {{}} }});
export const resolveSpawnQueueConfig = () => ({{ enabled: true, maxActiveSessions: 10 }});
export const isTerminalSession = () => false;
"#
            ),
        )
        .unwrap();

        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['worldarchitect']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "adopted PR remediation probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "docs/claude-guidance-20260814",
            )
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &pr_sha)
            .env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1")
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "bridge failed; stdout={stdout}; stderr={stderr}"
        );
        assert!(stdout.contains("SESSION=wa-3425"), "{stdout}");

        let created_wt = worktree_dir.join("worldarchitect/wa-3425");
        assert!(created_wt.is_dir(), "worktree directory was not created: {created_wt:?}");

        let created_head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&created_wt)
            .output()
            .unwrap();
        let created_head = String::from_utf8(created_head.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            created_head, pr_sha,
            "created worktree must be at expected revision {pr_sha}, got {created_head} (main_sha is {main_sha})"
        );

        let created_branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&created_wt)
            .output()
            .unwrap();
        let created_branch = String::from_utf8(created_branch.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(created_branch, "docs/claude-guidance-20260814");

        // A configured target checkout can itself be on the adopted PR
        // branch. Retrying there must fail closed: it must never treat the
        // primary checkout as a stale AO worktree and recursively delete it.
        std::process::Command::new("git")
            .args(["worktree", "unlock", created_wt.to_str().unwrap()])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["worktree", "remove", "--force", "--force", created_wt.to_str().unwrap()])
            .current_dir(&target)
            .status()
            .unwrap();
        let checkout_status = std::process::Command::new("git")
            .args(["checkout", "-q", "docs/claude-guidance-20260814"])
            .current_dir(&target)
            .status()
            .unwrap();
        assert!(checkout_status.success(), "target must own the adopted branch for this safety probe");
        let sentinel = target.join("operator-uncommitted-sentinel");
        std::fs::write(&sentinel, "must survive failed worker spawn\n").unwrap();

        let collision_output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "adopted PR primary-worktree collision probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "docs/claude-guidance-20260814",
            )
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &pr_sha)
            .env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1")
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();
        assert!(
            !collision_output.status.success(),
            "primary-checkout branch collision must fail closed"
        );
        assert!(
            sentinel.is_file(),
            "worker retry must never delete the configured primary checkout"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bridge_remediates_and_resets_stale_branch_on_clean_retry() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_ao_bridge_adopted_retry_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cli = root.join("node_modules/@jleechanorg/ao-cli");
        let core = root.join("node_modules/@jleechanorg/ao-core");
        let target = root.join("managed-target");
        let worktree_dir = root.join("worktrees");
        let calls = root.join("calls.log");
        std::fs::create_dir_all(cli.join("dist/lib")).unwrap();
        std::fs::create_dir_all(core.join("dist")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&worktree_dir).unwrap();

        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                "base main commit",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let main_sha = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&target)
            .output()
            .unwrap();
        let main_sha = String::from_utf8(main_sha.stdout)
            .unwrap()
            .trim()
            .to_string();

        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                "adopted PR commit",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        let pr_sha = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&target)
            .output()
            .unwrap();
        let pr_sha = String::from_utf8(pr_sha.stdout)
            .unwrap()
            .trim()
            .to_string();

        std::process::Command::new("git")
            .args(["remote", "add", "origin", target.to_str().unwrap()])
            .current_dir(&target)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["update-ref", "refs/remotes/origin/main", &main_sha])
            .current_dir(&target)
            .status()
            .unwrap();

        // Simulate a prior failed spawn wa-3425 which left a stale worktree and branch at main_sha
        let stale_wt = worktree_dir.join("worldarchitect/wa-3425");
        std::fs::create_dir_all(stale_wt.parent().unwrap()).unwrap();
        std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "docs/claude-guidance-20260814",
                stale_wt.to_str().unwrap(),
                &main_sha,
            ])
            .current_dir(&target)
            .status()
            .unwrap();

        // Target checkout HEAD is at pr_sha
        std::process::Command::new("git")
            .args(["checkout", "--detach", &pr_sha])
            .current_dir(&target)
            .status()
            .unwrap();

        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@jleechanorg/ao-cli","version":"0.1.3","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(cli.join("dist/index.js"), "throw new Error('CLI must not run');\n")
            .unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@jleechanorg/ao-core","version":"0.1.0","type":"module","exports":{".":{"import":"./dist/index.js"}}}"#,
        )
        .unwrap();

        let target_str = target.to_string_lossy();
        let worktree_dir_str = worktree_dir.to_string_lossy();

        std::fs::write(
            core.join("dist/index.js"),
            format!(
                r#"import {{appendFileSync, mkdirSync}} from 'node:fs';
import {{execFileSync}} from 'node:child_process';
import {{join}} from 'node:path';

const log = (value) => appendFileSync(process.env.AO_FAKE_CALLS, JSON.stringify(value) + '\n');
export const loadConfig = () => ({{
  configPath: '/tmp/fake-ao.yaml',
  plugins: {{
    'workspace-worktree': {{ worktreeDir: '{worktree_dir_str}' }}
  }},
  defaults: {{ workspace: 'worktree' }},
  projects: {{
    'worldarchitect': {{ path: '{target_str}', defaultBranch: 'main' }}
  }}
}});

class MockWorktreePlugin {{
  constructor() {{
    this.name = 'worktree';
  }}
  async create(cfg) {{
    const wtPath = join('{worktree_dir_str}', cfg.projectId, cfg.sessionId);
    mkdirSync(join('{worktree_dir_str}', cfg.projectId), {{ recursive: true }});
    execFileSync('git', ['-C', '{target_str}', 'worktree', 'add', '-b', cfg.branch, wtPath, 'origin/main']);
    return {{ path: wtPath, branch: cfg.branch, sessionId: cfg.sessionId, projectId: cfg.projectId }};
  }}
}}

export const createPluginRegistry = () => {{
  const plugins = new Map();
  const pluginInstance = new MockWorktreePlugin();
  plugins.set('workspace:worktree', {{ manifest: {{ slot: 'workspace', name: 'worktree' }}, instance: pluginInstance }});
  return {{
    register: (mod) => {{}},
    get: (slot, name) => plugins.get(slot + ':' + name)?.instance || null,
    list: (slot) => [{{ slot: 'workspace', name: 'worktree' }}],
    loadFromConfig: async () => {{}},
  }};
}};

export const createSessionManager = ({{config, registry}}) => ({{
  list: async () => [],
  spawn: async (spec) => {{
    const ws = registry.get('workspace', 'worktree');
    const wsInfo = await ws.create({{ projectId: spec.projectId, sessionId: 'wa-3426', branch: spec.branch, project: config.projects[spec.projectId] }});
    log({{ kind: 'spawn', branch: spec.branch, workspacePath: wsInfo.path }});
    return {{ id: 'wa-3426', branch: spec.branch, workspacePath: wsInfo.path }};
  }},
}});

export const acquireSpawnLock = () => ({{ acquired: true, release() {{}} }});
export const resolveSpawnQueueConfig = () => ({{ enabled: true, maxActiveSessions: 10 }});
export const isTerminalSession = () => false;
"#
            ),
        )
        .unwrap();

        std::fs::write(
            cli.join("dist/lib/preflight.js"),
            "export const preflight = {checkTmux: async () => {}, checkGhAuth: async () => {}};\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/running-state.js"),
            "export const getRunning = async () => ({projects: ['worldarchitect']});\n",
        )
        .unwrap();
        std::fs::write(
            cli.join("dist/lib/lifecycle-service.js"),
            "export const ensureLifecycleWorker = async () => {};\n",
        )
        .unwrap();

        let bridge = ao_spawn_bridge_path();
        let output = std::process::Command::new(bridge_test_node())
            .arg(cli.join("dist/index.js"))
            .args([
                "spawn",
                "--project",
                "worldarchitect",
                "--agent",
                "minimax",
                "--",
                "adopted PR remediation retry probe",
            ])
            .env(
                "NODE_OPTIONS",
                format!(
                    "--experimental-import-meta-resolve --import={}",
                    bridge.display()
                ),
            )
            .env("DARK_FACTORY_AO_PARENT_NODE_OPTIONS", "")
            .env("DARK_FACTORY_AO_V013_BRIDGE", "1")
            .env(
                "DARK_FACTORY_AO_SPAWN_BRANCH",
                "docs/claude-guidance-20260814",
            )
            .env("DARK_FACTORY_AO_TARGET_CHECKOUT", &target)
            .env("DARK_FACTORY_AO_EXPECTED_REVISION", &pr_sha)
            .env("DARK_FACTORY_AO_MANAGED_CHECKOUT", "1")
            .env("AO_FAKE_CALLS", &calls)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "bridge retry failed; stdout={stdout}; stderr={stderr}"
        );
        assert!(stdout.contains("SESSION=wa-3426"), "{stdout}");

        let created_wt = worktree_dir.join("worldarchitect/wa-3426");
        assert!(created_wt.is_dir(), "worktree directory was not created: {created_wt:?}");

        let created_head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&created_wt)
            .output()
            .unwrap();
        let created_head = String::from_utf8(created_head.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            created_head, pr_sha,
            "remediated worktree must be at expected revision {pr_sha}, got {created_head}"
        );

        let created_branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&created_wt)
            .output()
            .unwrap();
        let created_branch = String::from_utf8(created_branch.stdout)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(created_branch, "docs/claude-guidance-20260814");

        let _ = std::fs::remove_dir_all(root);
    }
}

impl Sessions for CliSessions {
    fn active_count(&self) -> Result<usize, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], 30)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        active_session_count(&data)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        self.spawn_with_fallback(spec)
    }

    fn spawn_batch(&self, specs: &[SpawnSpec]) -> Result<Vec<SessionId>, DaemonError> {
        // AO's v0.1.3 spawn lock is per project. Serializing here is not an
        // optimization choice: concurrent bridge processes collide on that
        // lock, spuriously shift later specs to fallback vendors, and bypass
        // the single path's REQUEST=/Worktree classification. Reuse the
        // complete single-spawn path so every batch item preserves the same
        // admission, fallback, workspace, and error semantics.
        let mut spawned = Vec::with_capacity(specs.len());
        for spec in specs {
            match self.spawn_with_fallback(spec) {
                Ok(session) => spawned.push((session, spec)),
                Err(spawn_error) => {
                    let mut cleanup_errors = Vec::new();
                    for (session, spawned_spec) in spawned.iter().rev() {
                        if let Err(cleanup_error) = self.stop(session) {
                            cleanup_errors.push(SpawnBatchCleanupFailure {
                                session: session.0.clone(),
                                bead_id: spawned_spec.bead_id.clone(),
                                branch: spawned_spec.branch.clone(),
                                error: cleanup_error,
                            });
                        }
                    }
                    if cleanup_errors.is_empty() {
                        return Err(spawn_error);
                    }
                    return Err(DaemonError::SpawnBatchCleanupFailed {
                        spawn_error: Box::new(spawn_error),
                        cleanup_errors,
                    });
                }
            }
        }
        Ok(spawned
            .into_iter()
            .map(|(session, _)| session)
            .collect())
    }

    /// jleechan-hna3: reverse lookup of the AO session CURRENTLY associated
    /// with `branch`, so the re-roll handover flow (`reroll.rs`) can `stop()`
    /// it cleanly before creating a fresh attempt branch. This is NOT the
    /// interactive `ao session attach <name>` terminal-reconnect command
    /// (that would hang/misbehave from a non-interactive daemon subprocess)
    /// and it does NOT steer the old session with new instructions —
    /// remediation happens later via a fresh branch + `sessions.spawn()`.
    ///
    /// Same `ao status --json` parsing shape as `is_quiescent` /
    /// `session_branch` above (ground-truth field names verified live:
    /// `name`, `branch`, `activity`), just searched in the opposite
    /// direction: given a branch, find the entry whose `branch` field
    /// matches and return its `name` as the `SessionId`.
    ///
    /// "No matching entry" is a legitimate, expected failure (e.g. the bead
    /// has no currently-tracked session) — the caller in `reroll.rs` already
    /// handles it by parking `HumanHeld` and surfacing the error string
    /// verbatim in telemetry a human reads, so the message names the branch
    /// and bead explicitly instead of a generic "not found".
    fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError> {
        self.attach_within(branch, bead_id, 30)
    }

    /// Bead jleechan-zeij / issue #322 r4 P2: budget-bounded `attach` — the
    /// re-roll poll passes the time remaining until its window deadline so a
    /// single poll cannot block for multiples of the window on stacked ~30s
    /// `ao status` timeouts.
    fn attach_within(
        &self,
        branch: &str,
        bead_id: &str,
        timeout_secs: u64,
    ) -> Result<SessionId, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], timeout_secs)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        session_for_branch(&data, branch, bead_id)
    }

    fn stop(&self, id: &SessionId) -> Result<(), DaemonError> {
        run_tool("ao", &["session", "kill", &id.0], 30)?;
        Ok(())
    }

    fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], 30)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        session_is_quiescent(&data, id)
    }

    /// Bead jleechan-zeij / issue #322 r2: read AO's per-session `activity`
    /// field directly so the re-roll proceed predicate can tell an idle
    /// worker (`status=spawning, activity=idle` — safe to supersede jointly
    /// with a stable HEAD) apart from a running one (never safe). Same
    /// `ao status --json` parsing shape as `is_quiescent` above; a session
    /// with no matching status row classifies as `NotFound` (fully reaped).
    fn session_activity(
        &self,
        id: &SessionId,
    ) -> Result<crate::tools::SessionActivity, DaemonError> {
        self.session_activity_within(id, 30)
    }

    /// Bead jleechan-zeij / issue #322 r4 P2: budget-bounded
    /// `session_activity` — same rationale as `attach_within`.
    fn session_activity_within(
        &self,
        id: &SessionId,
        timeout_secs: u64,
    ) -> Result<crate::tools::SessionActivity, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], timeout_secs)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        session_activity(&data, id)
    }

    /// jleechan-5ia2: `ao status --json` already reports each session's
    /// `branch` field (verified live: `ao status --json | jq '.[].branch'`).
    /// Reuse the same parsing shape as `is_quiescent` above. Any failure to
    /// reach/parse `ao status` is folded into `Ok(None)` — "cannot verify" —
    /// rather than propagated as an `Err`, matching the trait contract that
    /// callers only ever reject a dispatch on a *positive* mismatch, never
    /// on an inability to check.
    fn session_branch(&self, id: &SessionId) -> Result<Option<String>, DaemonError> {
        let out = match run_tool("ao", &["status", "--json"], 30) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = match serde_json::from_str(&out[json_start..]) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if entry.get("name").and_then(|v| v.as_str()) == Some(&id.0) {
                    return Ok(entry
                        .get("branch")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// jleechan-bqdv Stage C: the spawn-time worktree remote assertion's data
    /// source. Uses the exact absolute `workspacePath` AO returned during
    /// spawn and reads back whatever `remote_name` would actually be pushed
    /// to there via `git remote get-url --push`. Missing mapping, missing
    /// directory, and git inspection failure are errors: after AO has
    /// returned a session, accepting an unverifiable worktree would silently
    /// bypass the wrong-repository safety gate.
    ///
    /// **Adversarial review finding (independent Claude review of this
    /// PR):** the original version used `git remote get-url <name>` (the
    /// FETCH url). What the coder actually pushes with is governed by
    /// `remote.<name>.pushurl` when one is configured — a real possibility
    /// for exactly the dual-remote worktree setups this check exists to
    /// police. `--push` asks git for the URL a `git push` would actually
    /// use, falling back to the fetch URL automatically when no separate
    /// pushurl is configured (so the common single-URL case is unaffected).
    fn worktree_remote_url(
        &self,
        ao_project: &str,
        branch: &str,
        remote_name: &str,
    ) -> Result<Option<String>, DaemonError> {
        let key = (ao_project.to_string(), branch.to_string());
        let path = self
            .spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                DaemonError::Config(format!(
                    "no AO workspacePath recorded for project {ao_project:?} branch {branch:?}; refusing to skip remote verification"
                ))
            })?;
        if !path.is_dir() {
            return Err(DaemonError::Config(format!(
                "AO workspacePath {} for project {ao_project:?} branch {branch:?} is not a directory",
                path.display()
            )));
        }
        let cwd = path.to_string_lossy().into_owned();
        let out = run_tool_in_dir(
            "git",
            &["remote", "get-url", "--push", remote_name],
            &cwd,
            10,
        )?;
        let trimmed = out.trim();
        if trimmed.is_empty() {
            Err(DaemonError::Parse(format!(
                "git returned an empty push URL for remote {remote_name:?} in AO workspace {}",
                path.display()
            )))
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    fn worktree_head_ancestry(
        &self,
        session_id: &SessionId,
        expected_branch: &str,
        ancestor_sha: &str,
    ) -> Result<Option<WorktreeHeadAncestry>, DaemonError> {
        let recorded = self
            .spawned_session_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id.0)
            .cloned();
        let Some((recorded_branch, path)) = recorded else {
            return Ok(None);
        };
        if recorded_branch != expected_branch {
            return Err(DaemonError::Config(format!(
                "AO workspace for session {} belongs to branch {recorded_branch:?}, expected {expected_branch:?}",
                session_id.0
            )));
        }
        if !path.is_dir() {
            return Err(DaemonError::Config(format!(
                "AO workspacePath {} for session {} branch {expected_branch:?} is not a directory",
                path.display(),
                session_id.0
            )));
        }
        let cwd = path.to_string_lossy().into_owned();
        let current_branch = run_tool_in_dir(
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            &cwd,
            10,
        )?
        .trim()
        .to_string();
        if current_branch != expected_branch {
            return Err(DaemonError::Config(format!(
                "AO workspace for session {} is currently on branch {current_branch:?}, expected {expected_branch:?}",
                session_id.0
            )));
        }
        let head_sha = run_tool_in_dir("git", &["rev-parse", "HEAD"], &cwd, 10)?
            .trim()
            .to_string();
        if head_sha.is_empty() {
            return Err(DaemonError::Parse(format!(
                "git returned an empty HEAD SHA in AO workspace {}",
                path.display()
            )));
        }
        let contains_ancestor = match run_tool_in_dir(
            "git",
            &["merge-base", "--is-ancestor", ancestor_sha, &head_sha],
            &cwd,
            10,
        ) {
            Ok(_) => true,
            Err(DaemonError::Tool { tool, rc: 1, .. }) if tool == "git" => false,
            Err(error) => return Err(error),
        };
        let stable_head = run_tool_in_dir("git", &["rev-parse", "HEAD"], &cwd, 10)?
            .trim()
            .to_string();
        let stable_branch = run_tool_in_dir(
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            &cwd,
            10,
        )?
        .trim()
        .to_string();
        if stable_head != head_sha || stable_branch != expected_branch {
            return Ok(None);
        }
        Ok(Some(WorktreeHeadAncestry {
            head_sha,
            contains_ancestor,
        }))
    }

    /// Bead jleechan-coder-silent-false-parks-h92r: unlike
    /// `worktree_remote_url` (a spawn-time-only assertion that fails closed
    /// on a missing mapping), this is an advisory liveness signal consulted
    /// on every tick a bead sits DISPATCHED — a missing AO workspace
    /// mapping, missing `$HOME`, or missing transcript directory are all
    /// "no evidence", not an error, so the coder-silence sweep can fall back
    /// to its other signal instead of hard-failing the whole tick.
    fn worktree_transcript_last_activity_epoch(
        &self,
        ao_project: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        let key = (ao_project.to_string(), branch.to_string());
        let path = self
            .spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned();
        let Some(path) = path else {
            return Ok(None);
        };
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return Ok(None);
        }
        let slug = crate::tools::claude_project_slug(&path);
        let transcript_dir = std::path::Path::new(&home)
            .join(".claude")
            .join("projects")
            .join(slug);
        Ok(latest_jsonl_mtime_epoch(&transcript_dir))
    }
}

/// Most recent modification time (unix epoch seconds) across every
/// `*.jsonl` file directly inside `dir`, or `None` when `dir` doesn't exist,
/// isn't readable, or contains no `.jsonl` files. A single unreadable entry
/// is skipped rather than failing the whole scan — this is a best-effort
/// liveness signal, not a correctness-critical read.
fn latest_jsonl_mtime_epoch(dir: &std::path::Path) -> Option<u64> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut latest: Option<u64> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let epoch = dur.as_secs();
        latest = Some(latest.map_or(epoch, |l: u64| l.max(epoch)));
    }
    latest
}

/// Count the daemon's one global AO worker envelope across every project.
/// Multi-repo dispatch resolves `SpawnSpec.ao_project` per bead, so filtering
/// status to `CliSessions::project` would undercount other configured projects
/// and let the global `max_workers` cap be exceeded. A status row is counted
/// unless AO positively reports one of its canonical terminal statuses or the
/// terminal `exited` activity. Explicit orchestrator rows do not consume the
/// worker budget; missing/unknown role or state fails closed as an active
/// worker.
fn is_terminal_ao_session(entry: &serde_json::Value) -> bool {
    matches!(
        entry.get("status").and_then(|value| value.as_str()),
        Some("killed" | "terminated" | "done" | "cleanup" | "errored" | "merged")
    ) || entry.get("activity").and_then(|value| value.as_str()) == Some("exited")
}

fn session_is_quiescent(
    data: &serde_json::Value,
    id: &SessionId,
) -> Result<bool, DaemonError> {
    let sessions = data
        .as_array()
        .ok_or_else(|| DaemonError::Parse("ao status JSON must be an array".to_string()))?;
    Ok(sessions
        .iter()
        .find(|entry| entry.get("name").and_then(|value| value.as_str()) == Some(&id.0))
        .is_some_and(is_terminal_ao_session))
}

/// Bead jleechan-zeij / issue #322 r2: classify a session's liveness into
/// [`SessionActivity`]. Ordering matters — a terminal status wins over the
/// `activity` field (a `killed` session may still carry a stale
/// `activity=idle`), then `activity=="idle"` distinguishes an alive-but-idle
/// worker (the #322 signature) from an actively-running one. A session with
/// no matching row is `NotFound`. Same `ao status --json` shape as
/// `session_is_quiescent`.
fn session_activity(
    data: &serde_json::Value,
    id: &SessionId,
) -> Result<crate::tools::SessionActivity, DaemonError> {
    use crate::tools::SessionActivity;
    let sessions = data
        .as_array()
        .ok_or_else(|| DaemonError::Parse("ao status JSON must be an array".to_string()))?;
    let Some(entry) = sessions
        .iter()
        .find(|entry| entry.get("name").and_then(|value| value.as_str()) == Some(&id.0))
    else {
        return Ok(SessionActivity::NotFound);
    };
    if is_terminal_ao_session(entry) {
        return Ok(SessionActivity::Terminal);
    }
    if entry.get("activity").and_then(|value| value.as_str()) == Some("idle") {
        return Ok(SessionActivity::Idle);
    }
    Ok(SessionActivity::Running)
}

fn session_for_branch(
    data: &serde_json::Value,
    branch: &str,
    bead_id: &str,
) -> Result<SessionId, DaemonError> {
    let sessions = data
        .as_array()
        .ok_or_else(|| DaemonError::Parse("ao status JSON must be an array".to_string()))?;
    let matching: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|entry| entry.get("branch").and_then(|value| value.as_str()) == Some(branch))
        .collect();
    if matching
        .iter()
        .any(|entry| entry.get("name").and_then(|value| value.as_str()).is_none())
    {
        return Err(DaemonError::SessionAmbiguous {
            branch: branch.to_string(),
            sessions: vec!["<missing-name>".to_string()],
        });
    }
    let active: Vec<String> = matching
        .iter()
        .filter(|entry| !is_terminal_ao_session(entry))
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect();
    match active.as_slice() {
        [name] => return Ok(SessionId(name.clone())),
        [] => {}
        _ => {
            return Err(DaemonError::SessionAmbiguous {
                branch: branch.to_string(),
                sessions: active,
            });
        }
    }

    let terminal: Vec<String> = matching
        .iter()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .collect();
    match terminal.as_slice() {
        [name] => Ok(SessionId(name.clone())),
        [] => Err(DaemonError::SessionNotFound {
            branch: branch.to_string(),
            bead_id: bead_id.to_string(),
        }),
        _ => Err(DaemonError::SessionAmbiguous {
            branch: branch.to_string(),
            sessions: terminal,
        }),
    }
}

fn active_session_count(data: &serde_json::Value) -> Result<usize, DaemonError> {
    let sessions = data
        .as_array()
        .ok_or_else(|| DaemonError::Parse("ao status JSON must be an array".to_string()))?;
    Ok(sessions
        .iter()
        .filter(|entry| {
            let is_orchestrator = entry.get("role").and_then(|value| value.as_str())
                == Some("orchestrator");
            !is_orchestrator && !is_terminal_ao_session(entry)
        })
        .count())
}

#[cfg(test)]
mod active_session_count_tests {
    use super::{active_session_count, session_activity, session_for_branch, session_is_quiescent};
    use crate::tools::{SessionActivity, SessionId};

    #[test]
    fn activity_probe_distinguishes_idle_running_terminal_and_missing() {
        // The #322 live signature is the `idle` row: NOT terminal (so
        // `is_quiescent` returns false and the r1 loop stalled), but distinct
        // from a genuinely running worker. A terminal status wins even when a
        // stale `activity` lingers.
        let status = serde_json::json!([
            {"name": "idle-spawning", "status": "spawning", "activity": "idle"},
            {"name": "running", "status": "working", "activity": "working"},
            {"name": "killed-stale-idle", "status": "killed", "activity": "idle"},
            {"name": "exited", "status": "working", "activity": "exited"},
            {"name": "done", "status": "done", "activity": "ready"}
        ]);

        assert_eq!(
            session_activity(&status, &SessionId("idle-spawning".into())).unwrap(),
            SessionActivity::Idle
        );
        assert_eq!(
            session_activity(&status, &SessionId("running".into())).unwrap(),
            SessionActivity::Running
        );
        assert_eq!(
            session_activity(&status, &SessionId("killed-stale-idle".into())).unwrap(),
            SessionActivity::Terminal
        );
        assert_eq!(
            session_activity(&status, &SessionId("exited".into())).unwrap(),
            SessionActivity::Terminal
        );
        assert_eq!(
            session_activity(&status, &SessionId("done".into())).unwrap(),
            SessionActivity::Terminal
        );
        assert_eq!(
            session_activity(&status, &SessionId("gone".into())).unwrap(),
            SessionActivity::NotFound
        );
        assert!(session_activity(&serde_json::json!({}), &SessionId("x".into())).is_err());
    }

    #[test]
    fn global_cap_counts_active_sessions_across_projects_and_unknown_activity() {
        let status = serde_json::json!([
            {"project": "dark-factory", "role": "worker", "status": "working", "activity": "working"},
            {"project": "worldarchitect", "role": "worker", "status": "review_pending", "activity": "ready"},
            {"project": "third-repo"},
            {"project": "dark-factory", "role": "worker", "status": "working", "activity": "exited"},
            {"project": "worldarchitect", "role": "worker", "status": "merged", "activity": "ready"},
            {"project": "worldarchitect", "role": "orchestrator", "status": "working", "activity": "working"}
        ]);

        assert_eq!(active_session_count(&status).unwrap(), 3);
    }

    #[test]
    fn malformed_non_array_status_fails_closed() {
        assert!(active_session_count(&serde_json::json!({"sessions": []})).is_err());
    }

    #[test]
    fn quiescence_uses_the_same_positive_terminal_contract_as_worker_accounting() {
        let status = serde_json::json!([
            {"name": "ready", "status": "working", "activity": "ready"},
            {"name": "unknown", "status": "mystery", "activity": "missing"},
            {"name": "exited", "status": "working", "activity": "exited"},
            {"name": "done", "status": "done", "activity": "ready"}
        ]);

        assert!(!session_is_quiescent(&status, &SessionId("ready".into())).unwrap());
        assert!(!session_is_quiescent(&status, &SessionId("unknown".into())).unwrap());
        assert!(!session_is_quiescent(&status, &SessionId("not-found".into())).unwrap());
        assert!(session_is_quiescent(&status, &SessionId("exited".into())).unwrap());
        assert!(session_is_quiescent(&status, &SessionId("done".into())).unwrap());
        assert!(session_is_quiescent(&serde_json::json!({}), &SessionId("x".into())).is_err());
    }

    #[test]
    fn branch_lookup_prefers_the_unique_active_row_over_older_terminal_history() {
        let status = serde_json::json!([
            {"name": "old", "branch": "feat/shared", "status": "done", "activity": "exited"},
            {"name": "current", "branch": "feat/shared", "status": "working", "activity": "ready"}
        ]);
        assert_eq!(
            session_for_branch(&status, "feat/shared", "bead").unwrap().0,
            "current"
        );

        let ambiguous = serde_json::json!([
            {"name": "a", "branch": "feat/shared", "status": "working", "activity": "ready"},
            {"name": "b", "branch": "feat/shared", "status": "working", "activity": "working"}
        ]);
        assert!(matches!(
            session_for_branch(&ambiguous, "feat/shared", "bead"),
            Err(crate::errors::DaemonError::SessionAmbiguous { .. })
        ));
    }
}

#[cfg(test)]
mod worktree_remote_url_tests {
    use super::{gh_env_test_lock, CliSessions};
    use crate::tools::{SessionId, Sessions};

    fn record_workspace(
        sessions: &CliSessions,
        project: &str,
        branch: &str,
        path: &std::path::Path,
    ) {
        sessions
            .spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (project.to_string(), branch.to_string()),
                path.to_path_buf(),
            );
    }

    fn record_session_workspace(
        sessions: &CliSessions,
        session_id: &str,
        branch: &str,
        path: &std::path::Path,
    ) {
        sessions
            .spawned_session_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session_id.to_string(),
                (branch.to_string(), path.to_path_buf()),
            );
    }

    fn git(path: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_commit(path: &std::path::Path, contents: &str) -> String {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        git(
            path,
            &[
                "config",
                "user.email",
                "jleechan2015@users.noreply.github.com",
            ],
        );
        git(path, &["config", "user.name", "Factory Test"]);
        std::fs::write(path.join("tracked.txt"), contents).unwrap();
        git(path, &["add", "tracked.txt"]);
        git(path, &["commit", "-qm", "initial"]);
        git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn worktree_head_ancestry_uses_exact_session_workspace() {
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_ancestry_true_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let first_sha = init_commit(&root, "first");
        git(&root, &["branch", "-m", "factory/test-r1"]);
        std::fs::write(root.join("tracked.txt"), "second").unwrap();
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "second"]);
        let current_sha = git(&root, &["rev-parse", "HEAD"]);

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_session_workspace(&sessions, "session-ancestry", "factory/test-r1", &root);
        let relation = sessions
            .worktree_head_ancestry(
                &SessionId("session-ancestry".into()),
                "factory/test-r1",
                &first_sha,
            )
            .unwrap()
            .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(relation.contains_ancestor);
        assert_eq!(relation.head_sha, current_sha);
    }

    #[test]
    fn worktree_head_ancestry_reports_real_local_rewrite() {
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_ancestry_false_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let first_sha = init_commit(&root, "first");
        git(
            &root,
            &["checkout", "-q", "--orphan", "factory/test-r2"],
        );
        git(&root, &["rm", "-q", "-f", "tracked.txt"]);
        std::fs::write(root.join("replacement.txt"), "replacement").unwrap();
        git(&root, &["add", "replacement.txt"]);
        git(&root, &["commit", "-qm", "rewritten"]);

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_session_workspace(&sessions, "session-rewrite", "factory/test-r2", &root);
        let relation = sessions
            .worktree_head_ancestry(
                &SessionId("session-rewrite".into()),
                "factory/test-r2",
                &first_sha,
            )
            .unwrap()
            .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(!relation.contains_ancestor);
    }

    #[test]
    fn worktree_head_ancestry_rejects_replaced_session_branch() {
        let sessions = CliSessions::new("owner/repo", "claude-code");
        let root = std::env::temp_dir();
        record_session_workspace(&sessions, "session-wrong", "factory/other-r1", &root);
        let error = sessions
            .worktree_head_ancestry(
                &SessionId("session-wrong".into()),
                "factory/expected-r1",
                "abc123",
            )
            .unwrap_err();
        assert!(error.to_string().contains("belongs to branch"));
    }

    #[test]
    fn worktree_head_ancestry_rejects_workspace_checked_out_elsewhere() {
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_ancestry_wrong_branch_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let first_sha = init_commit(&root, "first");
        git(&root, &["branch", "-m", "factory/expected-r1"]);
        git(&root, &["checkout", "-qb", "other-branch"]);

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_session_workspace(
            &sessions,
            "session-checked-out-elsewhere",
            "factory/expected-r1",
            &root,
        );
        let error = sessions
            .worktree_head_ancestry(
                &SessionId("session-checked-out-elsewhere".into()),
                "factory/expected-r1",
                &first_sha,
            )
            .unwrap_err();

        let _ = std::fs::remove_dir_all(&root);
        assert!(error.to_string().contains("currently on branch"));
    }

    #[test]
    fn worktree_head_ancestry_returns_none_after_restart_loses_mapping() {
        let sessions = CliSessions::new("owner/repo", "claude-code");
        assert_eq!(
            sessions
                .worktree_head_ancestry(
                    &SessionId("session-before-restart".into()),
                    "factory/test-r1",
                    "abc123",
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn worktree_remote_url_fails_closed_without_spawned_workspace() {
        let sessions = CliSessions::new("owner/repo", "claude-code");
        let result = sessions.worktree_remote_url("dark-factory", "factory/missing-r1", "origin");

        let error = result.expect_err("missing AO workspace mapping must fail closed");
        assert!(error.to_string().contains("no AO workspacePath recorded"));
    }

    /// Real `git`, no shim: record an opaque AO workspace, add a remote, and
    /// confirm validation reads that exact path rather than reconstructing
    /// one from the factory branch.
    #[test]
    fn worktree_remote_url_reads_real_git_remote() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_remote_url_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let worktree_path = root.join("df-opaque-134");
        std::fs::create_dir_all(&worktree_path).unwrap();
        let cwd = worktree_path.to_string_lossy().into_owned();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&cwd)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "worldai",
                "https://github.com/jleechanorg/worldarchitect.ai.git",
            ])
            .current_dir(&cwd)
            .status()
            .unwrap();

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_workspace(
            &sessions,
            "dark-factory",
            "factory/jleechan-bqdv-r1",
            &worktree_path,
        );
        let result =
            sessions.worktree_remote_url("dark-factory", "factory/jleechan-bqdv-r1", "worldai");

        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            result.unwrap().as_deref(),
            Some("https://github.com/jleechanorg/worldarchitect.ai.git")
        );
    }

    /// Adversarial review finding (independent Claude review of this PR):
    /// when a remote has a SEPARATE `pushurl` configured (distinct from its
    /// fetch URL — a real dual-remote-worktree scenario), `worktree_remote_url`
    /// must report the URL a `git push` would actually use, not the fetch
    /// URL. Regression guard for the `git remote get-url` -> `git remote
    /// get-url --push` fix.
    #[test]
    fn worktree_remote_url_reports_pushurl_when_distinct_from_fetch_url() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_remote_url_pushurl_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let worktree_path = root.join("df-opaque-pushurl");
        std::fs::create_dir_all(&worktree_path).unwrap();
        let cwd = worktree_path.to_string_lossy().into_owned();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&cwd)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "worldai",
                "https://github.com/jleechanorg/jleechanclaw.git",
            ])
            .current_dir(&cwd)
            .status()
            .unwrap();
        // Configure a DIFFERENT push URL — the exact scenario that defeats a
        // plain `git remote get-url` (fetch URL) check.
        std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "--push",
                "worldai",
                "https://github.com/jleechanorg/worldarchitect.ai.git",
            ])
            .current_dir(&cwd)
            .status()
            .unwrap();

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_workspace(
            &sessions,
            "dark-factory",
            "factory/jleechan-bqdv-r1",
            &worktree_path,
        );
        let result =
            sessions.worktree_remote_url("dark-factory", "factory/jleechan-bqdv-r1", "worldai");

        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            result.unwrap().as_deref(),
            Some("https://github.com/jleechanorg/worldarchitect.ai.git"),
            "must report the PUSH url, not the (different) fetch url"
        );
    }

    #[test]
    fn worktree_remote_url_fails_closed_for_unconfigured_remote_name() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_worktree_remote_url_unconfigured_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let worktree_path = root.join("df-opaque-no-remote");
        std::fs::create_dir_all(&worktree_path).unwrap();
        let cwd = worktree_path.to_string_lossy().into_owned();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&cwd)
            .status()
            .unwrap();

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_workspace(
            &sessions,
            "dark-factory",
            "factory/jleechan-bqdv-r1",
            &worktree_path,
        );
        let result =
            sessions.worktree_remote_url("dark-factory", "factory/jleechan-bqdv-r1", "origin");

        let _ = std::fs::remove_dir_all(&root);

        assert!(result.is_err(), "an unconfigured push remote must fail closed");
    }
}

#[cfg(test)]
mod worktree_transcript_last_activity_epoch_tests {
    use super::{gh_env_test_lock, CliSessions};
    use crate::tools::Sessions;

    fn record_workspace(
        sessions: &CliSessions,
        project: &str,
        branch: &str,
        path: &std::path::Path,
    ) {
        sessions
            .spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (project.to_string(), branch.to_string()),
                path.to_path_buf(),
            );
    }

    /// Bead jleechan-coder-silent-false-parks-h92r: no `spawned_worktrees`
    /// entry for the (project, branch) key must be "no evidence"
    /// (`Ok(None)`), never an error and never treated as proof of silence —
    /// this signal is advisory, unlike the spawn-time-only
    /// `worktree_remote_url` fail-closed check.
    #[test]
    fn no_evidence_without_spawned_workspace() {
        let sessions = CliSessions::new("owner/repo", "claude-code");
        let result = sessions
            .worktree_transcript_last_activity_epoch("dark-factory", "factory/missing-r1");

        assert_eq!(result.unwrap(), None);
    }

    /// Reproduces the 2026-07-17 false-park scenario: a real worktree with a
    /// transcript directory whose `.jsonl` file was modified moments ago
    /// must report a recent epoch, even though nothing was pushed to the
    /// remote branch.
    #[test]
    fn reports_latest_jsonl_mtime_from_derived_transcript_dir() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_transcript_activity_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let fake_home = root.join("home");
        let worktree_path = root.join("worktrees").join("dark-factory").join("df-100");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let slug = crate::tools::claude_project_slug(&worktree_path);
        let transcript_dir = fake_home.join(".claude").join("projects").join(&slug);
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(transcript_dir.join("session-1.jsonl"), "{}\n").unwrap();
        std::fs::write(transcript_dir.join("not-a-transcript.txt"), "ignore me").unwrap();

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &fake_home);

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_workspace(&sessions, "dark-factory", "factory/df-100-r1", &worktree_path);
        let result =
            sessions.worktree_transcript_last_activity_epoch("dark-factory", "factory/df-100-r1");

        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(&root);

        let epoch = result.unwrap().expect("must find the .jsonl file's mtime");
        assert!(
            epoch >= before,
            "reported epoch {epoch} must be >= test-start epoch {before}"
        );
    }

    /// A resolvable worktree whose transcript directory doesn't exist yet
    /// (e.g. the coder session hasn't written any transcript file yet, or
    /// the naming convention drifted) is "no evidence", not an error.
    #[test]
    fn no_evidence_when_transcript_dir_missing() {
        let _guard = gh_env_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "afd_transcript_activity_missing_dir_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let fake_home = root.join("home");
        let worktree_path = root.join("worktrees").join("dark-factory").join("df-101");
        std::fs::create_dir_all(&worktree_path).unwrap();
        std::fs::create_dir_all(&fake_home).unwrap();

        let prev_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &fake_home);

        let sessions = CliSessions::new("owner/repo", "claude-code");
        record_workspace(&sessions, "dark-factory", "factory/df-101-r1", &worktree_path);
        let result =
            sessions.worktree_transcript_last_activity_epoch("dark-factory", "factory/df-101-r1");

        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(result.unwrap(), None);
    }
}

/// `target_repo` is the "<owner>/<repo>" the daemon automates (`cfg.target_repo`,
/// e.g. `jleechanorg/worldarchitect.ai`) -- NOT the daemon's own source repo.
/// jleechan-9sl1: `remote_head_sha`/`is_ancestor` used to shell out to the
/// local `git` binary, which only ever operates against the daemon
/// PROCESS's own cwd (its systemd `WorkingDirectory`, the daemon's own repo
/// checkout) -- structurally incapable of succeeding for any real
/// `target_repo`. Both methods now go through `gh api` scoped to this field
/// instead; see their doc comments below.
pub struct CliVcs {
    pub target_repo: String,
}

impl CliVcs {
    pub fn new(target_repo: String) -> Self {
        Self { target_repo }
    }

    /// Clone this adapter targeting a different repo string (bead
    /// jleechan-35y4 Stage B — see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`'s "keep
    /// traits, add `with_repo` constructor" note). `CliVcs` carries no
    /// state beyond `target_repo`, so this is a cheap clone-with-override,
    /// not a trait signature change — call sites keep using the same `Vcs`
    /// trait, just against a handle bound to `repo` instead of the
    /// process-global `cfg.target_repo`. Migrating the ~27 call sites that
    /// currently read `cfg.target_repo` directly to call this (via
    /// `overlay.repo(cfg)`) is Stage D, bead jleechan-9xrs — this
    /// constructor is the capability, not the migration.
    pub fn with_repo(&self, repo: &str) -> Self {
        Self {
            target_repo: repo.to_string(),
        }
    }
}

impl Vcs for CliVcs {
    fn base_head(&self, base_branch: &str) -> Result<String, DaemonError> {
        let out = run_tool("git", &["rev-parse", base_branch], 30)?;
        Ok(out.trim().to_string())
    }

    /// jleechan-wuts / issue #349: routed-repo variant of [`base_head`].
    /// `gh api repos/<repo>/git/ref/heads/<branch>` returns the SHA at
    /// the HEAD of `<branch>` in `<repo>`. Equivalent in shape to
    /// `remote_head_sha` (`repos/<repo>/commits/<branch>` returns the
    /// same SHA, but `git/ref/heads/<branch>` is the Git Data API's
    /// canonical "branch HEAD" lookup and is what `gh` itself uses
    /// internally for branch refs). Decoupled from the daemon's own
    /// cwd (its systemd `WorkingDirectory`, the daemon's own source-repo
    /// checkout), so a bead whose `overlay.repo(cfg)` names a DIFFERENT
    /// repo from `cfg.target_repo` resolves the baseline against the
    /// routed repo instead of the daemon's own repo's same-named branch.
    ///
    /// Branches containing `/` (e.g. `release/2026-q3`) survive intact:
    /// the `git/ref/heads/` prefix accepts embedded slashes the same
    /// way `commits/<branch>` already does (cf. the doc comment on
    /// `remote_head_sha`).
    fn base_head_for_repo(&self, repo: &str, base_branch: &str) -> Result<String, DaemonError> {
        let path = format!("repos/{}/git/ref/heads/{}", repo, base_branch);
        let out = run_tool("gh", &["api", &path, "--jq", ".object.sha"], 30)?;
        let sha = out.trim();
        // `gh api ... --jq .object.sha` exits 0 with the literal string
        // `null` when the branch doesn't exist in the target repo --
        // treat that as an error rather than a valid SHA, mirroring the
        // `remote_head_sha` policy above.
        if sha.is_empty() || sha == "null" {
            return Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 0,
                stderr: format!(
                    "gh api {path} returned no sha for branch '{base_branch}' in {repo}"
                ),
            });
        }
        Ok(sha.to_string())
    }

    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError> {
        run_tool("git", &["branch", name, sha], 30)?;
        Ok(())
    }

    /// jleechan-wuts / issue #349: routed-repo variant of
    /// [`create_branch_at`]. POSTs a `refs/heads/<name>` ref via the
    /// Git Data API at `repos/<repo>/git/refs`. Decoupled from the
    /// daemon's own cwd (its systemd `WorkingDirectory`, the daemon's
    /// own source-repo checkout), so the new attempt's
    /// `factory/<bead>-r<n>` branch lands in the routed target repo
    /// where the worker will actually push -- not in the daemon's own
    /// source-repo checkout, where the old CWD-bound `git branch
    /// <name> <sha>` would have silently created it (and where the
    /// worker's first `git push` would then race or be rejected).
    ///
    /// The endpoint rejects a ref that already exists in `<repo>`
    /// with HTTP 422 -- the reroll path always passes a freshly
    /// formatted `factory/<bead>-r<attempt>` branch (incremented per
    /// attempt), so a collision in the routed repo is structurally
    /// impossible absent a stale `register_branch`/gh-state mismatch;
    /// the underlying `DaemonError::Tool` with the HTTP body as
    /// stderr surfaces that case to the operator for the same reason
    /// the old CWD-bound `git branch <name> <sha>` did.
    fn create_branch_at_for_repo(&self, repo: &str, name: &str, sha: &str) -> Result<(), DaemonError> {
        let path = format!("repos/{}/git/refs", repo);
        let ref_path = format!("refs/heads/{name}");
        let out = run_tool(
            "gh",
            &[
                "api",
                "--method",
                "POST",
                &path,
                "-f",
                &format!("ref={ref_path}"),
                "-f",
                &format!("sha={sha}"),
            ],
            30,
        )?;
        let _ = out; // POST success returns the created ref object; we don't need its body.
        Ok(())
    }

    /// Bead jleechan-znmh / issue #341: delete a ref via the routed-repo
    /// Data API. Companion to [`create_branch_at_for_repo`](Self::create_branch_at_for_repo):
    /// when a prior failed reroll left a stale `factory/<bead>-r<n>` ref
    /// behind (HTTP 422 on the next POST), the reroll calls this to clear
    /// it before retrying the create. Cross-repo, cwd-independent —
    /// identical plumbing shape to the create, mirroring how
    /// `create_branch_at_for_repo` was added for issue #349.
    fn delete_branch_at_for_repo(
        &self,
        repo: &str,
        name: &str,
    ) -> Result<(), DaemonError> {
        let path = format!("repos/{}/git/refs/heads/{}", repo, name);
        let out = run_tool(
            "gh",
            &["api", "--method", "DELETE", &path],
            30,
        )?;
        let _ = out; // DELETE success returns 204; we don't need the body.
        Ok(())
    }

    fn head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        self.head_sha_within(branch, 30)
    }

    /// Bead jleechan-zeij / issue #322 r4 P2: budget-bounded `head_sha` — the
    /// re-roll poll caps this `git rev-parse` at the remaining window budget.
    fn head_sha_within(&self, branch: &str, timeout_secs: u64) -> Result<String, DaemonError> {
        let out = run_tool("git", &["rev-parse", branch], timeout_secs)?;
        Ok(out.trim().to_string())
    }

    /// Bead dark-factory-mw85: repo-scoped, budget-bounded `head_sha` — queries
    /// `gh api repos/<repo>/git/ref/heads/<branch>` to fetch branch HEAD SHA,
    /// decoupling reroll quiescence from the daemon's local process CWD.
    fn head_sha_within_for_repo(
        &self,
        repo: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<String, DaemonError> {
        let path = format!("repos/{}/git/ref/heads/{}", repo, branch);
        let out = run_tool("gh", &["api", &path, "--jq", ".object.sha"], timeout_secs)?;
        let sha = out.trim();
        if sha.is_empty() || sha == "null" {
            return Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 0,
                stderr: format!(
                    "gh api {path} returned no sha for branch '{branch}' in {repo}"
                ),
            });
        }
        Ok(sha.to_string())
    }

    fn is_remote_ahead(&self, branch: &str, remote_sha: &str) -> Result<bool, DaemonError> {
        // Two-step check:
        // 1. local_head == remote_sha ⇒ not ahead (worker hasn't actually
        //    pushed anything new since the daemon's last view).
        // 2. `git merge-base --is-ancestor <local> <remote>` returns 0 iff
        //    local is reachable from remote (i.e. every local commit is in
        //    remote) — combined with the inequality check this is the
        //    strict "remote has all of local + more" predicate. A
        //    divergent branch or a local-only-ahead branch returns rc=1.
        let local = self.head_sha(branch)?;
        if local.is_empty() || remote_sha.is_empty() || local == remote_sha {
            return Ok(false);
        }
        // `--is-ancestor` exits 0 on true, 1 on false; we don't care about
        // commit messages so `--quiet` keeps stderr clean.
        let r = run_tool(
            "git",
            &["merge-base", "--is-ancestor", &local, remote_sha],
            30,
        );
        Ok(r.is_ok())
    }

    /// jleechan-9sl1: query GitHub directly for `self.target_repo`'s branch
    /// tip via `gh api`, instead of `git fetch origin <branch>` against the
    /// DAEMON's own cwd (its systemd `WorkingDirectory`, the daemon's own
    /// source repo checkout, not `target_repo`) -- the old implementation
    /// was structurally incapable of succeeding for any real target repo.
    /// `GET /repos/{owner}/{repo}/commits/{ref}` (the "get a commit" REST
    /// endpoint) special-cases its `ref` path segment to allow embedded
    /// slashes, so a branch name like `fix/7887-cc-finish-level-commit`
    /// (a real branch from the jleechan-93ft incident) works directly with
    /// no URL-encoding and no `heads/` prefix needed -- do NOT percent-encode
    /// the branch or try to split on `/` to separate "owner/repo" from
    /// "branch"; the branch itself may contain `/`.
    fn remote_head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        let path = format!("repos/{}/commits/{}", self.target_repo, branch);
        let out = run_tool("gh", &["api", &path, "--jq", ".sha"], 30)?;
        let sha = out.trim();
        // `gh api ... --jq .sha` exits 0 with the literal string `null` when
        // the JSON path doesn't resolve (e.g. a malformed/unexpected
        // response shape) -- that is not a valid SHA and must not be
        // returned as one; the daemon relies on `remote_head_sha`'s return
        // value as a real git object id for the subsequent `is_ancestor`
        // check.
        if sha.is_empty() || sha == "null" {
            return Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 0,
                stderr: format!(
                    "gh api {path} returned no sha for branch '{branch}' in {}",
                    self.target_repo
                ),
            });
        }
        Ok(sha.to_string())
    }

    /// jleechan-9sl1: replace local `git merge-base --is-ancestor` (which
    /// needs both SHAs' git objects present locally -- they never are, since
    /// they live in `target_repo`'s history, not the daemon's own repo) with
    /// GitHub's "compare two commits" REST endpoint,
    /// `GET /repos/{owner}/{repo}/compare/{base}...{head}`, where `status`
    /// is reported relative to `base...head` (here `base` = `ancestor_sha`,
    /// `head` = `descendant_sha`):
    /// - `identical` — same commit → trivially an ancestor → `true`.
    /// - `ahead` — head is ahead of base: head contains every commit base
    ///   has, plus more → base (ancestor) is reachable from head
    ///   (descendant) → `true`.
    /// - `behind` — head is BEHIND base: base has commits head doesn't →
    ///   ancestor is NOT reachable from descendant (it's the other way
    ///   around) → `false`.
    /// - `diverged` — neither contains the other → `false`.
    ///
    /// The identical-SHA case (`ancestor_sha == descendant_sha`) is handled
    /// by an explicit short-circuit BEFORE any `gh` call, sidestepping the
    /// need to depend on undocumented behavior for what the compare API
    /// does when asked to compare a SHA against itself.
    ///
    /// Any non-recognized `status`, or a `gh` invocation failure, propagates
    /// as a real `Err` — unlike the old implementation, this does NOT fold
    /// every non-`Ok(true)` outcome into `Ok(false)`. Per the trait's doc
    /// comment, callers already treat `Err` and `Ok(false)` identically
    /// (fail-closed / escalate to human) for this method's force-push-
    /// detection use case, but the code itself keeps "confirmed false" and
    /// "cannot determine" as distinct outcomes rather than silently
    /// swallowing the latter into the former.
    fn is_ancestor(&self, ancestor_sha: &str, descendant_sha: &str) -> Result<bool, DaemonError> {
        if ancestor_sha == descendant_sha {
            return Ok(true);
        }
        let path = format!(
            "repos/{}/compare/{}...{}",
            self.target_repo, ancestor_sha, descendant_sha
        );
        let out = run_tool("gh", &["api", &path, "--jq", ".status"], 30)?;
        let status = out.trim();
        match status {
            "identical" | "ahead" => Ok(true),
            "behind" | "diverged" => Ok(false),
            other => Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 0,
                stderr: format!(
                    "gh api {path} returned unrecognized compare status '{other}'"
                ),
            }),
        }
    }

    fn push_fix_commit(&self, branch: &str, message: &str) -> Result<(), DaemonError> {
        // Fetch the branch fresh, then check out a local ref pinned exactly
        // to `origin/<branch>` — this is the append-only starting point: the
        // daemon never bases the fix commit on any local/stale ref, only on
        // what the contributor's remote actually has right now.
        run_tool("git", &["fetch", "origin", branch], 30)?;
        run_tool(
            "git",
            &["checkout", "-B", branch, &format!("origin/{branch}")],
            30,
        )?;
        // `--allow-empty`: the daemon's job here is the push mechanism, not
        // generating the code diff itself (that's out of scope for the
        // adopted-branch path per bead jleechan-tfs1 — there is no factory
        // AO session to attach to for an externally authored branch); the
        // commit is a remediation marker carrying the review feedback as its
        // message. A real code-bearing commit would replace this call's
        // caller-supplied `message` with an actual diff, but the append-only
        // contract (one new commit on top, never rewrite) is identical either way.
        run_tool("git", &["commit", "--allow-empty", "-m", message], 30)?;
        // Deliberately NOT --force / --force-with-lease: a non-fast-forward
        // rejection here means the remote diverged from what the daemon just
        // fetched (e.g. the contributor pushed more commits concurrently, or
        // there's a genuine conflict with base). That is exactly the signal
        // the caller (`reroll::execute`) must treat as "needs a human", never
        // silently retried with a history-rewrite.
        run_tool("git", &["push", "origin", branch], 30)?;
        Ok(())
    }
}

pub struct ChainLlm;

/// Single source of truth for the cwd every fallback invocation runs from.
/// Each LLM backend reads AGENTS.md / `.claude/` / settings files from the
/// process's cwd — running fallback from `/tmp` (a bug introduced during
/// bead `jleechan-g1k` work and reverted) makes those backends behave as if
/// launched outside the project, which is the failure mode the smoke test
/// below pins down. `.` is the daemon's invocation cwd; the daemon is
/// launched from the target repo checkout, which is what every backend
/// expects.
const FALLBACK_CWD: &str = ".";

/// Anthropic weekly-limit bypass: re-invoke the `claude` binary against the
/// MiniMax-hosted Anthropic-compatible endpoint instead of api.anthropic.com.
/// Runs from `FALLBACK_CWD` for the same reason every other fallback step
/// does (bead `jleechan-g1k`) — MiniMax is still driving the `claude` CLI, so
/// it still reads AGENTS.md / `.claude/` from the invocation cwd.
fn run_minimax_judge(claude_bin: &str, prompt: &str) -> Result<String, DaemonError> {
    let minimax_key = std::env::var("MINIMAX_API_KEY").map_err(|e| {
        DaemonError::Tool {
            tool: "minimax".into(),
            rc: -1,
            stderr: format!("MINIMAX_API_KEY not set: {e}"),
        }
    })?;

    let mut cmd = std::process::Command::new(claude_bin);
    cmd.args(["--print", "--dangerously-skip-permissions", "--setting-sources", "", prompt])
        .current_dir(FALLBACK_CWD)
        .stdin(std::process::Stdio::null())
        .env("ANTHROPIC_BASE_URL", "https://api.minimax.io/anthropic")
        .env("ANTHROPIC_API_KEY", minimax_key);

    let output = cmd.output().map_err(|e| DaemonError::Tool {
        tool: "minimax".into(),
        rc: -1,
        stderr: format!("failed to run minimax: {e}"),
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(DaemonError::Tool {
            tool: "minimax".into(),
            rc: output.status.code().unwrap_or(-1),
            stderr,
        })
    }
}

impl Llm for ChainLlm {
    fn is_real(&self) -> bool {
        true
    }

    fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
        // Each fallback runs from the project's cwd (FALLBACK_CWD), NOT
        // `/tmp` (which would strip AGENTS.md / .claude/ context — the very
        // mode bead `jleechan-g1k` flagged) and NOT bare `run_tool` (which
        // would silently inherit whatever cwd the daemon happened to be
        // launched from). Prompt and skill flags are distinct argv entries;
        // `--dangerously-skip-permissions` is a flag, never the message
        // text — see the named-argv assertions in `chain_llm_fallback_argv`
        // below.
        let r = run_tool_in_dir(
            "codex",
            &["exec", "--yolo", "--skip-git-repo-check", prompt],
            FALLBACK_CWD,
            120,
        );
        if let Ok(out) = r {
            return Ok(out);
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let nvm_claude = format!("{}/.nvm/versions/node/v22.22.0/bin/claude", home);
        let claude_bin = if std::path::Path::new(&nvm_claude).exists() {
            nvm_claude
        } else {
            "claude".to_string()
        };
        let r = run_tool_in_dir(
            &claude_bin,
            &[
                "--dangerously-skip-permissions",
                "--print",
                "--setting-sources",
                "",
                prompt,
            ],
            FALLBACK_CWD,
            120,
        );
        if let Ok(out) = r {
            return Ok(out);
        }
        let r = run_minimax_judge(&claude_bin, prompt);
        if let Ok(out) = r {
            return Ok(out);
        }
        let r = run_tool_in_dir(
            "agy",
            &["--dangerously-skip-permissions", "--print", prompt],
            FALLBACK_CWD,
            120,
        );
        if let Ok(out) = r {
            return Ok(out);
        }
        Err(DaemonError::Tool {
            tool: "ChainLlm".into(),
            rc: 1,
            stderr: "All LLM backends in fallback chain failed".into(),
        })
    }
}

/// Check-run names whose verdict is owned by a dedicated gate rather than
/// gate 1 (CI). "Evidence Gate" belongs to gate 6 (evidence floor / /er):
/// a fresh factory PR opens with no evidence marker by construction, so
/// letting its failing check-run turn gate 1 red rerolled every attempt
/// before evidence could be attached (live incident 2026-07-28, PR #485).
/// CodeRabbit / Bugbot check-runs stay in gate 1's input: they report
/// pass/neutral buckets and their dedicated gates read reviews/comments,
/// not check-runs.
pub fn check_owned_by_dedicated_gate(check_name: &str) -> bool {
    check_name.trim() == "Evidence Gate"
}

/// CI gate input for `PrSnapshot.ci_success`. Production requires every bucket
/// to be `pass` or `skipping`; `pending` is not green. Iteration stub may treat
/// `pending` as acceptable but never `fail`/`cancel`.
pub fn ci_success_from_check_buckets(buckets: &[&str], iteration_stub: bool) -> bool {
    if buckets.is_empty() {
        return false;
    }
    if iteration_stub {
        buckets
            .iter()
            .all(|b| matches!(*b, "pass" | "skipping" | "pending"))
    } else {
        buckets.iter().all(|b| matches!(*b, "pass" | "skipping"))
    }
}

/// jleechan-35y4 Stage B: `with_repo` constructors on `CliScm`/`CliVcs` — the
/// repo-parameterized adapter capability the multi-repo dispatch fix
/// depends on. `CliScm`/`CliVcs` have no test-visible way to invoke gh/git,
/// so these tests only assert the cheap clone-with-override contract:
/// the returned handle targets the new repo string, and the original
/// instance is untouched (it's `&self`, not `self` — no move).
#[cfg(test)]
mod with_repo_tests {
    use super::{CliScm, CliVcs};

    #[test]
    fn cli_vcs_with_repo_targets_new_repo_and_leaves_original_untouched() {
        let original = CliVcs::new("jleechanorg/worldarchitect.ai".to_string());
        let retargeted = original.with_repo("jleechanorg/dark-factory");
        assert_eq!(retargeted.target_repo, "jleechanorg/dark-factory");
        assert_eq!(original.target_repo, "jleechanorg/worldarchitect.ai");
    }

    #[test]
    fn cli_scm_with_repo_targets_new_repo_and_leaves_original_untouched() {
        let original = CliScm::new("jleechanorg/worldarchitect.ai".to_string());
        let retargeted = original.with_repo("jleechanorg/dark-factory");
        assert_eq!(retargeted.repo, "jleechanorg/dark-factory");
        assert_eq!(original.repo, "jleechanorg/worldarchitect.ai");
    }
}

#[cfg(test)]
mod external_ref_tests {
    use super::{
        canonicalize_external_ref_for_comment, parse_external_ref,
        parse_external_refs_from_br_list, unresolved_thread_count_from_gql,
    };

    /// jleechan-mdgr: reproduces the exact 2026-07-11T00:05:15Z corruption —
    /// `ESCALATION_NOTIFICATION_FAILED` fired for bead jleechan-8dyu with
    /// error "parse: invalid external_ref format for comment:
    /// jleechanorg/worldarchitect.ai#7888#local-8dyu". The stored
    /// `external_ref` already had a valid `<repo>#<pr>` shape AND had a
    /// `#local-<bead-id>` disambiguation suffix (see
    /// `daemon/scripts/backfill_external_ref.py`) appended on top of it,
    /// producing 3 `#`-delimited segments instead of 2. `comment_external`
    /// must still be able to recover the real `repo#PR` comment target from
    /// this already-corrupted shape (defense in depth — no bulk data
    /// cleanup pass) even though the writer itself is fixed to never
    /// produce this shape again.
    #[test]
    fn double_suffix_external_ref_is_recovered_for_comment_target() {
        let corrupted = "jleechanorg/worldarchitect.ai#7888#local-8dyu";
        assert_eq!(
            canonicalize_external_ref_for_comment(corrupted),
            Some(("jleechanorg/worldarchitect.ai".to_string(), "7888".to_string())),
            "expected the real repo#PR target to be recovered from the double-suffix corruption"
        );
    }

    #[test]
    fn clean_external_ref_still_parses_unchanged() {
        assert_eq!(
            canonicalize_external_ref_for_comment("owner/repo#42"),
            Some(("owner/repo".to_string(), "42".to_string()))
        );
    }

    #[test]
    fn bare_local_ref_is_not_recovered_by_this_helper() {
        // jleechan-twa0's territory — a bare `local-<id>` ref (no real PR at
        // all) is a distinct bug (parser format acceptance), not the
        // double-append this fixes. Must stay None here so this helper does
        // not silently grow into twa0's scope.
        assert_eq!(canonicalize_external_ref_for_comment("local-w1y"), None);
    }

    #[test]
    fn triple_hash_ref_without_local_marker_is_not_recovered() {
        // Only the specific `<repo>#<pr>#local-<id>` shape is recovered;
        // anything else with more than one `#` and no `local-` marker on
        // the trailing segment is left as a genuine parse failure.
        assert_eq!(
            canonicalize_external_ref_for_comment("owner/repo#42#weird-suffix"),
            None
        );
    }

    #[test]
    fn parse_external_ref_preserves_short_form() {
        assert_eq!(
            parse_external_ref("owner/repo#42"),
            Some(("owner/repo".to_string(), "42".to_string()))
        );
    }

    #[test]
    fn parse_external_ref_accepts_github_pull_url() {
        assert_eq!(
            parse_external_ref(
                "https://github.com/jleechanorg/worldarchitect.ai/pull/8064"
            ),
            Some((
                "jleechanorg/worldarchitect.ai".to_string(),
                "8064".to_string()
            ))
        );
    }

    #[test]
    fn parse_external_ref_accepts_github_issue_url() {
        assert_eq!(
            parse_external_ref("https://github.com/jleechanorg/dark-factory/issues/238"),
            Some((
                "jleechanorg/dark-factory".to_string(),
                "238".to_string()
            ))
        );
    }

    #[test]
    fn fetch_all_external_refs_includes_closed_beads() {
        let json = r#"{
            "issues": [
                {"id": "open-1", "external_ref": "owner/repo#1", "status": "open"},
                {"id": "closed-1", "external_ref": "jleechanorg/worldarchitect.ai#8171", "status": "closed"},
                {"id": "no-ref", "external_ref": null, "status": "closed"}
            ]
        }"#;
        let refs = parse_external_refs_from_br_list(json).unwrap();
        assert_eq!(refs.len(), 2);
        assert!(refs.contains("owner/repo#1"));
        assert!(refs.contains("jleechanorg/worldarchitect.ai#8171"));
    }

    #[test]
    fn truncated_br_list_output_is_rejected() {
        // jleechan-v09l: br list paginates at 50 rows by default; a truncated
        // page must fail closed instead of producing a partial dedup set.
        let json = r#"{
            "issues": [
                {"id": "open-1", "external_ref": "owner/repo#1", "status": "open"}
            ],
            "total": 130,
            "limit": 50,
            "offset": 0,
            "has_more": true
        }"#;
        let err = parse_external_refs_from_br_list(json).unwrap_err();
        assert!(
            err.to_string().contains("truncated"),
            "expected truncation error, got: {err}"
        );
    }

    #[test]
    fn complete_br_list_output_with_pagination_fields_is_accepted() {
        let json = r#"{
            "issues": [
                {"id": "open-1", "external_ref": "owner/repo#1", "status": "open"}
            ],
            "total": 1,
            "limit": 0,
            "offset": 0,
            "has_more": false
        }"#;
        let refs = parse_external_refs_from_br_list(json).unwrap();
        assert_eq!(refs.len(), 1);
        assert!(refs.contains("owner/repo#1"));
    }

    #[test]
    fn graphql_thread_count_counts_unresolved_threads() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviewThreads": {
                            "nodes": [
                                {"isResolved": true},
                                {"isResolved": false},
                                {"isResolved": false}
                            ]
                        }
                    }
                }
            }
        }"#;

        let count = unresolved_thread_count_from_gql(json).unwrap();

        assert_eq!(count, 2);
    }

    #[test]
    fn graphql_thread_count_parse_error_is_not_faked_as_zero() {
        let err = unresolved_thread_count_from_gql("gh: GraphQL API rate limit exceeded")
            .expect_err("invalid GraphQL output must fail closed");

        assert!(
            matches!(err, crate::errors::DaemonError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }

    #[test]
    fn graphql_thread_count_missing_pull_request_is_not_faked_as_zero() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": null
                }
            }
        }"#;

        let err = unresolved_thread_count_from_gql(json)
            .expect_err("missing pullRequest must fail closed");

        assert!(
            matches!(err, crate::errors::DaemonError::Parse(_)),
            "expected parse error, got {err:?}"
        );
    }
}

#[cfg(test)]
mod ci_bucket_tests {
    use super::ci_success_from_check_buckets;

    #[test]
    fn production_pending_is_not_green() {
        assert!(!ci_success_from_check_buckets(&["pass", "pending"], false));
    }

    #[test]
    fn production_all_pass_or_skip_is_green() {
        assert!(ci_success_from_check_buckets(&["pass", "skipping"], false));
    }

    #[test]
    fn production_fail_is_not_green() {
        assert!(!ci_success_from_check_buckets(&["pass", "fail"], false));
    }

    #[test]
    fn iteration_stub_allows_pending_not_fail() {
        assert!(ci_success_from_check_buckets(&["pass", "pending"], true));
        assert!(!ci_success_from_check_buckets(&["pass", "fail"], true));
    }
}

#[cfg(test)]
mod ci_dedicated_gate_ownership_tests {
    use super::check_owned_by_dedicated_gate;

    // Live incident 2026-07-28 (PR #485): a factory PR with real CI green
    // (test + daemon-tests success) but a failing "Evidence Gate" check-run
    // was classified ci_green=fail and instantly rerolled. Since coders open
    // PRs without evidence markers by construction, gate 1 counting the
    // evidence check-run made every attempt non-convergent.
    #[test]
    fn evidence_gate_check_run_is_owned_by_gate_6_not_ci() {
        assert!(check_owned_by_dedicated_gate("Evidence Gate"));
        assert!(check_owned_by_dedicated_gate(" Evidence Gate "));
    }

    #[test]
    fn real_ci_and_review_bot_check_runs_stay_in_gate_1() {
        for name in ["test", "daemon-tests", "CodeRabbit", "Cursor Bugbot", "notify / notify"] {
            assert!(!check_owned_by_dedicated_gate(name), "{name} must stay in gate 1");
        }
    }
}

/// Regression tests for the ChainLlm fallback chain. The bug bead
/// `jleechan-g1k` flagged — "claude fallback passes
/// `--dangerously-skip-permissions` as message text" — is structurally
/// indistinguishable from "argv got reordered under our feet" until something
/// actually observes the rendered argv. These tests inject a fake `codex` /
/// `claude` / `agy` binary on PATH that dumps its argv (one token per line,
/// argv[0] preserved) so the test can pin both the *order* of the flags and
/// the *cwd* the child was launched in.
///
/// Why this lives in `adapters.rs` rather than a separate test crate: the
/// fake-binary shim is shell-level, so it needs to set PATH before
/// `ChainLlm::judge` runs. Putting the shim in `cargo test`'s setup phase
/// keeps it isolated from production builds (the `#[cfg(test)]` gate).
#[cfg(test)]
mod chain_llm_fallback_argv_tests {
    use super::ChainLlm;
    use crate::tools::Llm;
    use std::sync::Mutex;

    /// Process-wide mutex that serializes the `PATH` / `HOME` mutations
    /// performed by the fallback-argv tests below. Without this guard,
    /// `cargo test` runs unit tests in parallel by default — test A could
    /// call `set_var("PATH", a)` then yield, then test B could call
    /// `set_var("PATH", b)` and observe A's environment restored on B's
    /// failure path (or vice versa), causing one of the two tests to
    /// invoke the real `codex` binary from the system PATH instead of
    /// the argv-dump shim (or to fail because ChainLlm::judge fell through
    /// to a real backend). The mutex holds for the entire mutation +
    /// `ChainLlm::judge` window, so a single shared binary mutex is
    /// sufficient even though two tests exist.
    ///
    /// jleechan-9sl1: delegates to the file-level `gh_env_test_lock()` shared
    /// by every PATH/env-mutating test module in this file -- see that
    /// function's doc comment for why a module-local lock is insufficient
    /// (it only serializes within one module, not across the
    /// `chain_llm_fallback_argv_tests` / `pr_snapshot_checks_fetch_failure_tests`
    /// / `cli_vcs_gh_tests` modules, which all mutate the same global `PATH`).
    fn env_lock() -> &'static Mutex<()> {
        super::gh_env_test_lock()
    }

    /// Write an executable shell script at `path` that prints every element
    /// of its argv, one per line, on stdout. argv[0] (the script path) is
    /// preserved as the first line so tests can assert the child was
    /// actually our shim — not some unrelated binary on PATH.
    fn write_argv_dump_shim(path: &std::path::Path) {
        std::fs::write(
            path,
            "#!/usr/bin/env bash\n\
             printf 'argv0=%s\\n' \"$0\"\n\
             for arg in \"$@\"; do\n\
               printf '%s\\n' \"$arg\"\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(
            path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }

    /// Prepare a temp directory containing a single executable named
    /// `bin_name` that prints its argv. Returns the directory so the
    /// caller can `chdir` into it (or set PATH) before invoking
    /// `ChainLlm::judge`. The directory layout matches what `ChainLlm`
    /// expects for the daemon's own cwd (`FALLBACK_CWD = "."`).
    fn make_argv_dump_dir(prefix: &str, bin_name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("afd_chain_llm_{}_{}", prefix, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        write_argv_dump_shim(&dir.join("bin").join(bin_name));
        dir
    }

    /// Drive `ChainLlm::judge` with a PATH that contains ONLY our shim for
    /// `codex` (which is the first link in the chain), then pin the
    /// argv/cwd the shim observed.
    #[test]
    #[cfg(unix)]
    fn chain_llm_fallback_uses_explicit_cwd_and_argv_order() {
        // Hold the process-wide env mutex for the entire mutation +
        // ChainLlm::judge window so this test cannot interleave with
        // `codex_argv_preserves_flag_boundary` (see ENV_LOCK rationale).
        // On mutex poisoning we re-raise as a regular panic so the test
        // still fails loudly rather than silently skipping.
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Key the temp dir on nanos since parallel test invocations need
        // unique paths (process-id is shared across threads in a binary).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = make_argv_dump_dir(&format!("argv_{nanos}"), "codex");
        let bin = dir.join("bin");

        // Make ChainLlm resolve `codex` from our shim. Other backends
        // (`claude`, `agy`) must NOT be reachable from PATH so the chain
        // stops at the shim — this pins the argv of the FIRST link rather
        // than accidentally exercising the fallback.
        let prior_path = std::env::var_os("PATH");
        let mut new_path = std::ffi::OsString::from(bin.to_str().unwrap());
        if let Some(prior) = prior_path.as_ref() {
            new_path.push(":");
            new_path.push(prior);
        }
        // SAFETY: tests mutate env vars sequentially here. ENV_LOCK above
        // ensures no parallel test from this module can interleave; the
        // per-test temp dir + `nanos` suffix is defense-in-depth in case
        // a future contributor adds a test that does NOT take the lock.
        unsafe { std::env::set_var("PATH", &new_path) };
        let prior_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", dir.to_str().unwrap()) };

        let result = ChainLlm.judge("hello-router-prompt");

        // Restore env first so a failed assertion leaves the test run
        // hygienic for the next case. Drop the guard explicitly after
        // restoration so a panic in the assertions does not skip the
        // env restore (Drop for MutexGuard would not run, but the
        // restore is the test's responsibility regardless).
        unsafe {
            if let Some(prior) = prior_home {
                std::env::set_var("HOME", prior);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(prior) = prior_path {
                std::env::set_var("PATH", prior);
            } else {
                std::env::remove_var("PATH");
            }
        }
        drop(_guard);

        let captured = result.expect("codex shim should succeed");

        // The shim prepends a `argv0=<path>` marker line followed by one
        // line per remaining argv slot (argv[1..]). Strip the marker and
        // compare against the expected argv (argv[1..]).
        let mut lines = captured.lines();
        let argv0_line = lines
            .next()
            .expect("argv0 marker present in shim output");
        assert!(
            argv0_line.starts_with("argv0="),
            "shim output must start with the argv0 marker; got {argv0_line:?}"
        );

        let actual_args: Vec<&str> = lines.collect();

        let expected = &["exec", "--yolo", "--skip-git-repo-check", "hello-router-prompt"];
        assert_eq!(
            actual_args, expected,
            "codex fallback argv mismatch — got {actual_args:?}, expected {expected:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pin that the codex shim path does NOT swallow `--yolo` /
    /// `--skip-git-repo-check` into a single argv slot, which is the
    /// structural failure mode that lets a `--dangerously-skip-permissions`
    /// flag accidentally become part of the message text (bead
    /// `jleechan-g1k`).
    #[test]
    #[cfg(unix)]
    fn codex_argv_preserves_flag_boundary() {
        // Hold the same process-wide env mutex as the sibling test so
        // these two cannot interleave their `PATH` / `HOME` mutations
        // (see ENV_LOCK rationale).
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = make_argv_dump_dir(&format!("bnd_{nanos}"), "codex");
        let bin = dir.join("bin");

        let prior_path = std::env::var_os("PATH");
        let mut new_path = std::ffi::OsString::from(bin.to_str().unwrap());
        if let Some(prior) = prior_path.as_ref() {
            new_path.push(":");
            new_path.push(prior);
        }
        unsafe { std::env::set_var("PATH", &new_path) };
        let prior_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", dir.to_str().unwrap()) };

        let result = ChainLlm.judge("boundary-check");

        unsafe {
            if let Some(prior) = prior_home {
                std::env::set_var("HOME", prior);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(prior) = prior_path {
                std::env::set_var("PATH", prior);
            } else {
                std::env::remove_var("PATH");
            }
        }
        drop(_guard);

        let captured = result.expect("codex shim should succeed");
        let mut lines = captured.lines();
        let argv0_line = lines
            .next()
            .expect("argv0 marker present in shim output");
        assert!(
            argv0_line.starts_with("argv0="),
            "shim output must start with the argv0 marker; got {argv0_line:?}"
        );
        let actual_args: Vec<&str> = lines.collect();

        assert_eq!(
            actual_args,
            vec!["exec", "--yolo", "--skip-git-repo-check", "boundary-check"],
            "codex argv must keep each flag as a separate argv slot — the structural invariant that prevents --dangerously-skip-permissions from becoming message text"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Regression tests for jleechan-e7lp: `pr_snapshot`'s checks-fetch fallback
/// must distinguish "we could not fetch CI status at all" (primary `gh pr
/// checks` AND the REST `check-runs` fallback both failed / returned
/// unparseable output) from "we fetched successfully and the PR genuinely
/// has zero checks yet". Before this fix both shapes silently collapsed into
/// an empty `checks` vec, which fabricated `ci_pending = true` even when the
/// daemon had no real signal — the live incident is bead jleechan-93ft / PR
/// jleechanorg/worldarchitect.ai#7888 logging VERIFICATION_PENDING 244+
/// times in 10 minutes while GraphQL was rate-limited and the PR's CI was
/// already 100% terminal.
///
/// These tests inject a fake `gh` binary on PATH (same technique as
/// `chain_llm_fallback_argv_tests` above) that dispatches on subcommand /
/// REST path so `CliScm::pr_snapshot` exercises the real fallback code path
/// end-to-end without touching the network.
#[cfg(test)]
mod pr_snapshot_checks_fetch_failure_tests {
    use super::CliScm;

    /// r6: dedicated mutex used by `capped_vendor_excluded_from_ci_success_and_status`
    /// and `capped_bugbot_excluded_from_ci_success_and_status` to
    /// serialise the cap-record / cap-clear / snapshot-read sequence
    /// across the two peer tests. Cannot use `env_lock()` here
    /// because `run_pr_snapshot_with_fake_gh` acquires `env_lock()`
    /// internally, and re-entering it would self-deadlock. This
    /// mutex is module-local to the test module so it has no
    /// contention with any production code path.
    static CAP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use crate::errors::DaemonError;
    use crate::tools::Scm;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// jleechan-9sl1: delegates to the file-level `gh_env_test_lock()` shared
    /// by every PATH/env-mutating test module in this file (previously this
    /// module had its own independent lock, which only serialized against
    /// itself, not against `chain_llm_fallback_argv_tests` or
    /// `cli_vcs_gh_tests` -- see `gh_env_test_lock`'s doc comment for the
    /// cross-module race that caused).
    fn env_lock() -> &'static Mutex<()> {
        super::gh_env_test_lock()
    }

    /// Write a `gh` shim that answers every call `CliScm::pr_snapshot` makes:
    /// - `gh pr view ...` -> a fixed, valid PR view JSON.
    /// - `gh pr checks ...` -> controlled by `$GH_TEST_PRIMARY_CHECKS`.
    /// - `gh api .../check-runs` -> controlled by `$GH_TEST_FALLBACK_CHECKS`.
    /// - `gh api .../statuses` -> always an empty legacy-status array.
    /// - `gh api graphql ...` -> always an empty review-threads reply (this
    ///   daemon already tolerates GraphQL failure here independently of the
    ///   checks fetch under test, so keeping it green isolates the
    ///   assertion to the checks path).
    ///
    /// `$GH_TEST_PRIMARY_CHECKS` / `$GH_TEST_FALLBACK_CHECKS` values:
    /// - `fail` -> exit 1 (simulates a `gh` invocation failure: rate limit,
    ///   network, timeout).
    /// - `badjson` -> exit 0 with a non-JSON body (simulates a malformed
    ///   response).
    /// - anything else -> exit 0 with a genuinely empty checks array.
    fn write_fake_gh(path: &std::path::Path) {
        let script = r#"#!/usr/bin/env bash
set -u
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  cat <<'JSON'
{"mergeable":"MERGEABLE","reviews":[],"headRefOid":"deadbeefcafefeed0123456789abcdef01234567","body":"test body","comments":[],"files":[],"updatedAt":"2026-07-08T12:00:00Z"}
JSON
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "checks" ]; then
  case "${GH_TEST_PRIMARY_CHECKS:-}" in
    fail) echo "gh: GraphQL API rate limit already exceeded" >&2; exit 1 ;;
    badjson) echo "not json"; exit 0 ;;
    coderabbit_pending) echo '[{"state":"SUCCESS","bucket":"pass","name":"build"},{"state":"PENDING","bucket":"pending","name":"CodeRabbit"}]'; exit 0 ;;
    bugbot_pending) echo '[{"state":"SUCCESS","bucket":"pass","name":"build"},{"state":"PENDING","bucket":"pending","name":"Bugbot"}]'; exit 0 ;;
    *) echo "[]"; exit 0 ;;
  esac
fi
if [ "$1" = "api" ]; then
  url=""
  for arg in "$@"; do
    case "$arg" in
      api) continue ;;
      -*) continue ;;
      *) url="$arg"; break ;;
    esac
  done
  case "$url" in
    *check-runs*)
      case "${GH_TEST_FALLBACK_CHECKS:-}" in
        fail) echo "gh: network error" >&2; exit 1 ;;
        badjson) echo "not json"; exit 0 ;;
        *) echo '{"check_runs": []}'; exit 0 ;;
      esac
      ;;
    *statuses*)
      echo "[]"; exit 0
      ;;
    graphql)
      echo '{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[]}}}}}'
      exit 0
      ;;
    *)
      echo "{}"; exit 0
      ;;
  esac
fi
echo "pr_snapshot_checks_fetch_failure_tests: unhandled gh invocation: $*" >&2
exit 1
"#;
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Prepare a temp dir containing only the fake `gh` shim and return it
    /// (caller prepends `<dir>/bin` to PATH). Keyed on nanos + pid so
    /// concurrent test binaries never collide.
    fn make_fake_gh_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "afd_pr_snapshot_checks_{prefix}_{}_{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        write_fake_gh(&dir.join("bin").join("gh"));
        dir
    }

    /// Run `CliScm::pr_snapshot(pr)` with the fake `gh` shim on PATH and the
    /// two checks-fetch env knobs set, restoring PATH/env afterwards
    /// regardless of outcome.
    fn run_pr_snapshot_with_fake_gh(
        prefix: &str,
        primary_checks: &str,
        fallback_checks: &str,
        pr: u64,
    ) -> Result<crate::tools::PrSnapshot, DaemonError> {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let dir = make_fake_gh_dir(prefix);
        let bin = dir.join("bin");

        let prior_path = std::env::var_os("PATH");
        let mut new_path = std::ffi::OsString::from(bin.to_str().unwrap());
        if let Some(prior) = prior_path.as_ref() {
            new_path.push(":");
            new_path.push(prior);
        }
        let prior_primary = std::env::var_os("GH_TEST_PRIMARY_CHECKS");
        let prior_fallback = std::env::var_os("GH_TEST_FALLBACK_CHECKS");
        // SAFETY: serialized by ENV_LOCK above, matching the established
        // `chain_llm_fallback_argv_tests` pattern for mutating process env
        // vars in tests.
        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("GH_TEST_PRIMARY_CHECKS", primary_checks);
            std::env::set_var("GH_TEST_FALLBACK_CHECKS", fallback_checks);
        }

        let scm = CliScm::new("jleechanorg/dark-factory-test".to_string());
        let result = scm.pr_snapshot(pr);

        unsafe {
            if let Some(prior) = prior_path {
                std::env::set_var("PATH", prior);
            } else {
                std::env::remove_var("PATH");
            }
            if let Some(prior) = prior_primary {
                std::env::set_var("GH_TEST_PRIMARY_CHECKS", prior);
            } else {
                std::env::remove_var("GH_TEST_PRIMARY_CHECKS");
            }
            if let Some(prior) = prior_fallback {
                std::env::set_var("GH_TEST_FALLBACK_CHECKS", prior);
            } else {
                std::env::remove_var("GH_TEST_FALLBACK_CHECKS");
            }
        }
        drop(_guard);

        std::fs::remove_dir_all(&dir).ok();
        result
    }

    /// (a) Primary `gh pr checks` fails AND the REST `check-runs` fallback
    /// also fails to execute -> `pr_snapshot` must return `Err`, not a
    /// fabricated `Ok(snapshot)` with `ci_pending = true` synthesized from
    /// an empty checks vec. This is the exact jleechan-93ft incident shape.
    #[test]
    #[cfg(unix)]
    fn both_checks_fetch_paths_failing_returns_err_not_fabricated_pending() {
        let result = run_pr_snapshot_with_fake_gh("both_fail", "fail", "fail", 7888);
        let err = result.expect_err(
            "pr_snapshot must fail closed when BOTH the primary `gh pr checks` \
             call and the REST check-runs fallback fail to execute -- \
             fabricating an empty checks vec here is exactly the \
             jleechan-93ft VERIFICATION_PENDING-spam bug",
        );
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "expected DaemonError::Tool (transient, retried next tick per \
             jleechan-qdw), got {err:?}"
        );
    }

    /// Same as above but the REST fallback executes and returns a 200 with a
    /// non-JSON body -- must also fail closed rather than defaulting to an
    /// empty checks vec.
    #[test]
    #[cfg(unix)]
    fn fallback_non_json_response_returns_err_not_fabricated_pending() {
        let result = run_pr_snapshot_with_fake_gh("fallback_badjson", "fail", "badjson", 7888);
        let err = result.expect_err(
            "pr_snapshot must fail closed when the REST check-runs fallback \
             returns a body that isn't valid JSON",
        );
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "expected DaemonError::Tool, got {err:?}"
        );
    }

    /// (b) Both calls execute successfully and genuinely report zero
    /// checks (the primary `gh pr checks` failed, forcing the REST
    /// fallback, which itself succeeds with an empty `check_runs` array --
    /// i.e. the PR really has no checks yet). This must still produce
    /// `ci_pending = true`, preserving the pre-existing correct behavior;
    /// the fix is narrowly about distinguishing fetch failure from genuine
    /// emptiness, not about changing empty-checks-means-pending semantics.
    #[test]
    #[cfg(unix)]
    fn genuinely_empty_checks_via_fallback_still_reports_ci_pending() {
        let result = run_pr_snapshot_with_fake_gh("genuinely_empty", "fail", "empty", 42);
        let snapshot = result.expect(
            "pr_snapshot must succeed when the REST fallback executes and \
             genuinely reports zero checks",
        );
        assert!(
            snapshot.ci_pending,
            "a PR with genuinely zero checks yet must still report \
             ci_pending = true (pre-existing correct behavior)"
        );
        assert_eq!(snapshot.ci_status, "unknown");
    }

    /// Same as (b) but checks come back empty directly from the PRIMARY
    /// `gh pr checks` call (no fallback needed at all) -- the baseline
    /// "PR just opened" case, confirming the fix didn't touch this path.
    #[test]
    #[cfg(unix)]
    fn genuinely_empty_checks_via_primary_still_reports_ci_pending() {
        let result = run_pr_snapshot_with_fake_gh("genuinely_empty_primary", "empty", "empty", 43);
        let snapshot = result.expect(
            "pr_snapshot must succeed when the primary call executes and \
             genuinely reports zero checks",
        );
        assert!(snapshot.ci_pending);
        assert_eq!(snapshot.ci_status, "unknown");
    }

    #[test]
    #[cfg(unix)]
    fn capped_vendor_excluded_from_ci_success_and_status() {
        use crate::vendor_health::{Vendor, CapObservation, CapSource, VendorHealthLedger};
        use std::sync::{Arc, Mutex};

        // r6: hold the dedicated `CAP_TEST_LOCK` across the entire
        // test body (not just inside `run_pr_snapshot_with_fake_gh`)
        // so the peer Bugbot regression test cannot clear our caps
        // between our step-2 record and step-3 assertion when Rust
        // runs the lib-test suite in parallel.
        let _test_lock = CAP_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // r6: switch from `set_global_ledger` (OnceLock no-op after
        // first call) to `get_or_init` so the peer Bugbot regression
        // test can share the same global ledger instead of being
        // silently no-op'd by a populated OnceLock.
        let global: Arc<Mutex<VendorHealthLedger>> = crate::vendor_health::GLOBAL_LEDGER
            .get_or_init(|| Arc::new(Mutex::new(VendorHealthLedger::new())))
            .clone();

        // Clean baseline for BOTH vendors so the Bugbot peer's
        // leftover caps cannot leak into step 1.
        global.lock().unwrap().clear(Vendor::CodeRabbit);
        global.lock().unwrap().clear(Vendor::Bugbot);

        // 1. Without being capped, pending CodeRabbit makes CI success false and status unknown
        let result = run_pr_snapshot_with_fake_gh("not_capped", "coderabbit_pending", "fail", 44);
        let snapshot = result.expect("pr_snapshot must succeed");
        assert!(!snapshot.ci_success);
        assert!(snapshot.ci_pending);
        assert_eq!(snapshot.ci_status, "unknown");

        // 2. Set CodeRabbit as capped (mutate the shared global ledger).
        {
            let mut guard = global.lock().unwrap();
            for ts in 1..=3 {
                guard.record_cap(CapObservation {
                    vendor: Vendor::CodeRabbit,
                    source: CapSource::UnknownGateRepeated,
                    bead_id: format!("bead-{}", ts),
                    pr_number: 44,
                    ts_epoch: ts,
                    note: "test fixture".into(),
                });
            }
        }

        // 3. With CodeRabbit capped, the pending CodeRabbit check is excluded, making CI green
        let result = run_pr_snapshot_with_fake_gh("capped", "coderabbit_pending", "fail", 44);
        let snapshot = result.expect("pr_snapshot must succeed");
        assert!(snapshot.ci_success);
        assert!(!snapshot.ci_pending);
        assert_eq!(snapshot.ci_status, "green");

        // Clean up BOTH vendors so any subsequent test (including
        // the Bugbot peer, if re-ordered) starts from a clean slate.
        global.lock().unwrap().clear(Vendor::CodeRabbit);
        global.lock().unwrap().clear(Vendor::Bugbot);
    }

    /// r6 regression: the collapsible_if clippy fix collapsed the
    /// nested `if name_lower.contains(...) { if is_global_vendor_capped(...) }`
    /// into a single `if cond1 && cond2 { continue; }` for BOTH
    /// CodeRabbit and Bugbot. This test pins the Bugbot side so a future
    /// refactor of the collapsed if (e.g. extracting a helper, inverting
    /// the predicate) cannot silently regress Bugbot filter behaviour.
    ///
    /// Test-isolation note: `set_global_ledger` is a `OnceLock.set`,
    /// so it can only populate the global slot ONCE for the whole
    /// test binary. The matching CodeRabbit-side test
    /// (`capped_vendor_excluded_from_ci_success_and_status`) is the
    /// canonical populator; this Bugbot-side test relies on the
    /// shared `Arc<Mutex<VendorHealthLedger>>` already being installed.
    ///
    /// Because Rust runs tests in alphabetical order within a single
    /// binary by default, this test (`capped_bugbot_…`) runs BEFORE
    /// the CodeRabbit peer (`capped_vendor_…`). We therefore CANNOT
    /// call `set_global_ledger` (it would either succeed and prevent
    /// the peer test from installing its own ledger, or be a no-op and
    /// leave no global state at all). Instead we install a fresh
    /// ledger exactly once for the whole binary via
    /// `OnceLock::get_or_init`, and both peer tests mutate that
    /// shared ledger in place through `record_cap` / `clear`. Any
    /// leftover caps are cleared at exit so neither peer test's
    /// baseline state is poisoned by the other.
    ///
    /// The env/lock guard is held across the entire test body so the
    /// peer CodeRabbit test cannot interleave its `clear(Vendor::CodeRabbit)`
    /// between our step-2 record and step-3 assertion when Rust runs
    /// the lib-test suite in parallel.
    #[test]
    #[cfg(unix)]
    fn capped_bugbot_excluded_from_ci_success_and_status() {
        use crate::vendor_health::{Vendor, CapObservation, CapSource, VendorHealthLedger};
        use std::sync::{Arc, Mutex};

        // Hold the dedicated `CAP_TEST_LOCK` across the entire test
        // body so the peer CodeRabbit test cannot interleave its
        // `clear` calls against our cap-record window when Rust runs
        // the lib-test suite in parallel. We use a dedicated mutex
        // (not `env_lock`) because `run_pr_snapshot_with_fake_gh`
        // already acquires `env_lock` internally; re-entering would
        // self-deadlock.
        let _test_lock = CAP_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Install the global ledger exactly once for this test binary.
        // `OnceLock::get_or_init` is the only safe way to populate it
        // from more than one test without poisoning the peer's
        // `set_global_ledger` call.
        let global: Arc<Mutex<VendorHealthLedger>> = crate::vendor_health::GLOBAL_LEDGER
            .get_or_init(|| Arc::new(Mutex::new(VendorHealthLedger::new())))
            .clone();

        // Clean baseline for BOTH vendors so any leftover cap from a
        // prior test cannot leak into step 1.
        global.lock().unwrap().clear(Vendor::CodeRabbit);
        global.lock().unwrap().clear(Vendor::Bugbot);

        // 1. Without being capped, pending Bugbot makes CI success false and status unknown
        let result = run_pr_snapshot_with_fake_gh("not_capped_bugbot", "bugbot_pending", "fail", 45);
        let snapshot = result.expect("pr_snapshot must succeed");
        assert!(!snapshot.ci_success);
        assert!(snapshot.ci_pending);
        assert_eq!(snapshot.ci_status, "unknown");

        // 2. Mark Bugbot as capped (mutate the shared global ledger in place).
        {
            let mut guard = global.lock().unwrap();
            for ts in 1..=3 {
                guard.record_cap(CapObservation {
                    vendor: Vendor::Bugbot,
                    source: CapSource::UnknownGateRepeated,
                    bead_id: format!("bead-bugbot-{}", ts),
                    pr_number: 45,
                    ts_epoch: ts,
                    note: "test fixture".into(),
                });
            }
        }

        // 3. With Bugbot capped, the pending Bugbot check is excluded, making CI green
        let result = run_pr_snapshot_with_fake_gh("capped_bugbot", "bugbot_pending", "fail", 45);
        let snapshot = result.expect("pr_snapshot must succeed");
        assert!(
            snapshot.ci_success,
            "Bugbot-capped PR must report ci_success=true (cap filter dropped the pending Bugbot check)"
        );
        assert!(!snapshot.ci_pending);
        assert_eq!(snapshot.ci_status, "green");

        // Clean up BOTH vendors so the shared global ledger is left
        // in a state where the CodeRabbit peer test (and any
        // subsequent test) can install its own vendor caps without
        // residue from this test polluting the global snapshot.
        global.lock().unwrap().clear(Vendor::Bugbot);
        global.lock().unwrap().clear(Vendor::CodeRabbit);
    }
}

/// Regression + correctness tests for jleechan-9sl1: `CliVcs::remote_head_sha`
/// and `CliVcs::is_ancestor` used to shell out to the *local* `git` binary
/// (`git fetch origin <branch>` / `git merge-base --is-ancestor`), which only
/// ever operates against the DAEMON's own cwd/repo (its systemd
/// `WorkingDirectory`) -- never against `cfg.target_repo`, a completely
/// different repo the daemon automates. That bug made stage-2 adopted-PR
/// remediation 100% non-functional for any real target repo: `reroll.rs`'s
/// `execute_adopted`-style path calls `remote_head_sha` to capture a
/// pre-session baseline SHA before dispatching a coder session (fails every
/// time), and `tick.rs`'s force-push-detection sweep calls both methods on
/// every tick for every `Dispatched` adopted bead and is explicitly
/// fail-closed, so even a lucky dispatch would get falsely flagged as a
/// history rewrite on the very next tick.
///
/// These tests inject a fake `gh` binary on PATH (same technique as
/// `pr_snapshot_checks_fetch_failure_tests` above) that answers
/// `gh api repos/<repo>/commits/<branch> --jq .sha` and
/// `gh api repos/<repo>/compare/<a>...<b> --jq .status` calls, and logs every
/// invocation's argv to a file so tests can assert on the EXACT URL
/// `CliVcs` constructed -- proving both that it targets the configured
/// `target_repo` (not the daemon's own repo) and that a branch name
/// containing a `/` (e.g. `fix/7887-cc-finish-level-commit`, a real branch
/// name from the jleechan-93ft incident) survives intact rather than being
/// mangled by naive string splitting. Written BEFORE the `gh api`
/// reimplementation landed: at that point these tests failed because the old
/// code shelled out to local `git` (which never even invokes the fake `gh`
/// shim, and fails outright since no local git object/remote-tracking ref
/// for these synthetic branches/SHAs exists in this repo).
#[cfg(test)]
mod cli_vcs_gh_tests {
    use super::CliVcs;
    use crate::tools::Vcs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// jleechan-9sl1: delegates to the file-level `gh_env_test_lock()` shared
    /// by every PATH/env-mutating test module in this file -- a module-local
    /// lock here would only serialize this module's own tests against each
    /// other, not against `chain_llm_fallback_argv_tests` or
    /// `pr_snapshot_checks_fetch_failure_tests`, which mutate the same
    /// global `PATH`. See `gh_env_test_lock`'s doc comment for the
    /// cross-module flake this fixes.
    fn env_lock() -> &'static Mutex<()> {
        super::gh_env_test_lock()
    }

    /// Write a `gh` shim that answers `gh api <path> --jq <filter>` calls for
    /// the "commits" (`remote_head_sha`) and "compare" (`is_ancestor`)
    /// endpoints, scoped to `$GH_TEST_TARGET_REPO` (any other repo path is
    /// treated as an unknown/404 repo, mirroring a real `gh api` 404 for a
    /// repo the caller isn't authorized against or that doesn't exist), and
    /// appends every invocation's argv (one arg per line, `---` separator
    /// between calls) to `$GH_TEST_ARGV_LOG` when set.
    fn write_fake_gh_vcs(path: &std::path::Path) {
        let script = r#"#!/usr/bin/env bash
set -u
if [ -n "${GH_TEST_ARGV_LOG:-}" ]; then
  for a in "$@"; do printf '%s\n' "$a" >> "$GH_TEST_ARGV_LOG"; done
  printf -- '---\n' >> "$GH_TEST_ARGV_LOG"
fi

if [ "${1:-}" != "api" ]; then
  echo "cli_vcs_gh_tests fake gh: unhandled invocation: $*" >&2
  exit 1
fi
shift

url_path=""
jq_filter=""
while [ $# -gt 0 ]; do
  case "$1" in
    --jq)
      jq_filter="$2"
      shift 2
      ;;
    -*)
      shift
      ;;
    *)
      url_path="$1"
      shift
      ;;
  esac
done

repo="${GH_TEST_TARGET_REPO:-}"
case "$url_path" in
  "repos/${repo}/commits/"*)
    if [ "$jq_filter" = ".sha" ]; then
      echo "${GH_TEST_SHA:-}"
      exit 0
    fi
    ;;
  "repos/${repo}/git/ref/heads/"*)
    if [ "$jq_filter" = ".object.sha" ]; then
      echo "${GH_TEST_SHA:-}"
      exit 0
    fi
    ;;
  "repos/${repo}/compare/"*)
    if [ "$jq_filter" = ".status" ]; then
      echo "${GH_TEST_STATUS:-}"
      exit 0
    fi
    ;;
esac
echo "cli_vcs_gh_tests fake gh: unhandled or repo-mismatched path: $url_path" >&2
exit 1
"#;
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn make_fake_gh_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "afd_cli_vcs_gh_{prefix}_{}_{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        write_fake_gh_vcs(&dir.join("bin").join("gh"));
        dir
    }

    /// Prepend `dir/bin` to PATH, set the `GH_TEST_*` env knobs, run `f`,
    /// then restore PATH/env regardless of outcome. `argv_log` (if `Some`) is
    /// where the shim will append every call's argv. Serialized by
    /// `ENV_LOCK` since `cargo test` runs tests in this module in parallel
    /// within one process and PATH/env are process-global.
    fn with_fake_gh<T>(
        prefix: &str,
        target_repo_env: &str,
        sha_env: &str,
        status_env: &str,
        argv_log: Option<&std::path::Path>,
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_fake_gh_dir(prefix);
        let bin = dir.join("bin");

        let prior_path = std::env::var_os("PATH");
        let mut new_path = std::ffi::OsString::from(bin.to_str().unwrap());
        if let Some(prior) = prior_path.as_ref() {
            new_path.push(":");
            new_path.push(prior);
        }
        let prior_repo = std::env::var_os("GH_TEST_TARGET_REPO");
        let prior_sha = std::env::var_os("GH_TEST_SHA");
        let prior_status = std::env::var_os("GH_TEST_STATUS");
        let prior_log = std::env::var_os("GH_TEST_ARGV_LOG");
        // SAFETY: serialized by ENV_LOCK above, matching the established
        // `pr_snapshot_checks_fetch_failure_tests` pattern for mutating
        // process env vars in tests.
        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("GH_TEST_TARGET_REPO", target_repo_env);
            std::env::set_var("GH_TEST_SHA", sha_env);
            std::env::set_var("GH_TEST_STATUS", status_env);
            if let Some(log) = argv_log {
                std::env::set_var("GH_TEST_ARGV_LOG", log);
            } else {
                std::env::remove_var("GH_TEST_ARGV_LOG");
            }
        }

        let result = f();

        unsafe {
            match prior_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
            match prior_repo {
                Some(p) => std::env::set_var("GH_TEST_TARGET_REPO", p),
                None => std::env::remove_var("GH_TEST_TARGET_REPO"),
            }
            match prior_sha {
                Some(p) => std::env::set_var("GH_TEST_SHA", p),
                None => std::env::remove_var("GH_TEST_SHA"),
            }
            match prior_status {
                Some(p) => std::env::set_var("GH_TEST_STATUS", p),
                None => std::env::remove_var("GH_TEST_STATUS"),
            }
            match prior_log {
                Some(p) => std::env::set_var("GH_TEST_ARGV_LOG", p),
                None => std::env::remove_var("GH_TEST_ARGV_LOG"),
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        result
    }

    /// `remote_head_sha` must call `gh api repos/<target_repo>/commits/<branch>
    /// --jq .sha` -- scoped to the CONFIGURED target repo, not the daemon's
    /// own repo -- and the branch's embedded `/` (a real branch name shape,
    /// e.g. `fix/7887-cc-finish-level-commit` from jleechan-93ft) must
    /// survive intact in the constructed URL rather than being mangled by
    /// naive string splitting.
    #[test]
    #[cfg(unix)]
    fn remote_head_sha_targets_configured_repo_and_preserves_branch_slash() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let log_dir = std::env::temp_dir().join(format!(
            "afd_cli_vcs_argvlog_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("argv.log");

        let expected_sha = "deadbeefcafefeed0123456789abcdef01234567";
        let branch = "fix/7887-cc-finish-level-commit";

        let result = with_fake_gh(
            "remote_head_sha_slash",
            "some-owner/some-repo",
            expected_sha,
            "",
            Some(&log_path),
            || {
                let vcs = CliVcs::new("some-owner/some-repo".to_string());
                vcs.remote_head_sha(branch)
            },
        );

        assert_eq!(
            result.expect("remote_head_sha should succeed against the matching target_repo"),
            expected_sha
        );

        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            log.contains("repos/some-owner/some-repo/commits/fix/7887-cc-finish-level-commit"),
            "expected the exact target_repo+branch URL (with the branch's \
             embedded slash intact) in the logged gh invocation, got:\n{log}"
        );
        assert!(
            log.contains(".sha"),
            "expected --jq .sha in the logged gh invocation, got:\n{log}"
        );

        std::fs::remove_dir_all(&log_dir).ok();
    }

    /// Bead dark-factory-mw85: test `head_sha_within_for_repo` calls `gh api repos/<repo>/git/ref/heads/<branch>`
    /// with `--jq .object.sha`.
    #[test]
    #[cfg(unix)]
    fn head_sha_within_for_repo_targets_configured_repo_and_preserves_branch() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let log_dir = std::env::temp_dir().join(format!(
            "afd_cli_vcs_argvlog_mw85_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("argv.log");

        let expected_sha = "1234567890abcdef1234567890abcdef12345678";
        let target_repo = "other-owner/other-repo";
        let branch = "factory/mw85-r1";

        let result = with_fake_gh(
            "head_sha_within_for_repo_slash",
            target_repo,
            expected_sha,
            "",
            Some(&log_path),
            || {
                let vcs = CliVcs::new("daemon-owner/daemon-repo".to_string());
                vcs.head_sha_within_for_repo(target_repo, branch, 10)
            },
        );

        assert_eq!(
            result.expect("head_sha_within_for_repo should succeed"),
            expected_sha
        );

        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert!(
            log.contains("repos/other-owner/other-repo/git/ref/heads/factory/mw85-r1"),
            "expected the exact target_repo+branch URL in the logged gh invocation, got:\n{log}"
        );
        assert!(
            log.contains(".object.sha"),
            "expected --jq .object.sha in the logged gh invocation, got:\n{log}"
        );

        std::fs::remove_dir_all(&log_dir).ok();
    }

    /// Companion to the above: the SAME fake `gh` shim only answers for
    /// `some-owner/some-repo`; constructing `CliVcs` against a DIFFERENT
    /// repo must fail, proving `remote_head_sha` is actually scoped to
    /// `self.target_repo` rather than the shim just always answering
    /// regardless of the URL it was asked for.
    #[test]
    #[cfg(unix)]
    fn remote_head_sha_wrong_target_repo_returns_err() {
        let result = with_fake_gh(
            "remote_head_sha_wrong_repo",
            "some-owner/some-repo",
            "deadbeefcafefeed0123456789abcdef01234567",
            "",
            None,
            || {
                let vcs = CliVcs::new("wrong-owner/wrong-repo".to_string());
                vcs.remote_head_sha("main")
            },
        );
        assert!(
            result.is_err(),
            "remote_head_sha against a repo the fake gh doesn't recognize \
             must fail, not silently succeed: {result:?}"
        );
    }

    /// `--jq .sha` prints the literal string `null` (exit 0) when the JSON
    /// path doesn't resolve (e.g. a genuinely malformed/empty API response)
    /// -- this must be treated as an error, not returned as a bogus SHA.
    #[test]
    #[cfg(unix)]
    fn remote_head_sha_null_output_is_err() {
        let result = with_fake_gh(
            "remote_head_sha_null",
            "some-owner/some-repo",
            "null",
            "",
            None,
            || {
                let vcs = CliVcs::new("some-owner/some-repo".to_string());
                vcs.remote_head_sha("main")
            },
        );
        assert!(
            result.is_err(),
            "a literal 'null' --jq .sha output must not be treated as a \
             valid SHA: {result:?}"
        );
    }

    /// `is_ancestor` status-mapping matrix (compare API status is relative to
    /// `base...head` = `ancestor_sha...descendant_sha`):
    /// `identical`/`ahead` -> true (ancestor_sha's history is fully contained
    /// in descendant_sha's), `behind`/`diverged` -> false.
    #[test]
    #[cfg(unix)]
    fn is_ancestor_status_mapping_matrix() {
        let cases = [
            ("identical", true),
            ("ahead", true),
            ("behind", false),
            ("diverged", false),
        ];
        for (status, expected) in cases {
            let result = with_fake_gh(
                &format!("is_ancestor_{status}"),
                "some-owner/some-repo",
                "",
                status,
                None,
                || {
                    let vcs = CliVcs::new("some-owner/some-repo".to_string());
                    vcs.is_ancestor(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    )
                },
            );
            assert_eq!(
                result.unwrap_or_else(|e| panic!(
                    "is_ancestor should succeed for status '{status}': {e:?}"
                )),
                expected,
                "status '{status}' mapped to the wrong boolean"
            );
        }
    }

    /// An unrecognized/garbage compare status must propagate as `Err`, not
    /// be silently folded into `Ok(false)` -- callers rely on `Err` vs
    /// `Ok(false)` staying distinct internally even though both are treated
    /// the same way (fail-closed / escalate-to-human) by the force-push-
    /// detection caller in `tick.rs`.
    #[test]
    #[cfg(unix)]
    fn is_ancestor_unrecognized_status_is_err() {
        let result = with_fake_gh(
            "is_ancestor_garbage",
            "some-owner/some-repo",
            "",
            "???",
            None,
            || {
                let vcs = CliVcs::new("some-owner/some-repo".to_string());
                vcs.is_ancestor(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                )
            },
        );
        assert!(
            result.is_err(),
            "an unrecognized compare status must not silently become \
             Ok(false): {result:?}"
        );
    }

    /// `ancestor_sha == descendant_sha` must short-circuit to `Ok(true)`
    /// WITHOUT ever invoking `gh` -- proven here by removing `gh` (and every
    /// other binary) from PATH entirely, so any accidental subprocess call
    /// would fail with "No such file or directory" and this test would fail
    /// closed instead of coincidentally passing.
    #[test]
    #[cfg(unix)]
    fn is_ancestor_identical_sha_short_circuits_without_gh_call() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let empty_dir = std::env::temp_dir().join(format!(
            "afd_cli_vcs_no_gh_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&empty_dir).unwrap();

        let prior_path = std::env::var_os("PATH");
        // PATH containing ONLY an empty directory -- no `gh`, no `git`,
        // nothing. Any subprocess call this code path makes will fail.
        unsafe {
            std::env::set_var("PATH", &empty_dir);
        }

        let sha = "cccccccccccccccccccccccccccccccccccccccc";
        let vcs = CliVcs::new("some-owner/some-repo".to_string());
        let result = vcs.is_ancestor(sha, sha);

        unsafe {
            match prior_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        std::fs::remove_dir_all(&empty_dir).ok();

        assert!(
            result.unwrap_or_else(|e| panic!(
                "identical-SHA short-circuit must succeed even with no gh on PATH: {e:?}"
            )),
            "ancestor_sha == descendant_sha must short-circuit to Ok(true)"
        );
    }

    /// advice-627-630-20260809 PR #628 finding 1 (the MISSING REGRESSION
    /// GUARD): every other test in this module -- including
    /// `head_sha_within_for_repo_targets_configured_repo_and_preserves_branch`
    /// above -- runs `with_fake_gh`'s closure IN-PROCESS from whatever cwd
    /// `cargo test` was invoked from (the checkout root, a real git work
    /// tree). `with_fake_gh` never touches cwd at all. None of them would
    /// catch a regression that reintroduced a cwd-bound `git` subprocess
    /// call into `head_sha_within_for_repo` (e.g. reverting its body back to
    /// `self.head_sha_within(branch, timeout_secs)`, which shells out to
    /// `git rev-parse <branch>` in the CALLING PROCESS's cwd) -- exactly the
    /// installed, non-git daemon `WorkingDirectory` bug bead dark-factory-mw85
    /// exists to fix.
    ///
    /// This test proves the probe survives a GENUINELY non-git process cwd.
    /// It never calls `std::env::set_current_dir` in this shared,
    /// `cargo test`-parallel process (that would race every other test in
    /// the binary sharing the same process cwd) -- instead it re-invokes the
    /// test binary itself as a child process (the established pattern in
    /// this crate, see `target_worktree.rs`'s
    /// `refreshes_clean_managed_checkout_to_stale_snapshot` and siblings),
    /// with `Command::current_dir` pointed at a freshly created directory
    /// that is verified non-git FIRST (a real `git rev-parse --show-toplevel`
    /// must fail there) before the child ever runs.
    #[test]
    #[cfg(unix)]
    fn head_sha_within_for_repo_survives_non_git_daemon_cwd() {
        if std::env::var_os("AFD_HEAD_SHA_NONGIT_HELPER").is_some() {
            // Child mode: this process's OWN cwd (set by the parent's
            // `Command::current_dir`) is the freshly created, verified-non-git
            // directory. If `head_sha_within_for_repo` ever regresses to a
            // cwd-bound `git rev-parse` call, this directory has no `.git`
            // for it to find and the call fails right here.
            let target_repo = std::env::var("AFD_HEAD_SHA_NONGIT_REPO")
                .expect("parent must set AFD_HEAD_SHA_NONGIT_REPO");
            let branch = std::env::var("AFD_HEAD_SHA_NONGIT_BRANCH")
                .expect("parent must set AFD_HEAD_SHA_NONGIT_BRANCH");
            let expected_sha = std::env::var("AFD_HEAD_SHA_NONGIT_SHA")
                .expect("parent must set AFD_HEAD_SHA_NONGIT_SHA");
            let vcs = CliVcs::new("daemon-owner/daemon-repo".to_string());
            let result = vcs.head_sha_within_for_repo(&target_repo, &branch, 10);
            assert_eq!(
                result.unwrap_or_else(|e| panic!(
                    "head_sha_within_for_repo must succeed from a non-git daemon \
                     cwd (this is exactly the mw85 regression if it fails): {e:?}"
                )),
                expected_sha
            );
            return;
        }

        // Parent mode: build the fixtures, spawn the child, assert it passed.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();

        // A freshly created, empty directory -- not inside any git work tree
        // by construction (a brand-new subdirectory of the OS temp dir).
        let nongit_dir =
            std::env::temp_dir().join(format!("afd_head_sha_nongit_cwd_{pid}_{nanos}"));
        let _ = std::fs::remove_dir_all(&nongit_dir);
        std::fs::create_dir_all(&nongit_dir).unwrap();

        // Verify non-git FIRST -- fail loudly (not silently false-pass) if
        // the fixture directory somehow ended up inside a git work tree,
        // since that would make the rest of this test prove nothing.
        let probe = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&nongit_dir)
            .output()
            .expect("git must be on PATH to run the non-git precondition check");
        assert!(
            !probe.status.success(),
            "precondition failed: {} is inside a git work tree (git rev-parse \
             --show-toplevel unexpectedly succeeded: {}); this test cannot \
             prove anything about non-git cwds until the fixture directory is \
             genuinely outside any work tree",
            nongit_dir.display(),
            String::from_utf8_lossy(&probe.stdout)
        );

        // Fake `gh` shim, same shape `with_fake_gh` sets up, so the child can
        // resolve `gh api repos/<repo>/git/ref/heads/<branch>` without a real
        // network/auth dependency.
        let fake_gh_dir = make_fake_gh_dir("head_sha_nongit_cwd");
        let bin = fake_gh_dir.join("bin");

        let target_repo = "other-owner/other-repo";
        let branch = "factory/mw85-nongit-r1";
        let expected_sha = "abcdef0123456789abcdef0123456789abcdef01";

        let mut child_path = std::ffi::OsString::from(bin.to_str().unwrap());
        if let Some(prior) = std::env::var_os("PATH") {
            child_path.push(":");
            child_path.push(prior);
        }

        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(&exe)
            .args([
                "--exact",
                "adapters::cli_vcs_gh_tests::head_sha_within_for_repo_survives_non_git_daemon_cwd",
                "--nocapture",
            ])
            .current_dir(&nongit_dir)
            .env("AFD_HEAD_SHA_NONGIT_HELPER", "1")
            .env("AFD_HEAD_SHA_NONGIT_REPO", target_repo)
            .env("AFD_HEAD_SHA_NONGIT_BRANCH", branch)
            .env("AFD_HEAD_SHA_NONGIT_SHA", expected_sha)
            .env("PATH", &child_path)
            .env("GH_TEST_TARGET_REPO", target_repo)
            .env("GH_TEST_SHA", expected_sha)
            .status()
            .expect("failed to spawn child test process");

        std::fs::remove_dir_all(&fake_gh_dir).ok();
        std::fs::remove_dir_all(&nongit_dir).ok();

        assert!(
            status.success(),
            "head_sha_within_for_repo regressed: it failed when invoked from a \
             genuinely non-git daemon cwd (see the child test's own output \
             above -- likely a 'not a git repository' error from a \
             reintroduced cwd-bound `git` call)"
        );
    }
}

// jleechan-agy-vendor-name-drift-9lvs (operator guidance, r2): the original
// skeptic P1 from PR #321's head 7ade4ef2 was that
// adapters.rs::verify_ao_bridge_compatibility skipped
// validate_configured_vendors when `agentPlugins` was empty/missing/malformed
// and the bridge deliberately emitted `[]` on registry errors, so startup
// passed without proving resolved agents exist. r2 closes all three paths.
#[cfg(test)]
mod vendor_drift_preflight_r2_tests {
    use super::{canonical_for_alias, validate_configured_vendors};

    // jleechan-9lvs red proof #1: legacy `agy` must alias onto the renamed
    // `antigravity` plugin name (AO main 2026-07-18 rename).
    #[test]
    fn legacy_agy_alias_resolves_to_canonical_antigravity_plugin() {
        assert_eq!(canonical_for_alias("agy"), Some("antigravity"));
    }

    // Pre-existing legacy alias must still resolve.
    #[test]
    fn legacy_aow_alias_resolves_to_canonical_minimax_plugin() {
        assert_eq!(canonical_for_alias("aow"), Some("minimax"));
        assert_eq!(canonical_for_alias("claudem"), Some("minimax"));
    }

    // Non-legacy vendor names pass through unchanged so the bridge sees the
    // exact plugin name the registry has.
    #[test]
    fn unknown_vendor_name_returns_no_alias() {
        assert_eq!(canonical_for_alias("claude-code"), None);
        assert_eq!(canonical_for_alias("antigravity"), None);
        assert_eq!(canonical_for_alias(""), None);
    }

    // jleechan-9lvs acceptance: valid list with the agy alias resolves and
    // the daemon accepts it as installed (the alias IS the configuration).
    #[test]
    fn valid_list_with_legacy_agy_alias_passes_preflight() {
        let installed = vec!["antigravity".to_string(), "claude-code".to_string()];
        let configured = vec!["agy".to_string(), "claude-code".to_string()];
        assert!(validate_configured_vendors(Ok(&installed), &configured).is_ok());
    }

    // jleechan-9lvs r2 P1: genuine empty registry is a HARD failure. A
    // factory with zero coder plugins cannot dispatch, so passing the
    // preflight and only failing on the first bead is the exact bug class
    // this PR is closing.
    #[test]
    fn empty_installed_plugin_list_fails_preflight_loud() {
        let installed: Vec<String> = Vec::new();
        let configured = vec!["minimax".to_string()];
        let error = validate_configured_vendors(Ok(&installed), &configured)
            .expect_err("empty registry must fail");
        let message = error.to_string();
        assert!(
            message.contains("zero installed agent plugins"),
            "empty-list error must name the registry state, got: {message}"
        );
        assert!(
            message.contains("cannot dispatch"),
            "empty-list error must call out that the factory cannot dispatch, got: {message}"
        );
    }

    // jleechan-9lvs r2 P1: a registry error (list reachable but threw) is a
    // distinct state from a genuinely-empty list. The error message must
    // surface the underlying exception so the operator knows to fix the
    // AO install (or restart AO), not the daemon config.
    #[test]
    fn registry_error_fails_preflight_with_distinct_message() {
        let configured = vec!["minimax".to_string()];
        let error = validate_configured_vendors(
            Err("TypeError: registry.list is not a function"),
            &configured,
        )
        .expect_err("registry error must fail");
        let message = error.to_string();
        assert!(
            message.contains("registry error"),
            "registry-error path must call out registry error, got: {message}"
        );
        assert!(
            message.contains("TypeError"),
            "registry-error path must propagate the underlying exception, got: {message}"
        );
        assert!(
            !message.contains("zero installed agent plugins"),
            "registry-error message must NOT be confused with empty-list, got: {message}"
        );
    }

    // When the registry reports a non-empty list but the configured vendor
    // (after alias) is missing, every missing name is reported in one error
    // so a single fix covers every lane (jleechan-r56m aggregation property).
    #[test]
    fn missing_canonical_vendor_is_reported_alongside_installed_set() {
        let installed = vec!["antigravity".to_string()];
        let configured = vec!["agy".to_string(), "claude-code".to_string()];
        let error = validate_configured_vendors(Ok(&installed), &configured)
            .expect_err("missing claude-code must fail");
        let message = error.to_string();
        assert!(
            message.contains("claude-code"),
            "missing vendor must be named in the error, got: {message}"
        );
        assert!(
            message.contains("antigravity"),
            "installed set must be enumerated in the error, got: {message}"
        );
    }

    // The configured vendor chain (default + fallback) is deduped by
    // canonical form so a config that names both `agy` and `antigravity`
    // does not double-list a vendor that aliases to the same plugin.
    #[test]
    fn duplicate_canonical_vendors_are_deduplicated_before_validation() {
        // Both canonical targets must be installed for the dedup test —
        // the assertion is that `agy`/`antigravity` collapse into ONE
        // canonical entry, and `aow`/`minimax` collapse into ANOTHER, not
        // that an incomplete installed set passes validation.
        let installed = vec!["antigravity".to_string(), "minimax".to_string()];
        let configured = vec![
            "agy".to_string(),
            "antigravity".to_string(),
            "aow".to_string(),
            "minimax".to_string(),
        ];
        assert!(validate_configured_vendors(Ok(&installed), &configured).is_ok());
    }
}
