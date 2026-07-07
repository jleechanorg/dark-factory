use crate::errors::DaemonError;
use crate::tools::run_tool;
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GatesComputeOutput {
    pub ci_green: String,
    pub no_conflicts: String,
    pub coderabbit: String,
    pub bugbot: String,
    pub comments_resolved: String,
}

pub fn run_gates_compute(pr: u64, repo_opt: Option<String>) -> Result<(), DaemonError> {
    let cfg_path = PathBuf::from("config/daemon.toml");
    if !cfg_path.exists() {
        return Err(DaemonError::Config("config/daemon.toml is missing".into()));
    }

    let cfg = crate::config::load(&cfg_path)?;

    let repo = match repo_opt {
        Some(r) => r,
        None => cfg.target_repo,
    };

    if repo.is_empty() {
        return Err(DaemonError::Config("target repo is empty".into()));
    }

    let pr_str = pr.to_string();

    // Gate 1: CI green
    let checks_out = run_tool(
        "gh",
        &["pr", "checks", &pr_str, "--repo", &repo, "--json", "state,bucket"],
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

    let ci_green = if checks.is_empty() {
        "unknown".to_string()
    } else {
        let mut any_pending = false;
        let mut any_failed = false;
        for c in &checks {
            if c.bucket == "pending" {
                any_pending = true;
            } else if c.bucket == "fail" || c.bucket == "cancel" {
                any_failed = true;
            }
        }
        if any_pending {
            "unknown".to_string()
        } else if any_failed {
            "red".to_string()
        } else {
            "green".to_string()
        }
    };

    // Gate 2: No conflicts
    let view_out = run_tool(
        "gh",
        &[
            "pr",
            "view",
            &pr_str,
            "--repo",
            &repo,
            "--json",
            "mergeable",
        ],
        30,
    )?;

    #[derive(serde::Deserialize)]
    struct GhPrMergeable {
        mergeable: String,
    }

    let json_start = view_out.find('{').unwrap_or(0);
    let view: GhPrMergeable = serde_json::from_str(&view_out[json_start..]).map_err(|e| {
        DaemonError::Parse(format!("failed to parse gh pr view mergeable JSON: {e}"))
    })?;

    let no_conflicts = match view.mergeable.as_str() {
        "MERGEABLE" => "green".to_string(),
        "CONFLICTING" => "red".to_string(),
        _ => "unknown".to_string(),
    };

    // Gate 3: CodeRabbit approval
    let reviews_out = run_tool(
        "gh",
        &[
            "pr",
            "view",
            &pr_str,
            "--repo",
            &repo,
            "--json",
            "reviews",
        ],
        30,
    )?;

    #[derive(serde::Deserialize)]
    struct GhReviewsView {
        reviews: Vec<GhReview>,
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

    let json_start_r = reviews_out.find('{').unwrap_or(0);
    let reviews_view: GhReviewsView = serde_json::from_str(&reviews_out[json_start_r..]).map_err(|e| {
        DaemonError::Parse(format!("failed to parse gh pr view reviews JSON: {e}"))
    })?;

    let last_coderabbit_review = reviews_view.reviews.iter()
        .filter(|r| r.author.login.contains("coderabbit") && r.state != "COMMENTED")
        .next_back();

    let coderabbit = match last_coderabbit_review {
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

    // Gate 4: Bugbot clean
    let comments_out = run_tool(
        "gh",
        &["pr", "view", &pr_str, "--repo", &repo, "--json", "comments"],
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

    let bugbot = if bugbot_error_count == 0 {
        "green".to_string()
    } else {
        "red".to_string()
    };

    // Gate 5: Comments resolved
    let owner = repo.split('/').next().unwrap_or("").to_string();
    let repo_name = repo.split('/').nth(1).unwrap_or("").to_string();
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
            &format!("repo={repo_name}"),
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

    let comments_resolved = if unresolved_thread_count == 0 {
        "green".to_string()
    } else {
        "red".to_string()
    };

    let output = GatesComputeOutput {
        ci_green,
        no_conflicts,
        coderabbit,
        bugbot,
        comments_resolved,
    };

    let out_json = serde_json::to_string_pretty(&output).map_err(|e| {
        DaemonError::Parse(format!("failed to serialize output: {e}"))
    })?;
    println!("{out_json}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_gates_compute_fails_if_config_missing() {
        let temp_dir = std::env::temp_dir().join(format!("gc_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let prev_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();

        let res = run_gates_compute(123, None);
        assert!(res.is_err());
        match res {
            Err(DaemonError::Config(msg)) => {
                assert!(msg.contains("config/daemon.toml is missing"));
            }
            other => panic!("expected Config error, got {:?}", other),
        }

        std::env::set_current_dir(prev_dir).unwrap();
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
