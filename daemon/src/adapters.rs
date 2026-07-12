use crate::errors::{DaemonError, SpawnBatchCleanupFailure};
use crate::tools::{run_tool, run_tool_in_dir, Bead, Issue, LabeledPr, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
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

    fn bead_status(&self, bead_id: &str) -> Result<Option<crate::tools::BeadStatus>, DaemonError> {
        let out = match run_tool("br", &["show", bead_id, "--json"], 30) {
            Ok(o) => o,
            Err(e) => {
                return if is_bead_missing_error(&e) {
                    Ok(None)
                } else {
                    Err(e)
                };
            }
        };
        let trimmed = out.trim();
        if trimmed.is_empty() {
            return Err(DaemonError::Parse(format!(
                "br show exit-0 empty stdout for {bead_id}; \
                 expected JSON object or array — empty output is not proof \
                 the bead is missing (only rc=3 with ISSUE_NOT_FOUND \
                 retryable=false means the bead is missing)"
            )));
        }
        let status: String = if let Some(arr_start) = trimmed.find('[') {
            if arr_start == 0 || trimmed[..arr_start].trim().is_empty() {
                let arr: Vec<serde_json::Value> =
                    serde_json::from_str(&trimmed[arr_start..]).map_err(|e| {
                        DaemonError::Parse(format!(
                            "failed to parse br show array JSON for {bead_id}: {e}"
                        ))
                    })?;
                if arr.is_empty() {
                    return Err(DaemonError::Parse(format!(
                        "br show exit-0 empty JSON array for {bead_id}; \
                         expected at least one object with a status field — \
                         empty array is not proof the bead is missing (only \
                         rc=3 with ISSUE_NOT_FOUND retryable=false means \
                         the bead is missing)"
                    )));
                }
                arr[0]
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| {
                        DaemonError::Parse(format!(
                            "br show array missing status field for {bead_id}"
                        ))
                    })?
            } else {
                return Err(DaemonError::Parse(format!(
                    "unexpected br show output for {bead_id}: {trimmed}"
                )));
            }
        } else if let Some(json_start) = trimmed.find('{') {
            #[derive(serde::Deserialize)]
            struct BrShow {
                status: String,
            }
            let data: BrShow =
                serde_json::from_str(&trimmed[json_start..]).map_err(|e| {
                    DaemonError::Parse(format!("failed to parse br show JSON for {bead_id}: {e}"))
                })?;
            data.status
        } else {
            return Err(DaemonError::Parse(format!(
                "no JSON object or array in br show output for {bead_id}: {out}"
            )));
        };
        Ok(Some(classify_bead_status(
            bead_id,
            status.to_ascii_lowercase().as_str(),
        )?))
    }
}

fn is_bead_missing_error(err: &DaemonError) -> bool {
    match err {
        DaemonError::Tool { rc, stderr, .. } if *rc == 3 => {
            let json_start = match stderr.find('{') {
                Some(pos) => pos,
                None => return false,
            };
            let val: serde_json::Value = match serde_json::from_str(&stderr[json_start..]) {
                Ok(v) => v,
                Err(_) => return false,
            };
            val.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                == Some("ISSUE_NOT_FOUND")
                && val
                    .get("error")
                    .and_then(|e| e.get("retryable"))
                    .and_then(|r| r.as_bool())
                    == Some(false)
        }
        _ => false,
    }
}

fn classify_bead_status(
    bead_id: &str,
    status: &str,
) -> Result<crate::tools::BeadStatus, DaemonError> {
    match status {
        "open" => Ok(crate::tools::BeadStatus::Open),
        "in_progress" | "blocked" | "deferred" | "active" | "reopened" | "awaiting_review"
        | "review" => Ok(crate::tools::BeadStatus::Active),
        "closed" | "completed" | "done" | "resolved" | "merged" => {
            Ok(crate::tools::BeadStatus::Closed)
        }
        other => Err(DaemonError::Parse(format!(
            "unknown bead status '{other}' for {bead_id}; \
             expected open / in_progress / blocked / deferred / closed / completed / done or a known \
             active or terminal status"
        ))),
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

    fn labeled_prs_via_rest(&self, label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
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
        #[derive(serde::Deserialize)]
        struct RestPull {
            head: RestHead,
        }
        #[derive(serde::Deserialize)]
        struct RestHead {
            #[serde(rename = "ref")]
            ref_name: String,
            repo: Option<RestRepo>,
        }
        #[derive(serde::Deserialize)]
        struct RestRepo {
            full_name: Option<String>,
            owner: Option<RestUser>,
        }

        let json_start = out.find('[').unwrap_or(0);
        let issues: Vec<RestIssue> = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh labeled PR REST list: {e}"))
        })?;
        let mut prs = Vec::new();
        let target_owner = self.repo.split('/').next().unwrap_or_default();
        for issue in issues.into_iter().filter(|issue| issue.pull_request.is_some()) {
            let pull_out = run_tool(
                "gh",
                &[
                    "api",
                    &format!("repos/{}/pulls/{}", self.repo, issue.number),
                ],
                30,
            )?;
            let json_start = pull_out.find('{').unwrap_or(0);
            let pull: RestPull = serde_json::from_str(&pull_out[json_start..]).map_err(|e| {
                DaemonError::Parse(format!(
                    "failed to parse gh pull REST response for PR #{}: {e}",
                    issue.number
                ))
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
            let is_cross_repository = head_repo_full_name
                .as_ref()
                .map(|repo| !repo.eq_ignore_ascii_case(&self.repo))
                .or_else(|| {
                    head_repo_owner_login
                        .as_ref()
                        .map(|owner| !owner.eq_ignore_ascii_case(target_owner))
                })
                .unwrap_or(false);
            prs.push(LabeledPr {
                number: issue.number,
                title: issue.title,
                body: issue.body.unwrap_or_default(),
                author_login: issue.user.map(|u| u.login).unwrap_or_default(),
                external_ref: format!("{}#{}", self.repo, issue.number),
                head_ref_name: pull.head.ref_name,
                is_cross_repository,
                head_repo_full_name,
                head_repo_owner_login,
            });
        }
        Ok(prs)
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

    fn labeled_prs(&self, label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
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
                "number,title,body,author,headRefName,isCrossRepository,headRepositoryOwner",
            ],
            30,
        ) {
            Ok(out) => out,
            Err(_) => return self.labeled_prs_via_rest(label),
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
            .map(|pr| LabeledPr {
                number: pr.number,
                title: pr.title,
                body: pr.body.unwrap_or_default(),
                author_login: pr.author.map(|a| a.login).unwrap_or_default(),
                external_ref: format!("{}#{}", self.repo, pr.number),
                head_ref_name: pr.head_ref_name,
                is_cross_repository: pr.is_cross_repository,
                head_repo_full_name: None,
                head_repo_owner_login: pr.head_repository_owner.map(|owner| owner.login),
            })
            .collect())
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
        let mut any_pending = false;
        let mut any_failed = false;
        for c in &checks {
            if c.bucket == "pending" {
                any_pending = true;
            } else if c.bucket == "fail" || c.bucket == "cancel" {
                any_failed = true;
            }
        }
        let ci_status = if checks.is_empty() || any_pending {
            "unknown".to_string()
        } else if any_failed {
            "red".to_string()
        } else {
            "green".to_string()
        };

        let iteration_stub =
            std::env::var("DARK_FACTORY_ITERATION_STUB").as_deref() == Ok("1");
        let ci_success = ci_success_from_check_buckets(
            &checks.iter().map(|c| c.bucket.as_str()).collect::<Vec<_>>(),
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

/// Runs the AO v0.1.3 preload in its read-only diagnostic mode. This checks
/// the actual `ao` executable selected by the daemon's production PATH, its
/// Node major version, package version, core APIs, configured project, and
/// plugin resolution without performing preflight side effects, acquiring a
/// spawn lock, creating a workspace, or launching a worker.
pub fn verify_ao_bridge_compatibility(
    ao_project: &str,
    agent: &str,
) -> Result<(), DaemonError> {
    let spec = SpawnSpec {
        bead_id: "daemon-startup-diagnostic".to_string(),
        branch: "factory/daemon-startup-diagnostic".to_string(),
        prompt: "dark-factory AO v0.1.3 read-only compatibility diagnostic".to_string(),
        repo: String::new(),
        ao_project: ao_project.to_string(),
        remote: String::new(),
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
    Ok(())
}

pub struct CliSessions {
    pub project: String,
    pub agent: String,
    spawned_worktrees:
        std::sync::Mutex<std::collections::HashMap<(String, String), std::path::PathBuf>>,
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
        let spawn_error = if workspace.is_none() {
            Some(DaemonError::Parse(format!(
                "ao spawn --agent {agent} returned session {} without an absolute Worktree path; refusing to dispatch without remote verification",
                session.0
            )))
        } else if observed_branch.as_deref() != Some(spec.branch.as_str()) {
            Some(DaemonError::Parse(format!(
                "ao spawn --agent {agent} returned session {} with branch {:?}, expected {:?}; refusing to dispatch a branch-mismatched worker",
                session.0, observed_branch, spec.branch
            )))
        } else {
            None
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
        let workspace = workspace.expect("validated absolute workspace");
        self.spawned_worktrees
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (spec.ao_project.clone(), spec.branch.clone()),
                workspace,
            );
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
            .unwrap_or_else(|_| "aow->claude-code->agy->minimax".to_string());
        
        let mut fallback_agents = Vec::new();
        fallback_agents.push(self.agent.clone());
        for part in fallback_str.split("->") {
            let part_trimmed = part.trim().to_string();
            let mapped_agent = if part_trimmed == "aow" {
                "minimax".to_string()
            } else {
                part_trimmed
            };
            if !mapped_agent.is_empty() && !fallback_agents.contains(&mapped_agent) {
                fallback_agents.push(mapped_agent);
            }
        }

        fallback_spawn(&fallback_agents, |agent| self.run_spawn_process(agent, spec))
    }
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
    use super::fallback_spawn;
    use crate::errors::DaemonError;

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
    handle.write(json.dumps({"kind": "spawn", "args": args, "branch": os.environ["DARK_FACTORY_AO_SPAWN_BRANCH"]}) + "\n")
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

    #[test]
    fn single_spawn_uses_v013_positional_prompt_and_exact_branch_binding() {
        let prompt = "single prompt with spaces\nand a second line";
        let branch = "factory/jleechan-contract-single-r1";
        let bindings = serde_json::json!({ prompt: branch });

        let (spawn_result, calls) = with_fake_ao("single", bindings, |log| {
            let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
            let result = sessions.spawn(&spec(prompt, branch));
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
        let out = run_tool("ao", &["status", "--json"], 30)?;
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

    /// jleechan-xsg4 (issue #270): authoritative session liveness for stale
    /// DISPATCHED recovery. Same `ao status --json` parsing shape as
    /// `is_quiescent`, but ONLY "exited" or "missing" is considered dead.
    /// A session in "ready" state is alive — the coder is working —
    /// and must NOT be requeued. Transient errors propagate so the caller
    /// can skip-recover (a single probe failure must never falsely requeue
    /// a live session).
    fn is_session_dead(&self, id: &SessionId) -> Result<bool, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], 30)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if entry.get("name").and_then(|v| v.as_str()) == Some(&id.0) {
                    if let Some(activity) = entry.get("activity").and_then(|v| v.as_str()) {
                        return Ok(activity == "exited" || activity == "missing");
                    }
                }
            }
        }
        Ok(true)
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
    use super::{active_session_count, session_for_branch, session_is_quiescent};
    use crate::tools::SessionId;

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

    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError> {
        run_tool("git", &["branch", name, sha], 30)?;
        Ok(())
    }

    fn head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        let out = run_tool("git", &["rev-parse", branch], 30)?;
        Ok(out.trim().to_string())
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

#[cfg(test)]
mod bead_status_tests {
    use super::{classify_bead_status, is_bead_missing_error};
    use crate::errors::DaemonError;
    use crate::tools::BeadStatus;

    #[test]
    fn classify_open_returns_open() {
        match classify_bead_status("b1", "open").unwrap() {
            BeadStatus::Open => {}
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn classify_in_progress_returns_active() {
        match classify_bead_status("b1", "in_progress").unwrap() {
            BeadStatus::Active => {}
            other => panic!("expected Active, got {other:?}"),
        }
    }

    #[test]
    fn classify_closed_returns_closed() {
        match classify_bead_status("b1", "closed").unwrap() {
            BeadStatus::Closed => {}
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn classify_completed_returns_closed() {
        match classify_bead_status("b1", "completed").unwrap() {
            BeadStatus::Closed => {}
            other => panic!("expected Closed, got {other:?}"),
        }
    }

    #[test]
    fn classify_unknown_status_is_parse_error() {
        let r = classify_bead_status("b1", "bogus");
        assert!(matches!(r, Err(DaemonError::Parse(_))));
    }

    #[test]
    fn classify_is_case_sensitive_requiring_pre_lowercased_input() {
        assert_eq!(classify_bead_status("b1", "open").unwrap(), BeadStatus::Open);
        assert_eq!(classify_bead_status("b1", "closed").unwrap(), BeadStatus::Closed);
        assert!(matches!(
            classify_bead_status("b1", "OPEN"),
            Err(DaemonError::Parse(_))
        ));
    }

    #[test]
    fn is_bead_missing_rejects_not_found_tool_error() {
        let err = DaemonError::Tool {
            tool: "br".into(),
            rc: 1,
            stderr: "Error: bead not found in database".into(),
        };
        assert!(!is_bead_missing_error(&err));
    }

    #[test]
    fn is_bead_missing_rejects_no_such_tool_error() {
        let err = DaemonError::Tool {
            tool: "br".into(),
            rc: 1,
            stderr: "no such bead".into(),
        };
        assert!(!is_bead_missing_error(&err));
    }

    #[test]
    fn is_bead_missing_rejects_transient_error() {
        let err = DaemonError::Tool {
            tool: "br".into(),
            rc: 1,
            stderr: "connection refused".into(),
        };
        assert!(!is_bead_missing_error(&err));
    }

    #[test]
    fn is_bead_missing_rejects_config_error() {
        let err = DaemonError::Config("bad config".into());
        assert!(!is_bead_missing_error(&err));
    }

    #[test]
    fn is_bead_missing_rejects_success_exit() {
        let err = DaemonError::Tool {
            tool: "br".into(),
            rc: 0,
            stderr: "no error".into(),
        };
        assert!(!is_bead_missing_error(&err));
    }

    #[test]
    fn is_bead_missing_rejects_parse_error() {
        let err = DaemonError::Parse("parse error".into());
        assert!(!is_bead_missing_error(&err));
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
}

/// Production `CliTracker` tests with a temp `br` on PATH (no mocks — real
/// subprocess calls driven through a shell shim). Each test holds
/// `gh_env_test_lock()` so PATH mutations are serialized across all
/// PATH-mutating test modules in this file (jleechan-9sl1 discipline).
#[cfg(test)]
mod cli_tracker_tests {
    use super::CliTracker;
    use crate::errors::DaemonError;
    use crate::tools::Tracker;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        super::gh_env_test_lock()
    }

    fn make_temp_dir(prefix: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "afd_cli_tracker_{prefix}_{nanos}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_br_shim(dir: &std::path::Path, script: &str) {
        let br_path = dir.join("br");
        std::fs::write(&br_path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(
            &br_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }

    struct PathGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl PathGuard {
        fn set(dir: &std::path::Path) -> Self {
            let prior = std::env::var_os("PATH");
            unsafe { std::env::set_var("PATH", dir) };
            PathGuard { prior }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(p) => std::env::set_var("PATH", p),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
    }

    // ── is_bead_missing_error classification ──────────────────────────

    #[test]
    #[cfg(unix)]
    fn bead_status_missing_rc3_issue_not_found_retryable_false() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_missing");
        write_br_shim(
            &dir,
            r#"printf '{"error":{"code":"ISSUE_NOT_FOUND","retryable":false,"message":"bead X not found"}}\n' >&2
exit 3"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("missing-bead");

        match result {
            Ok(None) => {}
            other => panic!("expected Ok(None) for missing bead, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc3_issue_not_found_retryable_true_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_retryable_true");
        write_br_shim(
            &dir,
            r#"printf '{"error":{"code":"ISSUE_NOT_FOUND","retryable":true,"message":"transient"}}\n' >&2
exit 3"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("retryable-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for retryable=true, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc3_wrong_error_code_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_wrong_code");
        write_br_shim(
            &dir,
            r#"printf '{"error":{"code":"DATABASE_ERROR","retryable":true}}\n' >&2
exit 3"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("wrong-code-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for wrong error.code, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc1_not_found_text_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_not_found");
        write_br_shim(
            &dir,
            r#"echo 'not found' >&2
exit 1"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("not-found-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for rc=1 not-found text, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc3_non_json_stderr_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_non_json");
        write_br_shim(
            &dir,
            r#"echo 'no such bead' >&2
exit 3"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("non-json-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for rc=3 non-JSON stderr, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc0_not_found_text_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_rc0_not_found");
        write_br_shim(
            &dir,
            r#"echo 'not found in database' >&2
exit 0"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("rc0-not-found");

        match result {
            Err(DaemonError::Parse(msg)) => {
                assert!(
                    msg.contains("empty stdout"),
                    "expected 'empty stdout' in Parse error, got: {msg}"
                );
            }
            other => panic!(
                "expected Err(Parse) for exit-0 empty stdout, got {other:?}"
            ),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc0_empty_stdout_is_parse_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_rc0_empty_out");
        write_br_shim(&dir, "exit 0");
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("rc0-empty-out");

        match result {
            Err(DaemonError::Parse(msg)) => {
                assert!(
                    msg.contains("empty stdout"),
                    "expected 'empty stdout' in Parse error, got: {msg}"
                );
            }
            other => panic!(
                "expected Err(Parse) for exit-0 empty stdout, got {other:?}"
            ),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc0_empty_json_array_is_parse_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_rc0_empty_arr");
        write_br_shim(
            &dir,
            r#"printf '[]\n'"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("rc0-empty-arr");

        match result {
            Err(DaemonError::Parse(msg)) => {
                assert!(
                    msg.contains("empty JSON array"),
                    "expected 'empty JSON array' in Parse error, got: {msg}"
                );
            }
            other => panic!(
                "expected Err(Parse) for exit-0 empty JSON array, got {other:?}"
            ),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc3_empty_stderr_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_empty_stderr");
        write_br_shim(&dir, "exit 3");
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("empty-stderr-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for rc=3 with empty stderr, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn bead_status_rc3_malformed_json_is_error_not_missing() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_malformed");
        write_br_shim(
            &dir,
            r#"printf '{"malformed": true' >&2
exit 3"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.bead_status("malformed-json-bead");

        match result {
            Err(DaemonError::Tool { .. }) => {}
            other => panic!("expected Err(Tool) for rc=3 malformed JSON, got {other:?}"),
        }
    }

    // ── fetch_candidates via production CliTracker ────────────────────

    #[test]
    #[cfg(unix)]
    fn fetch_candidates_parses_valid_br_list_json() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_list");
        write_br_shim(
            &dir,
            r#"printf '{"issues":[{"id":"b1","title":"t1","description":"d1","external_ref":"owner/repo#1"}],"has_more":false}\n'"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.fetch_candidates();

        match result {
            Ok(beads) => {
                assert_eq!(beads.len(), 1);
                assert_eq!(beads[0].id, "b1");
                assert_eq!(beads[0].title, "t1");
            }
            other => panic!("expected Ok with 1 bead, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn fetch_candidates_rejects_truncated_output() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = make_temp_dir("br_list_truncated");
        write_br_shim(
            &dir,
            r#"printf '{"issues":[],"has_more":true}\n'"#,
        );
        let _path_guard = PathGuard::set(&dir);

        let tracker = CliTracker;
        let result = tracker.fetch_candidates();

        match result {
            Err(DaemonError::Parse(msg)) => {
                assert!(msg.contains("has_more"), "expected has_more error, got: {msg}");
            }
            other => panic!("expected Err(Parse) for truncated output, got {other:?}"),
        }
    }
}
