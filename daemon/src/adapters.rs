use crate::errors::DaemonError;
use crate::tools::{Bead, Issue, Llm, Permission, PrSnapshot, run_tool, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use std::io::Read;
use std::process::{Command, Stdio};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};


pub struct CliTracker;

impl Tracker for CliTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        let out = run_tool("br", &["list", "--status", "open", "--label", "factory", "--json"], 30)?;
        let json_start = out.find('{').unwrap_or(0);
        #[derive(serde::Deserialize)]
        struct BrListOutput {
            issues: Vec<BrIssue>,
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

fn parse_external_ref(external_ref: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
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
        let out_issues = run_tool(
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
                "--json",
                "number,title,body,author",
            ],
            30,
        )?;
        let out_prs = run_tool(
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
                "--json",
                "number,title,body,author",
            ],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct GhIssue {
            number: u64,
            title: String,
            body: Option<String>,
            author: GhAuthor,
        }
        #[derive(serde::Deserialize)]
        struct GhAuthor {
            login: String,
        }
        let json_start_issues = out_issues.find('[').unwrap_or(0);
        let gh_issues: Vec<GhIssue> = serde_json::from_str(&out_issues[json_start_issues..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh issue list: {e}"))
        })?;
        let json_start_prs = out_prs.find('[').unwrap_or(0);
        let gh_prs: Vec<GhIssue> = serde_json::from_str(&out_prs[json_start_prs..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr list: {e}"))
        })?;
        let mut issues: Vec<Issue> = Vec::new();
        for item in gh_issues.into_iter().chain(gh_prs.into_iter()) {
            if !issues.iter().any(|i| i.number == item.number) {
                issues.push(Issue {
                    number: item.number,
                    title: item.title,
                    body: item.body.unwrap_or_default(),
                    author_login: item.author.login,
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
        let view_out = run_tool(
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
        )?;
        #[derive(serde::Deserialize)]
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
        #[derive(serde::Deserialize)]
        struct GhReview {
            author: GhAuthor,
            state: String,
        }
        #[derive(serde::Deserialize)]
        struct GhComment {
            author: GhAuthor,
            body: String,
        }
        #[derive(serde::Deserialize)]
        struct GhAuthor {
            login: String,
        }
        #[derive(serde::Deserialize)]
        struct GhFile {
            path: String,
            additions: u32,
            deletions: u32,
        }
        let json_start = view_out.find('{').unwrap_or(0);
        let view: GhPrView = serde_json::from_str(&view_out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr view JSON: {e}"))
        })?;

        let mergeable = view.mergeable == "MERGEABLE";
        let mut coderabbit_approved = false;
        for r in &view.reviews {
            if r.author.login.contains("coderabbit") {
                coderabbit_approved = r.state == "APPROVED";
            }
        }

        let checks_out = run_tool(
            "gh",
            &["pr", "checks", &pr_str, "--repo", &self.repo, "--json", "state,bucket"],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct GhCheck {
            state: String,
            bucket: String,
        }
        let json_start_c = checks_out.find('[').unwrap_or(0);
        let checks: Vec<GhCheck> = serde_json::from_str(&checks_out[json_start_c..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr checks JSON: {e}"))
        })?;
        let ci_success = !checks.is_empty() && checks.iter().all(|c| {
            c.bucket == "pass" || c.bucket == "skipping"
        });

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
        let json_start_g = gql_out.find('{').unwrap_or(0);
        let gql: GhGqlResponse = serde_json::from_str(&gql_out[json_start_g..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh graphql JSON: {e}"))
        })?;
        let mut unresolved_thread_count = 0;
        if let Some(pr_data) = gql.data.repository.pull_request {
            unresolved_thread_count = pr_data.review_threads.nodes.iter().filter(|n| !n.is_resolved).count() as u32;
        }

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
        let project = repo.split('/').last().unwrap_or(repo).to_string();
        Self {
            project,
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

        let mut cmd = Command::new("sandbox-exec");
        cmd.arg("-p").arg(&profile);
        cmd.arg("ao")
            .arg("spawn")
            .arg(&spec.prompt)
            .arg("--project")
            .arg(&self.project)
            .arg("--agent")
            .arg(&self.agent)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, _) in std::env::vars() {
            if k == "DARK_FACTORY_HOLDOUTS" || k.to_uppercase().contains("HOLDOUT") {
                cmd.env_remove(k);
            }
        }

        let mut child = cmd.spawn().map_err(|e| DaemonError::Tool {
            tool: "sandbox-exec".to_string(),
            rc: -1,
            stderr: format!("spawn failed: {e}"),
        })?;

        let status = child.wait().map_err(|e| DaemonError::Tool {
            tool: "sandbox-exec".to_string(),
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
            }
        }

        if let Some(name) = sess_name {
            Ok(SessionId(name))
        } else {
            Err(DaemonError::Parse(format!(
                "ao spawn produced no SESSION= line: {out}"
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
}

pub struct ChainLlm;

impl Llm for ChainLlm {
    fn is_real(&self) -> bool {
        true
    }

    fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
        let r = run_tool("codex", &["exec", "--yolo", "--skip-git-repo-check", prompt], 120);
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
        let r = run_tool(&claude_bin, &["--dangerously-skip-permissions", "--print", "--setting-sources", "", prompt], 120);
        if let Ok(out) = r {
            return Ok(out);
        }
        let r = run_tool("agy", &["--dangerously-skip-permissions", "--print", prompt], 120);
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
