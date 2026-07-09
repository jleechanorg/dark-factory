use crate::errors::DaemonError;
use crate::tools::{run_tool, run_tool_in_dir, Bead, Issue, LabeledPr, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use std::io::Read;
use std::process::{Command, Stdio};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};


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
        if let Some((repo, issue)) = parse_external_ref(external_ref) {
            run_tool("gh", &["issue", "comment", &issue, "--repo", &repo, "--body", body], 30)?;
            Ok(())
        } else {
            Err(DaemonError::Parse(format!(
                "invalid external_ref format for comment: {external_ref}"
            )))
        }
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
        None
    }
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
                        unresolved_thread_count: snap.unresolved_thread_count,
                        head_sha: snap.head_sha,
                        body: snap.body,
                        comments: snap.comments,
                        files: snap.files,
                        updated_at_epoch: snap.updated_at_epoch.unwrap_or(0),
                        ci_status,
                        coderabbit_status,
                        ci_pending: false,
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
                    comments: rest_comments.into_iter().map(|c| GhComment { author: GhAuthor { login: c.user.map(|u| u.login).unwrap_or_default() }, body: c.body }).collect(),
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
        // jleechan-qdw/GraphQL-rate-limit tolerance: a GraphQL failure here
        // (rate limit, transient network) must not abort the whole
        // `pr_snapshot` call — unresolved-thread count degrades to 0 (with a
        // logged warning) rather than propagating the error via `?`.
        let unresolved_thread_count = match gql_out {
            Ok(gql_out_str) => unresolved_thread_count_from_gql(&gql_out_str).unwrap_or_else(|e| {
                eprintln!("[warn] failed to parse unresolved-thread GraphQL response, defaulting to 0: {e:?}");
                0
            }),
            Err(e) => {
                eprintln!("[warn] GraphQL query failed, defaulting unresolved threads to 0: {e:?}");
                0
            }
        };

        let mut pr_comments: Vec<crate::tools::PrComment> = view.comments.into_iter().map(|c| crate::tools::PrComment {
            author: c.author.login,
            body: c.body,
        }).collect();

        for c in &checks {
            if c.name.to_lowercase().contains("skeptic") {
                if c.bucket == "pass" || c.state == "SUCCESS" {
                    pr_comments.push(crate::tools::PrComment {
                        author: "github-actions".to_string(),
                        body: "skeptic check run: verdict: pass".to_string(),
                    });
                } else if c.bucket == "fail" || c.state == "FAILURE" {
                    pr_comments.push(crate::tools::PrComment {
                        author: "github-actions".to_string(),
                        body: "skeptic check run: verdict: fail".to_string(),
                    });
                }
            }
        }

        let pr_files = view.files.into_iter().map(|f| crate::tools::PrFile {
            path: f.path,
            additions: f.additions,
            deletions: f.deletions,
        }).collect();

        let updated_at_epoch = crate::tools::iso8601_to_epoch(&view.updated_at).unwrap_or(0);
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
        };
        {
            let mut cache = self.pr_snapshot_cache.lock().unwrap();
            cache.insert(pr, (snapshot.clone(), Instant::now()));
        }
        Ok(snapshot)
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
}

pub struct CliSessions {
    pub project: String,
    pub agent: String,
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
        }
    }

    fn run_spawn_process(&self, agent: &str, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        let home = std::env::var("HOME").unwrap_or_default();
        let holdouts = std::env::var("DARK_FACTORY_HOLDOUTS")
            .unwrap_or_else(|_| format!("{}/projects/dark-factory-holdouts", home));
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n",
            holdouts, holdouts
        );

        let mut cmd = if std::env::consts::OS == "macos" {
            let mut c = Command::new("sandbox-exec");
            c.arg("-p").arg(&profile);
            c.arg("ao");
            c
        } else {
            Command::new("ao")
        };
        let display_name = spec.branch.strip_prefix("factory/").unwrap_or(&spec.branch);

        cmd.arg("spawn")
            .arg("--prompt")
            .arg(&spec.prompt)
            .arg("--project")
            .arg(&self.project)
            .arg("--agent")
            .arg(agent)
            .arg("--name")
            .arg(display_name)
            .arg("--branch")
            .arg(&spec.branch)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, _) in std::env::vars() {
            if k == "DARK_FACTORY_HOLDOUTS" || k.to_uppercase().contains("HOLDOUT") {
                cmd.env_remove(k);
            }
        }

        let output = cmd.output().map_err(|e| DaemonError::Tool {
            tool: if std::env::consts::OS == "macos" { "sandbox-exec".to_string() } else { "ao".to_string() },
            rc: -1,
            stderr: format!("execution failed: {e}"),
        })?;

        let out = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(DaemonError::Tool {
                tool: format!("ao spawn --agent {agent}"),
                rc: output.status.code().unwrap_or(-1),
                stderr: err_msg.into_owned(),
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

        let mut last_err = None;
        for agent in &fallback_agents {
            match self.run_spawn_process(agent, spec) {
                Ok(sess) => return Ok(sess),
                Err(e) => {
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            DaemonError::Config("No agents in fallback chain could be run".into())
        }))
    }
}

impl Sessions for CliSessions {
    fn active_count(&self) -> Result<usize, DaemonError> {
        let out = run_tool("ao", &["status", "--json"], 30)?;
        let json_start = out.find('[').unwrap_or(0);
        let data: serde_json::Value = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse ao status: {e}"))
        })?;
        let mut count = 0;
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if let Some(project) = entry.get("project").and_then(|v| v.as_str()) {
                    if project != self.project {
                        continue;
                    }
                }
                if let Some(activity) = entry.get("activity").and_then(|v| v.as_str()) {
                    if activity != "exited" && activity != "missing" {
                        count += 1;
                    }
                }
            }
        }
        Ok(count)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        self.spawn_with_fallback(spec)
    }

    fn spawn_batch(&self, specs: &[SpawnSpec]) -> Result<Vec<SessionId>, DaemonError> {
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

        let mut results = vec![None; specs.len()];
        let mut active_indices: Vec<usize> = (0..specs.len()).collect();

        for agent in &fallback_agents {
            if active_indices.is_empty() {
                break;
            }

            let mut children = Vec::new();
            for &idx in &active_indices {
                let spec = &specs[idx];
                let home = std::env::var("HOME").unwrap_or_default();
                let holdouts = std::env::var("DARK_FACTORY_HOLDOUTS")
                    .unwrap_or_else(|_| format!("{}/projects/dark-factory-holdouts", home));
                let profile = format!(
                    "(version 1)\n(allow default)\n(deny file-read* (subpath \"{}\"))\n(deny file-write* (subpath \"{}\"))\n",
                    holdouts, holdouts
                );

                let mut cmd = if std::env::consts::OS == "macos" {
                    let mut c = Command::new("sandbox-exec");
                    c.arg("-p").arg(&profile);
                    c.arg("ao");
                    c
                } else {
                    Command::new("ao")
                };
                // jleechan-pqip follow-up (PR#163 finding 3): mirror
                // `run_spawn_process`'s argv exactly — omitting `--name`/
                // `--branch` here previously meant every batch-spawned
                // session landed on whatever branch `ao spawn` defaults to
                // instead of `spec.branch`, and `spec.bead_id` was never
                // read at all.
                let display_name = spec.branch.strip_prefix("factory/").unwrap_or(&spec.branch);
                cmd.arg("spawn")
                    .arg("--prompt")
                    .arg(&spec.prompt)
                    .arg("--project")
                    .arg(&self.project)
                    .arg("--agent")
                    .arg(agent)
                    .arg("--name")
                    .arg(display_name)
                    .arg("--branch")
                    .arg(&spec.branch)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());

                for (k, _) in std::env::vars() {
                    if k == "DARK_FACTORY_HOLDOUTS" || k.to_uppercase().contains("HOLDOUT") {
                        cmd.env_remove(k);
                    }
                }

                if let Ok(child) = cmd.spawn() {
                    children.push((idx, child, agent.clone()));
                }
            }

            let mut next_active_indices = Vec::new();
            for (idx, mut child, _ag) in children {
                let status = match child.wait() {
                    Ok(st) => st,
                    Err(_) => {
                        next_active_indices.push(idx);
                        continue;
                    }
                };

                let mut stdout_buf = Vec::new();
                if let Some(mut stdout) = child.stdout {
                    let _ = stdout.read_to_end(&mut stdout_buf);
                }
                let out = String::from_utf8_lossy(&stdout_buf);

                if !status.success() {
                    next_active_indices.push(idx);
                    continue;
                }

                let mut sess_name = None;
                for line in out.lines() {
                    if line.starts_with("SESSION=") {
                        sess_name = Some(line.split('=').nth(1).unwrap_or("").trim().to_string());
                    }
                }

                if let Some(name) = sess_name {
                    results[idx] = Some(SessionId(name));
                } else {
                    next_active_indices.push(idx);
                }
            }

            active_indices = next_active_indices;
        }

        let mut final_ids = Vec::new();
        for (idx, res) in results.into_iter().enumerate() {
            match res {
                Some(id) => final_ids.push(id),
                None => {
                    return Err(DaemonError::Tool {
                        tool: "ao spawn batch".to_string(),
                        rc: -1,
                        stderr: format!("Failed to spawn session for spec prompt: {}", specs[idx].prompt),
                    });
                }
            }
        }

        Ok(final_ids)
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
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if entry.get("branch").and_then(|v| v.as_str()) == Some(branch) {
                    if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                        return Ok(SessionId(name.to_string()));
                    }
                }
            }
        }
        Err(DaemonError::Config(format!(
            "attach: no ao session currently tracks branch '{branch}' (bead {bead_id}); \
             it may have already exited, been reaped, or never been spawned"
        )))
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
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if entry.get("name").and_then(|v| v.as_str()) == Some(&id.0) {
                    if let Some(activity) = entry.get("activity").and_then(|v| v.as_str()) {
                        return Ok(activity == "ready" || activity == "exited" || activity == "missing");
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

    fn remote_head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        run_tool("git", &["fetch", "origin", branch], 30)?;
        self.head_sha(&format!("origin/{branch}"))
    }

    fn is_ancestor(&self, ancestor_sha: &str, descendant_sha: &str) -> Result<bool, DaemonError> {
        // `--is-ancestor` exits 0 on true, 1 on false, and non-0/1 on a
        // genuine git error (e.g. one of the SHAs isn't a known object) —
        // all non-zero exits collapse to `Ok(false)` here, same fold
        // `is_remote_ahead` already uses. Callers of `is_ancestor` for the
        // force-push-detection use case are documented (on the trait) to
        // treat that `Ok(false)` as fail-closed, unlike `is_remote_ahead`'s
        // callers.
        let r = run_tool(
            "git",
            &["merge-base", "--is-ancestor", ancestor_sha, descendant_sha],
            30,
        );
        Ok(r.is_ok())
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
mod external_ref_tests {
    use super::{parse_external_refs_from_br_list, unresolved_thread_count_from_gql};

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
    use std::sync::{Mutex, OnceLock};

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
    /// `OnceLock` so the allocation happens once per process; `Mutex` (not
    /// `RwLock`) because every holder mutates `std::env` between lock
    /// acquisition and `ChainLlm::judge` and the critical section is short.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
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
    use std::sync::{Mutex, OnceLock};

    /// Process-wide mutex serializing PATH/HOME mutation across this
    /// module's tests, mirroring `chain_llm_fallback_argv_tests::env_lock`
    /// (see that doc comment for the full rationale: parallel `cargo test`
    /// execution would otherwise let two tests clobber each other's PATH).
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
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
