use crate::errors::DaemonError;
use crate::tools::{Bead, Issue, Llm, Permission, PrSnapshot, run_tool, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs};
use std::io::Read;
use std::process::{Command, Stdio};

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
}

impl CliScm {
    pub fn new(repo: String) -> Self {
        Self { repo }
    }
}

impl Scm for CliScm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError> {
        let out = run_tool(
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
        let json_start = out.find('[').unwrap_or(0);
        let gh_issues: Vec<GhIssue> = serde_json::from_str(&out[json_start..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh issue list: {e}"))
        })?;
        let issues = gh_issues.into_iter().map(|issue| Issue {
            number: issue.number,
            title: issue.title,
            body: issue.body.unwrap_or_default(),
            author_login: issue.author.login,
            external_ref: format!("{}#{}", self.repo, issue.number),
        }).collect();
        Ok(issues)
    }

    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError> {
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
        Ok(perm)
    }

    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
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
                "mergeable,reviews,headRefOid",
            ],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct GhPrView {
            mergeable: String,
            reviews: Vec<GhReview>,
            #[serde(rename = "headRefOid")]
            head_ref_oid: String,
        }
        #[derive(serde::Deserialize)]
        struct GhReview {
            author: GhAuthor,
            state: String,
        }
        #[derive(serde::Deserialize)]
        struct GhAuthor {
            login: String,
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
            &["pr", "checks", &pr_str, "--repo", &self.repo, "--json", "state,conclusion"],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct GhCheck {
            state: String,
            conclusion: String,
        }
        let json_start_c = checks_out.find('[').unwrap_or(0);
        let checks: Vec<GhCheck> = serde_json::from_str(&checks_out[json_start_c..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr checks JSON: {e}"))
        })?;
        let ci_success = !checks.is_empty() && checks.iter().all(|c| {
            c.state == "COMPLETED" && (c.conclusion == "SUCCESS" || c.conclusion == "NEUTRAL" || c.conclusion == "SKIPPED")
        });

        let comments_out = run_tool(
            "gh",
            &["pr", "view", &pr_str, "--repo", &self.repo, "--json", "comments"],
            30,
        )?;
        #[derive(serde::Deserialize)]
        struct GhCommentsView {
            comments: Vec<GhComment>,
        }
        #[derive(serde::Deserialize)]
        struct GhComment {
            author: GhAuthor,
            body: String,
        }
        let json_start_comments = comments_out.find('{').unwrap_or(0);
        let comments_view: GhCommentsView = serde_json::from_str(&comments_out[json_start_comments..]).map_err(|e| {
            DaemonError::Parse(format!("failed to parse gh pr comments JSON: {e}"))
        })?;
        let mut bugbot_error_count = 0;
        for comment in comments_view.comments {
            let author = comment.author.login;
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

        Ok(PrSnapshot {
            pr_number: pr,
            ci_success,
            mergeable,
            coderabbit_approved,
            bugbot_error_count,
            unresolved_thread_count,
            head_sha: view.head_ref_oid,
        })
    }

    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError> {
        let pr_str = pr.to_string();
        run_tool(
            "gh",
            &["pr", "close", &pr_str, "--repo", &self.repo, "-c", comment],
            30,
        )?;
        Ok(())
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
        let out = run_tool("ao", &["status", "-p", &self.project, "--json"], 30)?;
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
            .arg("-p")
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
        let out = run_tool("ao", &["status", "-p", &self.project, "--json"], 30)?;
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
        let r = run_tool(&claude_bin, &["--print", "--dangerously-skip-permissions", "--setting-sources", "", prompt], 120);
        if let Ok(out) = r {
            return Ok(out);
        }
        let r = run_tool("agy", &["--print", "--dangerously-skip-permissions", prompt], 120);
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
