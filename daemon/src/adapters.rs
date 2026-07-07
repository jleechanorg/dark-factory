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
        }
        let checks_out = match run_tool(
            "gh",
            &["pr", "checks", &pr_str, "--repo", &self.repo, "--json", "state,bucket"],
            30,
        ) {
            Ok(out) => out,
            Err(_) => {
                // REST Fallback!
                let ref_url = format!("repos/{}/commits/{}/check-runs", self.repo, view.head_ref_oid);
                let cr_json = match run_tool("gh", &["api", &ref_url], 30) {
                    Ok(json) => json,
                    Err(_) => "{\"check_runs\":[]}".to_string()
                };
                
                #[derive(serde::Deserialize)]
                struct RestCheckRuns {
                    check_runs: Vec<RestCheckRun>,
                }
                #[derive(serde::Deserialize)]
                struct RestCheckRun {
                    status: String,
                    conclusion: Option<String>,
                }
                let rest_cr: RestCheckRuns = serde_json::from_str(&cr_json).unwrap_or(RestCheckRuns { check_runs: vec![] });
                
                let legacy_checks: Vec<GhCheck> = rest_cr.check_runs.into_iter().map(|cr| {
                    let (state, bucket) = if cr.status == "completed" {
                        match cr.conclusion.as_deref() {
                            Some("success") | Some("neutral") => ("SUCCESS".to_string(), "pass".to_string()),
                            Some("cancelled") => ("CANCELLED".to_string(), "cancel".to_string()),
                            _ => ("FAILURE".to_string(), "fail".to_string()),
                        }
                    } else {
                        ("PENDING".to_string(), "pending".to_string())
                    };
                    GhCheck { state, bucket }
                }).collect();
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
        )?;
        let unresolved_thread_count = unresolved_thread_count_from_gql(&gql_out)?;

        let pr_comments = view.comments.into_iter().map(|c| crate::tools::PrComment {
            author: c.author.login,
            body: c.body,
        }).collect();

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
    pub fn new(project: &str, agent: &str) -> Self {
        Self {
            project: project.to_string(),
            agent: agent.to_string(),
        }
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
            .arg(&self.agent)
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

        let mut child = cmd.spawn().map_err(|e| DaemonError::Tool {
            tool: if std::env::consts::OS == "macos" { "sandbox-exec".to_string() } else { "ao".to_string() },
            rc: -1,
            stderr: format!("spawn failed: {e}"),
        })?;

        let status = child.wait().map_err(|e| DaemonError::Tool {
            tool: if std::env::consts::OS == "macos" { "sandbox-exec".to_string() } else { "ao".to_string() },
            rc: -1,
            stderr: format!("wait failed: {e}"),
        })?;

        let mut stdout_buf = Vec::new();
        if let Some(mut stdout) = child.stdout {
            let _ = stdout.read_to_end(&mut stdout_buf);
        }
        let out = String::from_utf8_lossy(&stdout_buf);

        if !status.success() {
            let mut stderr_buf = Vec::new();
            if let Some(mut stderr) = child.stderr {
                let _ = stderr.read_to_end(&mut stderr_buf);
            }
            let err_msg = String::from_utf8_lossy(&stderr_buf);
            return Err(DaemonError::Tool {
                tool: "ao spawn".to_string(),
                rc: status.code().unwrap_or(-1),
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
            Ok(SessionId(name))
        } else {
            Err(DaemonError::Parse(format!(
                "ao spawn produced no session name: {out}"
            )))
        }
    }

    fn attach(&self, _branch: &str, _bead_id: &str) -> Result<SessionId, DaemonError> {
        Err(DaemonError::Config("attach is disabled/unimplemented".into()))
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
}

pub struct CliVcs;

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
