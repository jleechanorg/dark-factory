// Task 5: the only traits in the system (design doc §4). Each trait wraps exactly
// one external tool; production impls (Cli*) are thin `Command` wrappers sharing
// `run_tool`. Test fakes live in `daemon/tests/common/mod.rs` (scripted responses,
// call log, no subprocess use).
use crate::errors::DaemonError;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix_signals {
    pub type Pid = i32;
    pub const SIGKILL: i32 = 9;

    unsafe extern "C" {
        pub fn kill(pid: Pid, signal: i32) -> i32;
    }
}

/// A `br` bead candidate (design doc §4, spec §4.2.3).
///
/// `description`, `notes`, and `file_tree_summary` exist so the router's
/// rendered prompt (router.rs `render_prompt`) can judge routing complexity
/// from more than just the one-line title (spec Appendix C item 1 says
/// routing must be based on "the whole shape of the task" — a bare title is
/// not that):
/// * `description` — the bead's full body text as returned by
///   `br list --json` (that JSON shape's `description` field); "" if absent.
/// * `notes` — the bead's `br list --json` `notes` field (operator-authored
///   per-attempt guidance; populated via `br update --notes`, e.g. when
///   requeueing with refined scope instructions). "" if absent. Surfaced
///   into the coder prompt as a distinct, higher-priority-than-description
///   section (bead jleechan-0hqx, issue #338) so attempt rN coders don't
///   re-litigate scope that was settled when the bead was requeued.
/// * `file_tree_summary` — a short, pre-rendered listing of the repo paths
///   the bead is expected to touch (see `tools::summarize_file_tree`), so the
///   router can weigh blast radius without the LLM having to browse the repo
///   itself. "" if no relevant path is known.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub description: String, // full body/description from `br list --json`; "" if absent
    pub notes: String, // operator-authored `br update --notes` text; "" if absent
    pub file_tree_summary: String, // pre-rendered file-tree text; "" if unavailable
    pub external_ref: Option<String>, // "<owner>/<repo>#<issue_number>", None = manual bead
}

/// Render a bounded, human-readable file-tree summary rooted at `root`, for
/// embedding in the router prompt as blast-radius context (spec Appendix C
/// item 1). Stdlib-only (design doc §2's five-dependency budget has no room
/// for a `walkdir`-style crate): breadth-first over `std::fs::read_dir`,
/// skips dotfiles/dot-directories (`.git`, `.venv`, etc. are noise for a
/// router prompt), and stops after `max_entries` paths so a large repo can
/// never blow up prompt size. Returns "" (never an error) if `root` doesn't
/// exist or isn't readable — a missing/unreadable path is not fatal to
/// routing, it just means the prompt renders without this context.
pub fn summarize_file_tree(root: &std::path::Path, max_entries: usize) -> String {
    if max_entries == 0 || !root.is_dir() {
        return String::new();
    }

    let mut entries = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root.to_path_buf());

    'walk: while let Some(dir) = queue.pop_front() {
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        let mut children: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());

        for entry in children {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue; // skip .git, .venv, dotfiles — noise for a router prompt
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();

            if path.is_dir() {
                entries.push(format!("{rel}/"));
                queue.push_back(path);
            } else {
                entries.push(rel);
            }

            if entries.len() >= max_entries {
                break 'walk;
            }
        }
    }

    entries.join("\n")
}

/// A labeled GitHub issue as seen by the pre-poll normalizer (spec §4.2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author_login: String,
    pub external_ref: String, // "<owner>/<repo>#<issue_number>"
}

/// An open GitHub pull request labeled for the factory intake sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledPr {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author_login: String,
    pub external_ref: String, // "<owner>/<repo>#<pr_number>"
    pub head_ref_name: String,
    pub is_cross_repository: bool,
    pub head_repo_full_name: Option<String>,
    pub head_repo_owner_login: Option<String>,
    /// jtg8: the head commit SHA at the time `gh pr list` returned this row.
    /// Used as the primary cache key for the adoption-probe cache — if the
    /// SHA is unchanged between ticks, the daemon serves adoption/duplicate
    /// decisions from cache and skips per-PR gh probes (`collaborator_permission`,
    /// the REST `pulls/{n}` lookup). `None` when the upstream `gh pr list`
    /// JSON did not include a head SHA (defensive — the daemon MUST treat
    /// `None` as "uncacheable" and probe fresh every tick, preserving current
    /// behavior on under-detailed upstream payloads).
    pub head_sha: Option<String>,
    /// jtg8: PR `updated_at` epoch seconds at the time `gh pr list` returned
    /// this row. Secondary cache key alongside `head_sha`: edits to the PR
    /// body or comments bump `updated_at` without changing `head_sha`, and
    /// both must invalidate the cache for the per-PR permission / metadata
    /// probes to stay fresh. `None` = uncacheable (same rationale as
    /// `head_sha`).
    pub updated_at_epoch: Option<u64>,
}

/// Collaborator permission tier, coarsened to the write-tier gate (spec §4.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Permission {
    None,
    Read,
    Triage,
    Write,
    Admin,
}

impl Permission {
    /// Only `Write` or `Admin` may trigger dispatch (spec §4.2.3 write-tier minimum).
    pub fn is_write_tier(&self) -> bool {
        matches!(self, Permission::Write | Permission::Admin)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrComment {
    pub author: String,
    pub body: String,
    /// Unix epoch (seconds) the comment was created, or 0 when unknown
    /// (old offline snapshots, synthetic comments). jleechan-nplh: needed
    /// so `/er` verdicts posted BEFORE the PR's current head was pushed can
    /// be recognized as stale instead of short-circuiting re-verification.
    #[serde(default)]
    pub created_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrFile {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

/// One gate's read from the SCM, gathered for the 7/8-green verifier (spec §4.2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSnapshot {
    pub pr_number: u64,
    pub ci_success: bool,
    pub mergeable: bool,
    /// Bead jleechan-qzr3 / pr655-finding-1: `true` when GitHub returned
    /// `mergeable: null` from the REST fallback path (the merge-state
    /// computation is still pending, or rate-limit deferred). The verifier
    /// treats this arm as `GateResult::Unknown` rather than `Red` so the
    /// daemon does not fail-closed-stall on a transient fetch. When `false`,
    /// `mergeable` is authoritative; when `true`, `mergeable` is still
    /// populated (current snapshot's guess from the prior tick) but the
    /// gate emits `Unknown("PR mergeable state not yet computed (transient)")`.
    pub merge_state_unknown: bool,
    pub coderabbit_approved: bool,
    pub bugbot_error_count: u32,
    /// Count of unresolved GitHub review threads, or `None` when the
    /// GraphQL fetch/parse failed and the count could not be proven.
    /// jleechan-kk64: `None` MUST be treated as unknown/unverifiable by
    /// every caller — never coerced to `0`/Green. A transient GitHub API
    /// failure or malformed GraphQL response is not evidence that zero
    /// threads are unresolved.
    pub unresolved_thread_count: Option<u32>,
    pub head_sha: String,
    pub body: String,
    pub comments: Vec<PrComment>,
    pub files: Vec<PrFile>,
    pub updated_at_epoch: u64,
    pub ci_status: String,
    pub coderabbit_status: String,
    pub ci_pending: bool,
    /// jleechan-8s2p (phase 2): structured `bugbot_pending` field
    /// paralleling `coderabbit_status`. True iff Bugbot's check run is
    /// in a PENDING state on this snapshot — i.e. Bugbot has not yet
    /// reviewed the PR (outage / stuck / fair-use cap). The previous
    /// detector keyed on `bugbot_error_count > 0`, which is the
    /// FAILURE signal (Bugbot ran and produced error comments), NOT
    /// the OUTAGE signal. That made the Bugbot waiver path
    /// unreachable in production: real outages produce
    /// `error_count == 0`, so the predicate never returned true and
    /// the ledger never recorded a cap observation. The waiver must
    /// NEVER substitute for a real Bugbot RED verdict — that is still
    /// handled by the `> 0` branch in the BugbotClean gate. This
    /// field is the structural-unavailability signal only.
    pub bugbot_pending: bool,
    /// Unix epoch (seconds) of the head commit's committer date, or 0 when
    /// unknown. jleechan-nplh: the freshness floor for `/er` verdict
    /// comments — a verdict older than this predates the code it claims to
    /// verify. Committer date (not author date) is the best available proxy:
    /// rebases and merges rewrite it. Known limitation (independent review,
    /// PR#227): it is NOT the push date — a locally backdated commit pushed
    /// later can leave a window where a verdict for a prior head still
    /// clears the floor. GitHub exposes no reliable per-commit push
    /// timestamp, so this floor narrows the stale-verdict hole rather than
    /// closing it exactly.
    pub head_committed_epoch: u64,
}

/// Parameters for spawning a new AO/`aow` session (design doc §4).
///
/// `repo`/`ao_project`/`remote` (bead jleechan-35y4, Stage B of the
/// multi-repo dispatch fix — see
/// `docs/multirepo-dispatch-investigation-2026-07-11.md`) carry the
/// dispatch-time repo identity resolved by the caller (`overlay.repo(cfg)` +
/// `Config::resolve_repo`). Adding them here is the CAPABILITY: today's
/// `Sessions`/`CliSessions` impl still binds its AO project once at
/// construction time and does not yet consume these fields per-spawn — that
/// full call-site migration (dispatch prompt template + spawn-time remote
/// assertion) is Stage C, bead jleechan-bqdv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub bead_id: String,
    pub branch: String,
    pub prompt: String,
    /// `owner/repo` this bead's work belongs to (`overlay.repo(cfg)`).
    pub repo: String,
    /// AO project name to spawn into for `repo` (`Config::resolve_repo`).
    pub ao_project: String,
    /// git remote name the coder must push to for `repo` (e.g. `"origin"`
    /// or a dual-remote clone's non-default remote like `"worldai"`).
    pub remote: String,
    /// Local checkout to use as the AO process cwd. This keeps worktree
    /// creation and repository discovery bound to the bead's target repo.
    pub local_checkout: Option<std::path::PathBuf>,
    /// Authoritative revision the checkout must contain before spawning. For
    /// adopted remediation this is the remote branch SHA captured immediately
    /// before dispatch; a same-origin checkout at another HEAD is unsafe.
    pub expected_revision: Option<String>,
    /// Whether `local_checkout` is daemon-owned and may be refreshed to
    /// `expected_revision` after origin and cleanliness checks. Explicit
    /// operator-configured checkouts must remain protected from mutation.
    pub managed_checkout: bool,
    /// Bead jleechan-jw4c: the worker session's expected working directory
    /// (typically the agent-id subdirectory under `cfg.agent_worktree_root`).
    /// Stored on the spec so adapter implementations can validate the
    /// child process's cwd before returning `Ok`. `None` disables the guard
    /// for legacy single-checkout layouts where the operator has not flipped
    /// on the new isolation root.
    pub expected_cwd: Option<std::path::PathBuf>,
}

/// Opaque handle to an AO/`aow` session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// Helper function to convert ISO-8601 Zulu format to Unix epoch seconds
fn days_from_civil(mut y: i64, m: u32, d: u32) -> i64 {
    y -= if m <= 2 { 1 } else { 0 };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = ((153 * mp + 2) / 5) as u64 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

pub fn iso8601_to_epoch(s: &str) -> Option<u64> {
    if s.len() < 19 {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let mo: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    let h: u64 = s[11..13].parse().ok()?;
    let mi: u64 = s[14..16].parse().ok()?;
    let sec: u64 = s[17..19].parse().ok()?;

    let days = days_from_civil(y, mo, d);
    if days < 0 {
        return None;
    }
    let epoch = (days as u64) * 86400 + h * 3600 + mi * 60 + sec;
    Some(epoch)
}

/// Pure syntax normalization (no judgment call, ZFC-exempt): does a git
/// remote URL (`https://github.com/owner/repo.git`, `https://github.com/owner/repo`,
/// `git@github.com:owner/repo.git`, `git@github.com:owner/repo`, an
/// `ssh://` form with or without an explicit port, a `https://` URL with
/// embedded credentials/token, or any of those forms with a trailing `/`)
/// name the same `owner/repo` as `repo` (already in canonical `owner/repo`
/// form)?
///
/// Returns `Some(bool)` when `url` is in a recognized github.com form (the
/// bool is the actual match/mismatch verdict), or `None` when `url` is in a
/// form this normalizer does not recognize (a different host — including
/// GitHub Enterprise — an unusual scheme, or malformed input).
///
/// `None` means the URL cannot be tied positively to canonical github.com.
/// Spawn-time dispatch is fail-closed for canonical GitHub targets: callers
/// must reject both `Some(false)` and `None`, because a local path, different
/// host, or unusual scheme is not evidence that the workspace can safely
/// push to `repo`. Keeping indeterminate distinct from a parsed mismatch is
/// still useful for precise telemetry.
///
/// Bead jleechan-bqdv, Stage C spawn-time remote assertion: the worktree a
/// coder session lands in may be cloned via either transport depending on
/// how the local AO project checkout was set up, so the comparison must
/// tolerate both without guessing at intent — this is deterministic string
/// normalization, not semantic classification.
pub fn remote_url_matches_repo(url: &str, repo: &str) -> Option<bool> {
    let url = url.trim().trim_end_matches('/');
    let repo = repo.trim().trim_end_matches('/');
    if repo.is_empty() || url.is_empty() {
        return None;
    }

    // `https://[user[:token]@]github.com/owner/repo[.git]` (including
    // `http://` and an embedded credentials/token component, e.g.
    // `https://x-access-token:ghp_xxx@github.com/owner/repo.git`).
    for scheme in ["https://", "http://"] {
        let Some(rest) = url.strip_prefix(scheme) else {
            continue;
        };
        // Drop everything up to and including the LAST '@' before the first
        // '/', which strips any userinfo component without being fooled by
        // an '@' that might legitimately appear later in a path segment.
        let host_and_path = match rest.find('/') {
            Some(slash_idx) => {
                let (authority, path) = rest.split_at(slash_idx);
                let host = authority.rsplit('@').next().unwrap_or(authority);
                format!("{host}{path}")
            }
            None => rest.to_string(),
        };
        let path = host_and_path.strip_prefix("github.com/")?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        return Some(path.eq_ignore_ascii_case(repo));
    }

    // `git@github.com:owner/repo[.git]` (scp-like syntax; no port support in
    // this form).
    if let Some(path) = url.strip_prefix("git@github.com:") {
        let path = path.strip_suffix(".git").unwrap_or(path);
        return Some(path.eq_ignore_ascii_case(repo));
    }

    // `ssh://git@github.com[:PORT]/owner/repo[.git]`.
    if let Some(rest) = url.strip_prefix("ssh://git@github.com") {
        let rest = rest.strip_prefix(':').map_or(rest, |after_colon| {
            // Skip the numeric port, if present, up to the next '/'.
            after_colon
                .find('/')
                .map(|i| &after_colon[i..])
                .unwrap_or(after_colon)
        });
        let path = rest.strip_prefix('/')?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        return Some(path.eq_ignore_ascii_case(repo));
    }

    None
}

/// Safe display value for a configured git remote URL.
///
/// Remote URLs may contain HTTP userinfo credentials. Keep the raw value
/// available only to deterministic matching and never copy any part of it
/// into errors, telemetry, or outward-facing escalation comments.
pub fn remote_url_for_display(_url: &str) -> &'static str {
    "<redacted-git-remote>"
}

/// Pure syntax transform (no judgment call, ZFC-exempt), bead
/// jleechan-coder-silent-false-parks-h92r: Claude Code CLI names each
/// session's transcript directory under `~/.claude/projects/` after the
/// absolute cwd it was launched in, with every `/` and `.` replaced by `-`
/// (observed convention, e.g. `/home/jleechan/.worktrees/dark-factory/df-100`
/// -> `-home-jleechan--worktrees-dark-factory-df-100`). This lets the
/// coder-silence watcher locate a dispatched coder's own transcript
/// directory from the absolute worktree path AO already reports at spawn
/// time, without guessing at session identity.
/// Bead jleechan-jw4c: validate that the actual `cwd` of a spawned worker
/// matches the daemon's expected cwd. Returns `Ok(())` when the guard is
/// disabled (`expected` is `None`, which is the legacy layout) or when the
/// paths match. Returns `Err(DaemonError::WorktreeCwdMismatch)` when they
/// differ — the failure mode that the bead's RED measurement described
/// (worker writing into the shared primary checkout while its assigned
/// worktree was a different tree).
///
/// The comparison uses canonical paths on both sides so a `..`-resolved
/// `local_checkout` (e.g. `cwd` from a wrapper that landed at the repo
/// root) still matches the absolute form the daemon passed in. Relative
/// anchors are rejected: a worker whose cwd is `"."` is treated as a
/// mismatch because the assignment always uses an absolute path.
pub fn check_cwd_guard(
    expected: Option<&std::path::Path>,
    actual: &std::path::Path,
) -> Result<(), DaemonError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expected_canon = expected
        .canonicalize()
        .unwrap_or_else(|_| expected.to_path_buf());
    let actual_canon = actual
        .canonicalize()
        .unwrap_or_else(|_| actual.to_path_buf());
    if expected_canon == actual_canon {
        Ok(())
    } else {
        Err(DaemonError::WorktreeCwdMismatch {
            expected: expected_canon.display().to_string(),
            actual: actual_canon.display().to_string(),
        })
    }
}

pub fn claude_project_slug(worktree_path: &std::path::Path) -> String {
    worktree_path
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod claude_project_slug_tests {
    use super::claude_project_slug;
    use std::path::Path;

    #[test]
    fn replaces_slashes_and_dots_with_dashes() {
        let path = Path::new("/home/jleechan/.worktrees/dark-factory/df-100");
        assert_eq!(
            claude_project_slug(path),
            "-home-jleechan--worktrees-dark-factory-df-100"
        );
    }

    #[test]
    fn handles_plain_projects_path_without_leading_dotdir() {
        let path = Path::new("/home/jleechan/projects/dark-factory");
        assert_eq!(
            claude_project_slug(path),
            "-home-jleechan-projects-dark-factory"
        );
    }
}

/// `br` CLI. `fetch_candidates` == `br list --status open --label factory --json`.
pub trait Tracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError>;
    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError>;
    fn create_bead(
        &self,
        title: &str,
        body: &str,
        external_ref: &str,
    ) -> Result<String, DaemonError>;
    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError>;
}

pub fn parse_external_ref_repo(external_ref: &str) -> Option<String> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some(parts[0].to_string())
    } else {
        None
    }
}

/// `gh` CLI (REST + GraphQL). Production adapters use short-lived in-memory
/// TTL caches for repeated tick reads; a durable ETag cache is not wired yet.
pub trait Scm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError>;
    /// Fetch labeled PRs. `gh_calls` is incremented by every `gh` (or
    /// equivalent) subprocess the implementation makes — including the
    /// REST fallback's per-PR `pulls/{n}` calls. The intake code adds
    /// this into `IntakeProbeMetrics.gh_call_count` so the slow-tier
    /// `INTAKE_GH_CALL_WARN_THRESHOLD` warning reflects real subprocess
    /// invocations rather than counting the list query as 1 while
    /// silently burning O(N) on the REST fallback (bead jtg8-r5, codex
    /// P2 review "Count REST fallback subprocesses in gh metrics").
    fn labeled_prs(&self, label: &str, gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError>;
    /// Repo-scoped variant of [`labeled_prs`](Scm::labeled_prs). Default impl
    /// filters `labeled_prs` for items matching `repo` in `external_ref`, avoiding
    /// replaying identical PR lists across repositories on fake adapters. `CliScm`
    /// overrides to retarget the query via `with_repo`.
    fn labeled_prs_for_repo(
        &self,
        repo: &str,
        label: &str,
        gh_calls: &mut u32,
    ) -> Result<Vec<LabeledPr>, DaemonError> {
        let prs = self.labeled_prs(label, gh_calls)?;
        Ok(prs
            .into_iter()
            .filter(|pr| {
                if let Some(owner_repo) = parse_external_ref_repo(&pr.external_ref) {
                    owner_repo.eq_ignore_ascii_case(repo)
                } else {
                    false
                }
            })
            .collect())
    }
    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError>;
    /// Repo-scoped variant of [`collaborator_permission`](Scm::collaborator_permission).
    /// Default impl delegates to `collaborator_permission`; `CliScm` overrides to retarget
    /// via `with_repo`.
    fn collaborator_permission_for_repo(
        &self,
        repo: &str,
        login: &str,
    ) -> Result<Permission, DaemonError> {
        let _ = repo;
        self.collaborator_permission(login)
    }
    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError>;
    /// Repo-scoped variant of [`pr_snapshot`](Scm::pr_snapshot) (bead
    /// jleechan-9xrs, Stage D of the multi-repo dispatch fix — see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`). The
    /// verification loop (`skeptic_evidence`, `verifier::assess`,
    /// `er_runner::maybe_run`, and the fast-tier PR-state fetches in
    /// `tick.rs`) used to always fetch `pr_snapshot` against whatever repo
    /// the daemon's global adapter was constructed with (`cfg.target_repo`
    /// at `main.rs` startup) — silently wrong for any bead whose
    /// `overlay.repo(cfg)` names a DIFFERENT repo. `repo` should be
    /// `overlay.repo(cfg)`, not `cfg.target_repo` directly. Default impl
    /// ignores `repo` and delegates to `pr_snapshot` so existing test fakes
    /// and any impl that predates this method keep their original
    /// (single-repo) behavior; `CliScm` overrides it to actually retarget
    /// the query via `with_repo`.
    fn pr_snapshot_for_repo(&self, repo: &str, pr: u64) -> Result<PrSnapshot, DaemonError> {
        let _ = repo;
        self.pr_snapshot(pr)
    }
    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError>;
    /// Repo-scoped variant of [`close_pr`](Scm::close_pr) (bead jleechan-v6ud
    /// / issue #340). The factory-fabricated re-roll path
    /// (`reroll::execute` step 7) used to close the superseded PR via
    /// `close_pr(pr_number, comment)` — which is bound at `main.rs`
    /// construction time to `cfg.target_repo`. When a bead's resolved
    /// `overlay.repo(cfg)` names a DIFFERENT repo (Stage A intake), `gh pr
    /// close <n> --repo <default>` silently targets the DEFAULT repo's
    /// PR with the same numeric ID — and if that PR is already merged
    /// (the live failure for beads 8jxr and 9rkz: a same-numbered PR in
    /// `jleechanorg/worldarchitect.ai` was already merged at the moment
    /// the daemon tried to close it against the default repo), `gh` errors
    /// out with "can't be closed because it was already merged" and the
    /// bead wedges on a transient tool error. `repo` should always be
    /// `overlay.repo(cfg)`, not `cfg.target_repo`. Default impl ignores
    /// `repo` and delegates to `close_pr` so existing test fakes and any
    /// impl that predates this method keep their original (single-repo)
    /// behavior; `CliScm` overrides it to retarget via `with_repo`.
    fn close_pr_for_repo(&self, repo: &str, pr: u64, comment: &str) -> Result<(), DaemonError> {
        let _ = repo;
        self.close_pr(pr, comment)
    }
    fn remote_branch_last_commit(&self, branch: &str) -> Result<Option<u64>, DaemonError>;
    /// Repo-scoped variant of [`remote_branch_last_commit`](Scm::remote_branch_last_commit)
    /// (bead jleechan-bqdv, Stage C of the multi-repo dispatch fix — see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`). The daemon's
    /// coder-silence watcher (`tick.rs`'s `Dispatched` autonomy check) used
    /// to always poll `cfg.target_repo`'s branch, which is silently wrong
    /// for any bead whose `overlay.repo(cfg)` names a DIFFERENT repo — the
    /// watcher could never observe that coder's real progress and would
    /// eventually park it `coder_silent` even while it was actively pushing
    /// commits to its own (correct) repo. `repo` should be
    /// `overlay.repo(cfg)`, not `cfg.target_repo` directly. Default impl
    /// ignores `repo` and delegates to `remote_branch_last_commit` so
    /// existing test fakes and any impl that predates this method keep their
    /// original (single-repo) behavior; `CliScm` overrides it to actually
    /// retarget the query via `with_repo`.
    fn remote_branch_last_commit_for_repo(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        let _ = repo;
        self.remote_branch_last_commit(branch)
    }
    /// Resolve the head branch of PR `pr` in `repo`, but ONLY when that PR
    /// is currently OPEN AND its head lives in the SAME repo
    /// (bead jleechan-drive-pr-branch-binding-pcpr). Used at dispatch time
    /// to distinguish "drive an existing open PR" beads (whose
    /// `external_ref` names a live, same-repo PR — the coder MUST land work
    /// on the PR's own head branch, or AO's fail-closed branch validation
    /// parks the bead `session_branch_mismatch` when it reuses the session
    /// already bound to that branch) from ordinary create-new-work beads
    /// (which always get a fresh generated `factory/<bead>-r<attempt>`
    /// branch).
    ///
    /// `PrHeadBranch::Fork` is the fail-closed guard mirroring
    /// `intake::same_repo_pr`: a PR whose head lives on a FORK must never
    /// be bound to by name — the base repo has no such branch, so binding
    /// would create an unrelated same-named branch there and silently never
    /// touch the actual PR. `PrHeadBranch::NotFound` is the fail-safe
    /// default for every case that must fall back to the generated-branch
    /// path: a closed/merged/missing PR, an `external_ref` number that
    /// isn't actually a pull request, or any lookup failure (transient
    /// `gh` error, malformed response). Neither variant lets an
    /// inconclusive lookup fabricate a branch binding it can't positively
    /// confirm — see `CliScm`'s override for the real `gh api` lookup.
    /// Default impl returns `Ok(PrHeadBranch::NotFound)` unconditionally so
    /// every existing test fake and any impl that predates this method
    /// keeps behaving exactly as before (always the generated-branch path)
    /// without needing to implement it.
    fn open_pr_head_ref_for_repo(&self, repo: &str, pr: u64) -> Result<PrHeadBranch, DaemonError> {
        let _ = (repo, pr);
        Ok(PrHeadBranch::NotFound)
    }
    /// Resolve the CURRENT open PR whose head ref is `branch` in `repo`
    /// (bead jleechan-t40t, issue #326 branch-mismatch stale-state defect).
    /// Returns `Ok(Some(pr))` when a single open PR is bound to `branch`,
    /// `Ok(None)` when no such PR exists (or the lookup cannot positively
    /// confirm one), or `Err` on a hard tool failure. Used by the slow-tier
    /// DISPATCHED re-resolution path to detect drift between a bead's
    /// recorded `pr_number` and the PR actually bound to its branch right
    /// now — without this check, a stale `pr_number` (e.g. set from an AO
    /// session that has since been superseded by a later PR on the same
    /// branch) can keep a bead wedged DISPATCHED indefinitely: every tick
    /// queries the wrong PR and the real PR is never gate-assessed.
    /// `repo` should be `overlay.repo(cfg)`, not `cfg.target_repo`, so
    /// cross-repo beads are observable. Default impl returns `Ok(None)`
    /// unconditionally so existing test fakes and any impl that predates
    /// this method keep their original behavior; `CliScm` overrides it to
    /// actually call `gh pr list --head <branch>`.
    fn pr_number_for_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        let _ = (repo, branch);
        Ok(None)
    }
    /// Bead jleechan-yoqy / issue #323: verify a gist for the evidence gate.
    ///
    /// - `Ok(Some(true))` — fetchable AND non-empty (evidence Verified).
    /// - `Ok(Some(false))` — fetchable but EMPTY (definitive Failed).
    /// - `Ok(None)` — DEFINITIVELY not found: 404 / deleted / private (Failed).
    /// - `Err(..)` — TRANSIENT (gh outage / network): the gate waits (Unknown),
    ///   it does NOT churn a reroll (r5 finding 3).
    ///
    /// Default impl is `Err` ("unverifiable") so fakes and impls that predate
    /// this method never accidentally pass the gate; `CliScm` overrides it to
    /// run `gh api gists/<id>`.
    fn gist_nonempty(&self, gist_id: &str) -> Result<Option<bool>, DaemonError> {
        Err(DaemonError::Tool {
            tool: "gh".into(),
            rc: -1,
            stderr: format!("gist_nonempty not implemented for this adapter (gist {gist_id})"),
        })
    }
}

/// Bead jleechan-yoqy / issue #323: the ONE canonical evidence marker literal.
/// The coder-dispatch prompt requires the coder to put
/// `**Evidence**: <gist-url> (head <sha>)` in the PR body; the `/er` reviewer
/// contract references this same constant; and the verifier parser matches it
/// (case-insensitive, with the `**` bold markers optional). Defining it once
/// here — the shared low-level module every layer imports — guarantees the
/// prompt, the reviewer contract, and the parser can never drift apart.
pub const EVIDENCE_MARKER: &str = "**Evidence**:";

/// Resolution of an [`Scm::open_pr_head_ref_for_repo`] lookup (bead
/// jleechan-drive-pr-branch-binding-pcpr). A three-way result rather than
/// `Option<String>` so callers can tell "confirmed open PR, but its head is
/// on a fork — fail-closed, do not bind" apart from "no open PR found at
/// all" — the two have the same fallback (generated branch) but very
/// different causes, and dispatch-time telemetry needs to distinguish them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrHeadBranch {
    /// PR `pr` is OPEN and its head repo matches the queried `repo` —
    /// safe to bind the coder branch to this ref.
    SameRepo(String),
    /// PR `pr` is OPEN but its head lives in a DIFFERENT repo (a fork, or
    /// a deleted-fork PR whose `head.repo` GitHub no longer reports).
    /// Binding to this branch name in the queried repo would create an
    /// unrelated same-named branch there and never touch the actual PR —
    /// mirrors the fail-closed fork guard `intake::same_repo_pr` already
    /// applies to PR adoption.
    Fork,
    /// Closed/merged/missing PR, a `pr` number that isn't a pull request,
    /// or a lookup/parse failure.
    NotFound,
}

/// Liveness classification of a single AO session (bead jleechan-zeij /
/// issue #322 r2). `is_quiescent` collapses everything into a single
/// terminal-or-not boolean, which cannot tell "the worker exited" apart from
/// "the worker finished its task and went back to idle without an explicit
/// kill" — the exact `status=spawning, activity=idle` state that made the r1
/// quiescence loop stall. The re-roll fail-closed proceed predicate
/// (`reroll::execute`) needs that distinction: an `Idle` worker with a stable
/// branch HEAD is safe to supersede (predicate (c)), a `Running` worker is
/// NOT (it may still be pushing), so they must be joined with head-stability
/// in the same poll rather than treated identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionActivity {
    /// AO reports the session is actively doing work (any non-idle,
    /// non-terminal `activity`). Never safe to supersede — the worker may be
    /// mid-`git push`.
    Running,
    /// AO reports the session is alive but idle (`activity == "idle"`) — the
    /// #322 live signature. Safe to supersede ONLY jointly with a stable
    /// branch HEAD.
    Idle,
    /// AO reports one of its terminal statuses (`killed`/`done`/…) or
    /// `activity == "exited"` — equivalent to `is_quiescent == true`.
    Terminal,
    /// No AO status row currently names this session — the worker has been
    /// fully reaped. Equivalent to a `SessionNotFound` attach for supersede
    /// purposes (nothing live left to guard against).
    NotFound,
}

/// Append-only evidence read from the exact AO worker worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHeadAncestry {
    pub head_sha: String,
    pub contains_ancestor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRuntimeIdentity {
    pub session_id: SessionId,
    pub project: String,
    pub runtime_id: Option<String>,
    pub worktree_path: Option<std::path::PathBuf>,
    pub branch: Option<String>,
}

/// `ao` / `aow` CLIs.
pub trait Sessions {
    fn active_count(&self) -> Result<usize, DaemonError>;
    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError>;
    fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError>;
    /// Resolve a branch within its owning AO project. The default preserves
    /// compatibility with single-project adapters and test doubles.
    fn attach_in_project(
        &self,
        project: &str,
        branch: &str,
        bead_id: &str,
    ) -> Result<SessionId, DaemonError> {
        let _ = project;
        self.attach(branch, bead_id)
    }
    fn stop(&self, id: &SessionId) -> Result<(), DaemonError>;
    /// Stop a session while the caller has resolved its owning AO project.
    /// The default preserves compatibility with fakes and adapters whose
    /// session ids are globally addressable.
    fn stop_in_project(
        &self,
        project: &str,
        id: &SessionId,
    ) -> Result<(), DaemonError> {
        let _ = project;
        self.stop(id)
    }
    fn stop_runtime_in_project(&self, project: &str, id: &SessionId) -> Result<(), DaemonError> {
        self.stop_in_project(project, id)
    }
    fn confirm_runtime_absent_in_project(
        &self,
        project: &str,
        id: &SessionId,
    ) -> Result<bool, DaemonError> {
        let _ = (project, id);
        Ok(true)
    }
    fn archive_session_metadata_in_project(
        &self,
        project: &str,
        id: &SessionId,
        quarantined_worktree: Option<&std::path::Path>,
        dirty_hash: Option<&str>,
    ) -> Result<(), DaemonError> {
        let _ = (project, id, quarantined_worktree, dirty_hash);
        Ok(())
    }
    fn resolve_runtime_in_project(
        &self,
        project: &str,
        id: &SessionId,
    ) -> Result<Option<SessionRuntimeIdentity>, DaemonError> {
        let _ = (project, id);
        Ok(Some(SessionRuntimeIdentity {
            session_id: id.clone(),
            project: project.to_string(),
            runtime_id: Some(id.0.clone()),
            worktree_path: None,
            branch: None,
        }))
    }
    fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError>;
    /// Project-scoped liveness probe. The default delegates so existing
    /// adapters and test doubles remain source-compatible.
    fn is_quiescent_in_project(
        &self,
        project: &str,
        id: &SessionId,
    ) -> Result<bool, DaemonError> {
        let _ = project;
        self.is_quiescent(id)
    }
    /// Budget-bounded `attach` (bead jleechan-zeij / issue #322 r4 P2). The
    /// re-roll proceed poll caps each probe at the time remaining until its
    /// window deadline so a single poll cannot block for multiples of the
    /// window on stacked ~30s subprocess timeouts. The default ignores the
    /// budget and delegates to [`attach`](Sessions::attach) (fakes are
    /// instant); the real `CliSessions` overrides it to pass `timeout_secs`
    /// down to `ao status`.
    fn attach_within(
        &self,
        branch: &str,
        bead_id: &str,
        timeout_secs: u64,
    ) -> Result<SessionId, DaemonError> {
        let _ = timeout_secs;
        self.attach(branch, bead_id)
    }
    fn attach_within_in_project(
        &self,
        project: &str,
        branch: &str,
        bead_id: &str,
        timeout_secs: u64,
    ) -> Result<SessionId, DaemonError> {
        let _ = project;
        self.attach_within(branch, bead_id, timeout_secs)
    }
    /// Budget-bounded [`session_activity`](Sessions::session_activity) (bead
    /// jleechan-zeij / issue #322 r4 P2). Default delegates to the unbounded
    /// method; `CliSessions` overrides to pass `timeout_secs` to `ao status`.
    fn session_activity_within(
        &self,
        id: &SessionId,
        timeout_secs: u64,
    ) -> Result<SessionActivity, DaemonError> {
        let _ = timeout_secs;
        self.session_activity(id)
    }
    fn session_activity_within_in_project(
        &self,
        project: &str,
        id: &SessionId,
        timeout_secs: u64,
    ) -> Result<SessionActivity, DaemonError> {
        let _ = project;
        self.session_activity_within(id, timeout_secs)
    }
    /// Activity probe distinguishing idle vs running vs terminal (bead
    /// jleechan-zeij / issue #322 r2 — see [`SessionActivity`]). The default
    /// derives from `is_quiescent`: a quiescent session maps to `Terminal`,
    /// a non-quiescent one to `Running`. That default deliberately CANNOT
    /// report `Idle` — it fails closed toward "still running", so any adapter
    /// that does not override this treats an idle worker as live and defers
    /// rather than superseding it. The real adapter (`CliSessions`) overrides
    /// this to read AO's `activity` field directly and surface `Idle`.
    fn session_activity(&self, id: &SessionId) -> Result<SessionActivity, DaemonError> {
        if self.is_quiescent(id)? {
            Ok(SessionActivity::Terminal)
        } else {
            Ok(SessionActivity::Running)
        }
    }
    /// Project-scoped activity probe. The default delegates so existing
    /// adapters and test doubles remain source-compatible.
    fn session_activity_in_project(
        &self,
        project: &str,
        id: &SessionId,
    ) -> Result<SessionActivity, DaemonError> {
        let _ = project;
        self.session_activity(id)
    }
    /// Post-spawn session health monitor: checks if an active session died,
    /// failed authentication, hit quota limits, or suffered terminal errors in its terminal.
    fn check_session_health(&self, id: &SessionId) -> Result<Option<String>, DaemonError> {
        let _ = id;
        Ok(None)
    }
    /// Bead rev-4ou1z: wakes a session paused at a benign prompt (e.g. a
    /// Gemini "Individual quota reached" message whose reset time has
    /// passed) by sending an Enter keypress to its tmux pane. Returns
    /// `Ok(true)` when a pane was found and poked. Default no-op — fakes
    /// and tests that don't model tmux panes are unaffected; `CliSessions`
    /// overrides this with the real tmux `send-keys` call.
    fn wake_pane(&self, id: &SessionId) -> Result<bool, DaemonError> {
        let _ = id;
        Ok(false)
    }
    /// Returns the live branch AO reports for a given session, if known.
    ///
    /// jleechan-5ia2: a `bead_overlay` row was found with
    /// `state=DISPATCHED` and a real, live `session_id` — but that session
    /// belonged to a completely unrelated, pre-existing task/branch. No code
    /// path in this crate can produce that pairing through a genuine
    /// `spawn()` return value (AO's session-id reservation is atomic —
    /// `O_EXCL` — so a fresh spawn can never return an already-taken id, and
    /// the id in question predated this bead's dispatch attempt by ~1h).
    /// The row was almost certainly written by a stray out-of-band `sqlite3
    /// UPDATE` bypassing both this crate and `factory-overlay.sh`'s own
    /// "no direct sqlite3 mutations" contract. This check, combined with the
    /// `dispatch_ready` post-spawn verification and the `tick.rs`
    /// wedge-detection sweep, makes such a row impossible to *create* going
    /// forward and impossible to keep silently *trusting* if one somehow
    /// appears.
    ///
    /// `Ok(None)` covers both "session not found" and "adapter cannot
    /// verify" — callers must NOT distinguish. The default impl (for fakes
    /// that predate this check) always returns `Ok(None)`, which callers
    /// treat as "cannot verify, do not block" — this method only ever
    /// *rejects* a dispatch on a positively confirmed mismatch, never on
    /// absence of information.
    fn session_branch(&self, id: &SessionId) -> Result<Option<String>, DaemonError> {
        let _ = id;
        Ok(None)
    }
    /// Returns the git remote URL configured for `remote_name` inside the
    /// worktree backing a just-spawned session for `ao_project`/`branch`,
    /// or `None` when it cannot be determined (bead jleechan-bqdv, Stage C —
    /// see `docs/multirepo-dispatch-investigation-2026-07-11.md`). This is
    /// the spawn-time proper fix for jleechan-9sh5: a worktree cloned from
    /// the wrong local checkout (e.g. a dual-remote `worldarchitect.ai`
    /// clone whose `origin` points at `jleechanclaw`, not
    /// `worldarchitect.ai`) can silently strand a coder pushing to the wrong
    /// repo. `dispatch::dispatch_ready` calls this immediately after a
    /// successful `spawn()`, before trusting the dispatch as DISPATCHED, and
    /// compares the returned URL against the bead's resolved repo
    /// (`owner/repo`) via `remote_url_matches_repo`.
    ///
    /// The production adapter must fail closed when it cannot inspect the
    /// exact workspace AO returned. The default remains `Ok(None)` so older
    /// test adapters compile, but dispatch treats that absence as an
    /// unverifiable workspace and refuses to adopt the new session.
    fn worktree_remote_url(
        &self,
        ao_project: &str,
        branch: &str,
        remote_name: &str,
    ) -> Result<Option<String>, DaemonError> {
        let _ = (ao_project, branch, remote_name);
        Ok(None)
    }
    /// Inspect the exact AO worktree recorded for `session_id` and verify it
    /// still belongs to `expected_branch`, then report whether its current
    /// HEAD contains `ancestor_sha`. `Ok(None)`
    /// means no live-process workspace mapping is available (for example,
    /// after a daemon restart), so callers must use their remote fallback.
    fn worktree_head_ancestry(
        &self,
        session_id: &SessionId,
        expected_branch: &str,
        ancestor_sha: &str,
    ) -> Result<Option<WorktreeHeadAncestry>, DaemonError> {
        let _ = (session_id, expected_branch, ancestor_sha);
        Ok(None)
    }
    /// Most recent modification time (unix epoch seconds) observed across
    /// the coder's own Claude Code transcript directory for the worktree
    /// backing `ao_project`/`branch`, or `None` when it cannot be
    /// determined (bead jleechan-coder-silent-false-parks-h92r).
    ///
    /// 2026-07-17: all 6 active dispatch lanes were parked
    /// `PARKED_HUMAN_HELD reason=coder_silent` by `tick.rs`'s wedge-detection
    /// sweep while their coders were demonstrably working — transcripts
    /// growing, commits landing — because that sweep's only liveness signal
    /// was "has the branch received a REMOTE commit in the last 30
    /// minutes". A coder can spend well over 30 minutes editing, running
    /// tests, and iterating locally before its next push; silence on the
    /// remote branch is not evidence the coder is silent. This method gives
    /// the sweep a second, independent liveness signal sourced from the
    /// coder's own transcript activity, which updates continuously
    /// regardless of push cadence.
    ///
    /// `Ok(None)` means "no evidence" (missing worktree mapping, missing
    /// transcript directory, unreadable files, no `$HOME`) — callers MUST
    /// NOT treat that as proof the coder is silent, only as "this signal
    /// could not corroborate liveness". The default impl (for fakes/older
    /// adapters) always returns `Ok(None)`, which preserves today's
    /// branch-only fail-closed behavior for any caller that doesn't opt
    /// into the combined check.
    fn worktree_transcript_last_activity_epoch(
        &self,
        ao_project: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        let _ = (ao_project, branch);
        Ok(None)
    }
    fn spawn_batch(&self, specs: &[SpawnSpec]) -> Result<Vec<SessionId>, DaemonError> {
        let mut ids = Vec::new();
        for spec in specs {
            ids.push(self.spawn(spec)?);
        }
        Ok(ids)
    }
}

/// `git` CLI, always `git -C <workdir>`.
pub trait Vcs {
    fn base_head(&self, base_branch: &str) -> Result<String, DaemonError>;
    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError>;
    /// Repo-scoped variant of [`base_head`](Vcs::base_head) (bead
    /// jleechan-wuts / issue #349). The factory-fabricated re-roll path
    /// (`reroll::execute` step 4) used to compute the new attempt's
    /// base SHA via `base_head(base_branch)` — which is bound at
    /// `main.rs` construction time to the daemon process's CWD
    /// (its systemd `WorkingDirectory`, the daemon's own source-repo
    /// checkout). When a bead's resolved `overlay.repo(cfg)` names a
    /// DIFFERENT repo (Stage A intake — the live failure for the 8jxr /
    /// 9rkz class), `git rev-parse <branch>` runs against the daemon's
    /// own repo's same-named branch (or fails outright), never against
    /// the routed target repo — silently wrong for any cross-repo
    /// bead. `repo` should always be `overlay.repo(cfg)`, not
    /// `cfg.target_repo` directly. Default impl ignores `repo` and
    /// delegates to `base_head` so existing test fakes and any impl that
    /// predates this method keep their original (single-repo) behavior;
    /// `CliVcs` overrides it to retarget via `gh api
    /// repos/<repo>/git/ref/heads/<branch>` (the same `gh api` plumbing
    /// `remote_head_sha` already uses).
    fn base_head_for_repo(&self, repo: &str, base_branch: &str) -> Result<String, DaemonError> {
        let _ = repo;
        self.base_head(base_branch)
    }
    /// Repo-scoped variant of [`create_branch_at`](Vcs::create_branch_at)
    /// (bead jleechan-wuts / issue #349). The factory-fabricated re-roll
    /// path (`reroll::execute` step 5) used to create the new attempt's
    /// branch via `create_branch_at(name, sha)` — which shells out to
    /// LOCAL `git branch <name> <sha>` in the daemon process's CWD
    /// (the daemon's own source-repo checkout). When a bead's resolved
    /// `overlay.repo(cfg)` names a DIFFERENT repo (the live failure
    /// for the 8jxr / 9rkz class), the new `factory/<bead>-r<n>` branch
    /// is created in the daemon's own repo, never in the routed target
    /// repo where the worker will actually push — meaning the worker's
    /// first `git push` either lands on a branch the daemon never made,
    /// or is forced to create its own branch out-of-band, depending on
    /// the branch-protection rules. `repo` should always be
    /// `overlay.repo(cfg)`, not `cfg.target_repo` directly. Default
    /// impl ignores `repo` and delegates to `create_branch_at` so
    /// existing test fakes and any impl that predates this method keep
    /// their original (single-repo) behavior; `CliVcs` overrides it to
    /// POST a `refs/heads/<name>` ref via `gh api repos/<repo>/git/refs`
    /// (cross-repo ref creation that does NOT depend on the daemon's
    /// local checkout at all).
    fn create_branch_at_for_repo(&self, repo: &str, name: &str, sha: &str) -> Result<(), DaemonError> {
        let _ = repo;
        self.create_branch_at(name, sha)
    }
    /// Repo-scoped variant of ref deletion (bead jleechan-znmh / issue #341,
    /// reroll idempotency on stale local `-rN` branches). When the daemon's
    /// routed-repo `create_branch_at_for_repo` POST fails with HTTP 422
    /// "Reference already exists" — meaning a PRIOR failed reroll attempt
    /// left a `factory/<bead>-r<n>` ref behind in the routed repo, even
    /// though the daemon's local checkout never created it — the reroll
    /// must delete that stale ref via this entry point and retry the
    /// create. Like `create_branch_at_for_repo`, this is a cross-repo
    /// `gh api` operation decoupled from the daemon's own cwd (the same
    /// shape as #349). Default impl is a no-op so existing single-repo
    /// test fakes keep their original behaviour transparently; `CliVcs`
    /// overrides it to `DELETE repos/<repo>/git/refs/heads/<name>`.
    fn delete_branch_at_for_repo(
        &self,
        repo: &str,
        name: &str,
    ) -> Result<(), DaemonError> {
        let _ = (repo, name);
        Ok(())
    }
    fn head_sha(&self, branch: &str) -> Result<String, DaemonError>;
    /// Budget-bounded [`head_sha`](Vcs::head_sha) (bead jleechan-zeij / issue
    /// #322 r4 P2). Default delegates to the unbounded method; the real
    /// `CliVcs` overrides to pass `timeout_secs` down to `git`.
    fn head_sha_within(&self, branch: &str, timeout_secs: u64) -> Result<String, DaemonError> {
        let _ = timeout_secs;
        self.head_sha(branch)
    }
    /// Repo-scoped, budget-bounded variant of [`head_sha_within`](Vcs::head_sha_within) (bead dark-factory-mw85).
    /// Default delegates to `head_sha_within` ignoring `repo`; `CliVcs` overrides it to query `gh api`.
    fn head_sha_within_for_repo(
        &self,
        repo: &str,
        branch: &str,
        timeout_secs: u64,
    ) -> Result<String, DaemonError> {
        let _ = repo;
        self.head_sha_within(branch, timeout_secs)
    }
    /// `true` iff `local_head` (the local branch's SHA) is a strict ancestor of
    /// `remote_sha` — i.e. the remote PR head contains every local commit AND
    /// has at least one extra commit the local checkout has not seen yet. Returns
    /// `false` when local and remote match, when remote is behind local, or when
    /// the two branches have diverged (a stronger condition than `head_sha(branch)
    /// != remote_sha`, which a divergent local checkout would also satisfy).
    ///
    /// This is the predicate the `jleechan-ubas` stall-bypass guard relies on:
    /// "the worker is still landing commits" must mean "remote has new work
    /// local hasn't fetched yet", not "the two sides have diverged". A
    /// divergent or local-only-ahead branch must NOT trigger the bypass —
    /// otherwise the daemon would mask a real stall behind a green PR and
    /// ignore local-only work.
    fn is_remote_ahead(&self, branch: &str, remote_sha: &str) -> Result<bool, DaemonError>;

    /// Fetch `branch` fresh from `origin` (read-only: this updates ONLY the
    /// remote-tracking ref `refs/remotes/origin/<branch>` — never a local
    /// branch, the index, or the working tree, so it is safe to call every
    /// tick) and return the resulting `origin/<branch>` HEAD SHA. Used both
    /// to capture the pre-remediation-session baseline before dispatching a
    /// coder session onto an adopted branch, and later to read the
    /// branch's current tip when verifying that baseline is still intact.
    fn remote_head_sha(&self, branch: &str) -> Result<String, DaemonError>;

    /// `true` iff `ancestor_sha` is an ancestor of `descendant_sha` in the
    /// local git commit graph (`git merge-base --is-ancestor`). Generic
    /// two-SHA form of the same merge-base primitive `is_remote_ahead`
    /// already uses internally; both SHAs' commit objects must already be
    /// present locally (call `remote_head_sha` first to guarantee that for
    /// SHAs coming from `origin`).
    ///
    /// This is the post-hoc append-only verification for bead
    /// jleechan-tfs1's hard operator law ("no force-push on adopted
    /// branches, ever"): the coder session that remediates an adopted PR
    /// runs as an independent subprocess the daemon does not control at the
    /// git layer, so "don't force-push" is a PROMPT-level instruction to
    /// that session, not something the daemon can structurally block the
    /// way it can block itself from calling `create_branch_at`/`close_pr`.
    /// This method is how the daemon checks, after the fact, whether the
    /// session complied.
    ///
    /// Unlike `is_remote_ahead` (where an inconclusive/error result should
    /// NOT false-positive a stall, since a missed stall just retries next
    /// tick for free), callers of `is_ancestor` for this force-push-
    /// detection use case MUST treat any non-`Ok(true)` result (`Ok(false)`
    /// OR `Err`) as "cannot confirm append-only — escalate to a human", not
    /// as "assume fine and continue". A missed force-push is silent,
    /// permanent history loss on a branch the daemon does not own; the cost
    /// of a false-positive escalation (a human reviews and clears it) is
    /// far lower than the cost of a false pass.
    fn is_ancestor(&self, ancestor_sha: &str, descendant_sha: &str) -> Result<bool, DaemonError>;

    /// Append-only remediation push for an ADOPTED branch (bead jleechan-tfs1):
    /// add exactly one new commit on top of `branch`'s current tip (fetched
    /// fresh from `origin`) and push it non-force to `origin/<branch>`.
    ///
    /// MUST NEVER rewrite history: no `git rebase`, no `--force`/`--force-with-lease`.
    /// An adopted branch belongs to an external contributor; the daemon has no
    /// authority to touch commits it didn't author. A non-fast-forward push
    /// rejection (the remote diverged since the daemon last looked — e.g. the
    /// contributor pushed more commits, or a genuine merge conflict with base
    /// exists) is a real error the caller MUST surface as "needs a human", and
    /// MUST NOT silently retry with `--force` or fall back to a rebase.
    fn push_fix_commit(&self, branch: &str, message: &str) -> Result<(), DaemonError>;
}

/// LLM judgment calls (router, in-place-vs-reroll verdict, constraint extraction).
/// ZFC: ALL judgment goes through here — no keyword/heuristic routing in callers.
pub trait Llm {
    fn judge(&self, prompt: &str) -> Result<String, DaemonError>;
    fn is_real(&self) -> bool {
        false
    }
}

/// Shared subprocess helper for every `Cli*` impl: spawn `cmd args...`, drain
/// stdout/stderr concurrently on dedicated reader threads (macOS/Linux pipe
/// buffers are ~64KB; without concurrent draining a child that writes more than
/// that blocks on `write()` forever and `try_wait` never observes an exit —
/// see bead jleechan-ac1), poll `try_wait` every 100ms, kill the child and
/// return `DaemonError::Timeout` if the deadline elapses first; non-zero exit
/// -> `DaemonError::Tool`; otherwise stdout as a `String`.
/// `run_tool` defaults to the daemon's own cwd (i.e. does NOT set
/// `current_dir`), matching the discipline the rest of the codebase already
/// relies on; `run_tool_in_dir` is the explicit-cwd variant used by code that
/// must run in a different working directory (e.g. the LLM fallback chain in
/// `adapters.rs::ChainLlm`, where the CWD matters for AGENTS.md / .claude/ to
/// be picked up — a stray `/tmp` cwd was the root cause of bead
/// `jleechan-g1k`).
pub fn run_tool(cmd: &str, args: &[&str], timeout_secs: u64) -> Result<String, DaemonError> {
    run_tool_with_cwd(cmd, args, None, &[], timeout_secs)
}

/// Explicit-cwd variant of `run_tool`. `cwd = None` leaves the child's cwd
/// unchanged (matches `run_tool`); `cwd = Some(path)` sets the child's cwd to
/// `path`. A failed `set_current_dir` on the child returns
/// `DaemonError::Tool` rather than silently inheriting the parent's cwd.
pub fn run_tool_in_dir(
    cmd: &str,
    args: &[&str],
    cwd: &str,
    timeout_secs: u64,
) -> Result<String, DaemonError> {
    run_tool_with_cwd(cmd, args, Some(cwd), &[], timeout_secs)
}

/// Like `run_tool`, but overlays extra environment variables on the child
/// only. Used by the `claudem` reviewer so MiniMax credentials never leak
/// into a sibling thread's Anthropic/`agy` dispatch.
pub fn run_tool_with_env(
    cmd: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
    timeout_secs: u64,
) -> Result<String, DaemonError> {
    run_tool_with_cwd(cmd, args, None, extra_env, timeout_secs)
}

fn run_tool_with_cwd(
    cmd: &str,
    args: &[&str],
    cwd: Option<&str>,
    extra_env: &[(&str, &str)],
    timeout_secs: u64,
) -> Result<String, DaemonError> {
    // Centralized GitHub rate-limit circuit-breaker admission check.
    crate::gh_circuit_breaker::admit_or_suppress(cmd)?;

    let res = (|| {
        let mut command = Command::new(cmd);
        if cmd == "br" {
            if let Ok(db) = std::env::var("DARK_FACTORY_BR_DB") {
                command.args(["--db", db.as_str()]);
            }
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Reviewer CLIs such as Codex spawn helper processes. Give every tool
        // invocation a dedicated process group so a timeout cannot leave those
        // helpers running after their direct parent has been killed.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| DaemonError::Tool {
                tool: cmd.to_string(),
                rc: -1,
                stderr: format!("spawn failed: {e}"),
            })?;

        // Take the pipes and hand them to dedicated reader threads immediately so
        // they drain concurrently with the wait/poll loop below. Readers run to
        // EOF, which naturally occurs once the child exits (or is killed) and its
        // pipe ends close — they never block the timeout path.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        });
        let stderr_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        });

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let poll_interval = Duration::from_millis(100);

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        #[cfg(unix)]
                        {
                            // POSIX kill accepts a negative PID to signal the
                            // process group created above. This leaves the
                            // daemon's own group untouched.
                            unsafe {
                                unix_signals::kill(-(child.id() as unix_signals::Pid), unix_signals::SIGKILL);
                            }
                        }
                        #[cfg(not(unix))]
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(DaemonError::Timeout(format!(
                            "{cmd} exceeded {timeout_secs}s timeout"
                        )));
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(e) => {
                    break Err(DaemonError::Tool {
                        tool: cmd.to_string(),
                        rc: -1,
                        stderr: format!("try_wait failed: {e}"),
                    });
                }
            }
        };

        // Join the readers regardless of outcome: once the child has exited (or
        // been killed) its pipe fds close, so `read_to_end` returns promptly.
        let stdout_buf = stdout_reader.join().unwrap_or_default();
        let stderr_buf = stderr_reader.join().unwrap_or_default();

        let status = status?;

        let stdout = String::from_utf8_lossy(&stdout_buf).into_owned();
        if status.success() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&stderr_buf).into_owned();
        Err(DaemonError::Tool {
            tool: cmd.to_string(),
            rc: status.code().unwrap_or(-1),
            stderr,
        })
    })();

    crate::gh_circuit_breaker::record_result(cmd, &res);
    res
}

#[cfg(test)]
mod remote_url_matches_repo_tests {
    use super::{remote_url_for_display, remote_url_matches_repo};

    #[test]
    fn https_url_with_dot_git_suffix_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "https://github.com/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    #[test]
    fn https_url_without_dot_git_suffix_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "https://github.com/jleechanorg/dark-factory",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    #[test]
    fn ssh_style_url_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "git@github.com:jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    #[test]
    fn ssh_style_url_without_dot_git_suffix_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "git@github.com:jleechanorg/dark-factory",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    #[test]
    fn full_ssh_scheme_url_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "ssh://git@github.com/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    /// Adversarial review finding: `ssh://` URLs may carry an explicit port
    /// (e.g. when a firewall forces SSH-over-443). Must still resolve, not
    /// silently fall through to "unrecognized".
    #[test]
    fn full_ssh_scheme_url_with_explicit_port_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "ssh://git@github.com:22/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    /// Adversarial review finding: a credential/token-embedded HTTPS remote
    /// (`https://x-access-token:ghp_xxx@github.com/owner/repo.git`, a common
    /// CI-injected form) must be recognized, not misclassified as
    /// "unrecognized" (which would previously have collapsed to a false
    /// positive mismatch).
    #[test]
    fn https_url_with_embedded_credentials_matches() {
        assert_eq!(
            remote_url_matches_repo(
                "https://x-access-token:ghp_abc123@github.com/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    #[test]
    fn remote_display_never_exposes_embedded_credentials() {
        const SECRET: &str = "SYNTHETIC_REMOTE_CREDENTIAL_SENTINEL";
        let remote = format!("https://user:{SECRET}@github.com/owner/repo.git");

        let displayed = remote_url_for_display(&remote);

        assert_eq!(displayed, "<redacted-git-remote>");
        assert!(!displayed.contains(SECRET));
    }

    #[test]
    fn wrong_repo_is_a_confirmed_mismatch() {
        // The exact wa-3086 near-miss this bead exists to catch: a
        // dual-remote worldarchitect.ai worktree whose `origin` points at
        // jleechanclaw instead of worldarchitect.ai. This IS a recognized
        // github.com URL, so it must be a definite `Some(false)`, not `None`.
        assert_eq!(
            remote_url_matches_repo(
                "https://github.com/jleechanorg/jleechanclaw.git",
                "jleechanorg/worldarchitect.ai"
            ),
            Some(false)
        );
    }

    #[test]
    fn trailing_slash_is_tolerated() {
        assert_eq!(
            remote_url_matches_repo(
                "https://github.com/jleechanorg/dark-factory/",
                "jleechanorg/dark-factory"
            ),
            Some(true)
        );
    }

    /// Adversarial review finding (CONFIRMED bug in the original
    /// implementation): an unrecognized host (a different git host,
    /// including GitHub Enterprise) must be `None` ("cannot determine"),
    /// NEVER a confirmed mismatch — the caller in `dispatch::dispatch_ready`
    /// only kills a session on a positively confirmed mismatch, and treating
    /// every unparseable URL as "confirmed wrong" would kill perfectly
    /// correct sessions that merely use a URL flavor this normalizer doesn't
    /// know about.
    #[test]
    fn unrecognized_host_is_indeterminate_not_a_confirmed_mismatch() {
        assert_eq!(
            remote_url_matches_repo(
                "https://gitlab.com/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            None
        );
    }

    #[test]
    fn github_enterprise_host_is_indeterminate_not_a_confirmed_mismatch() {
        assert_eq!(
            remote_url_matches_repo(
                "https://github.enterprise.example.com/jleechanorg/dark-factory.git",
                "jleechanorg/dark-factory"
            ),
            None
        );
    }

    #[test]
    fn empty_url_is_indeterminate() {
        assert_eq!(
            remote_url_matches_repo("", "jleechanorg/dark-factory"),
            None
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn run_tool_success_captures_stdout() {
        let out = run_tool("true", &[], 5).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_nonzero_exit_is_tool_error() {
        let err = run_tool("false", &[], 5).unwrap_err();
        match err {
            DaemonError::Tool { rc, .. } => assert_eq!(rc, 1),
            other => panic!("expected Tool error, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_timeout_kills_child() {
        let err = run_tool("sleep", &["2"], 1).unwrap_err();
        assert!(
            matches!(err, DaemonError::Timeout(_)),
            "expected Timeout, got {err:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_timeout_kills_spawned_descendants() {
        let pid_file = std::env::temp_dir().join(format!(
            "dark_factory_run_tool_descendant_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pid_file_arg = pid_file.to_string_lossy().into_owned();
        let err = run_tool(
            "sh",
            &[
                "-c",
                "sleep 30 >/dev/null 2>&1 & echo $! > \"$1\"; wait",
                "sh",
                &pid_file_arg,
            ],
            1,
        )
        .unwrap_err();
        assert!(matches!(err, DaemonError::Timeout(_)), "got {err:?}");

        let pid = std::fs::read_to_string(&pid_file)
            .expect("timed-out child must record its descendant PID")
            .trim()
            .to_owned();
        let pid: unix_signals::Pid = pid.parse().expect("descendant PID must be numeric");
        let is_alive = process_is_live(pid);
        if is_alive {
            unsafe {
                unix_signals::kill(pid, unix_signals::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&pid_file);
        assert!(
            !is_alive,
            "timeout must terminate descendant PID {pid}, not only its direct child"
        );
    }

    #[cfg(target_os = "linux")]
    fn process_is_live(pid: unix_signals::Pid) -> bool {
        // Linux reports an unreaped zombie as signalable via kill(pid, 0),
        // even though it cannot execute. A timed-out descendant is correctly
        // terminated once it reaches Z; PID 1 owns reaping it thereafter.
        let stat_path = format!("/proc/{pid}/stat");
        match std::fs::read_to_string(stat_path) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .and_then(|(_, rest)| rest.chars().next())
                .map(|state| state != 'Z')
                .unwrap_or_else(|| unsafe { unix_signals::kill(pid, 0) == 0 }),
            Err(_) => false,
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn process_is_live(pid: unix_signals::Pid) -> bool {
        unsafe { unix_signals::kill(pid, 0) == 0 }
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_echo_captures_output() {
        let out = run_tool("echo", &["hello"], 5).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    /// Regression test for bead jleechan-ac1: a child writing more than one
    /// pipe buffer's worth of output (~64KB on macOS) must not deadlock.
    /// Without concurrent draining, the child blocks on `write()` once the
    /// stdout pipe fills, `try_wait` never observes an exit, and `run_tool`
    /// hangs until the timeout kills it — losing the output in the process.
    #[test]
    #[cfg(unix)]
    fn run_tool_large_output_does_not_deadlock() {
        const WANT_BYTES: usize = 200_000; // well over the ~64KB pipe buffer
        let out = run_tool("sh", &["-c", &format!("yes | head -c {WANT_BYTES}")], 10)
            .expect("run_tool should complete without hanging on large output");
        assert_eq!(
            out.len(),
            WANT_BYTES,
            "expected full {WANT_BYTES} bytes of output to be captured, got {}",
            out.len()
        );
    }

    fn make_tree(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), b"").unwrap();
        std::fs::write(root.join("README.md"), b"").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
    }

    #[test]
    fn summarize_file_tree_lists_files_and_dirs_skips_dotfiles() {
        let dir = std::env::temp_dir().join(format!("afd_file_tree_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        make_tree(&dir);

        let summary = summarize_file_tree(&dir, 100);

        assert!(summary.contains("README.md"), "summary: {summary:?}");
        assert!(summary.contains("src/"), "summary: {summary:?}");
        assert!(summary.contains("src/main.rs"), "summary: {summary:?}");
        assert!(
            !summary.contains(".git"),
            "dotfiles/dot-dirs must be skipped: {summary:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn summarize_file_tree_caps_at_max_entries() {
        let dir =
            std::env::temp_dir().join(format!("afd_file_tree_cap_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..20 {
            std::fs::write(dir.join(format!("file-{i:02}.txt")), b"").unwrap();
        }

        let summary = summarize_file_tree(&dir, 5);
        let line_count = summary.lines().count();

        assert_eq!(
            line_count, 5,
            "must cap at max_entries even with more files present: {summary:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn summarize_file_tree_missing_root_is_empty_not_error() {
        let missing = std::env::temp_dir().join("afd_definitely_does_not_exist_xyz");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(summarize_file_tree(&missing, 50), "");
    }

    #[test]
    fn summarize_file_tree_zero_max_entries_is_empty() {
        let dir = std::env::temp_dir();
        assert_eq!(summarize_file_tree(&dir, 0), "");
    }

    /// Regression test for `run_tool_in_dir` (added in beads qdw/g1k land):
    /// the explicit `cwd` argument MUST actually be the child's cwd, not be
    /// silently swallowed. Without the assertion below, a future refactor
    /// that drops the `current_dir` call would be invisible to tests until
    /// someone observed the LLM fallback chain losing its project context
    /// (the exact failure mode that produced bead `jleechan-g1k`).
    #[test]
    #[cfg(unix)]
    fn run_tool_in_dir_sets_child_cwd() {
        let tmp = std::env::temp_dir().join(format!(
            "afd_run_tool_in_dir_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        // Run `pwd` in the tmp dir — if `current_dir` is honored, the output
        // is the canonicalized tmp path; if it is dropped, we get the daemon's
        // cwd which is something else under `cargo test`.
        let out = run_tool_in_dir("pwd", &[], tmp.to_str().unwrap(), 5).unwrap();
        assert!(
            std::path::Path::new(out.trim())
                .canonicalize()
                .unwrap()
                == std::path::Path::new(tmp.to_str().unwrap())
                    .canonicalize()
                    .unwrap(),
            "run_tool_in_dir must set the child's cwd to the explicit value; \
             output of `pwd` was {out:?}, expected canonical of {tmp:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Regression test for `run_tool` (no-cwd variant): the child MUST inherit
    /// the parent's cwd. If a future refactor accidentally sets
    /// `current_dir(".")` or similar on the no-cwd path, the canonical-path
    /// resolution here would still pass but the no-cwd contract would not be
    /// tested — keep both tests paired.
    #[test]
    #[cfg(unix)]
    fn run_tool_inherits_parent_cwd() {
        let parent_cwd = std::env::current_dir().unwrap();
        let out = run_tool("pwd", &[], 5).unwrap();
        assert_eq!(
            std::path::Path::new(out.trim()).canonicalize().unwrap(),
            parent_cwd.canonicalize().unwrap(),
            "run_tool must NOT change the child's cwd; got {out:?}, parent cwd {parent_cwd:?}"
        );
    }

    // Bead jleechan-jw4c: the cwd guard rejects a worker whose actual cwd
    // does not match the daemon's expected cwd. The guard is silent (Ok)
    // when the expected cwd is `None` (legacy layout) and FAIL CLOSED when
    // a non-empty expected cwd does not match the actual.

    #[test]
    fn cwd_guard_passes_when_expected_is_none() {
        let result = check_cwd_guard(None, std::path::Path::new("/tmp"));
        assert!(result.is_ok());
    }

    #[test]
    fn cwd_guard_passes_when_paths_match_after_canonicalize() {
        let dir = std::env::temp_dir().join(format!("afd_cwd_match_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let result = check_cwd_guard(Some(&dir), &dir);
        assert!(result.is_ok(), "matching cwds must pass the guard");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cwd_guard_fails_closed_when_paths_differ() {
        let expected = std::env::temp_dir().join(format!("afd_cwd_expected_{}", std::process::id()));
        let actual = std::env::temp_dir().join(format!("afd_cwd_actual_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&expected);
        let _ = std::fs::remove_dir_all(&actual);
        std::fs::create_dir_all(&expected).unwrap();
        std::fs::create_dir_all(&actual).unwrap();
        let err = check_cwd_guard(Some(&expected), &actual).unwrap_err();
        match err {
            DaemonError::WorktreeCwdMismatch { expected: e, actual: a } => {
                assert!(e.contains("afd_cwd_expected"));
                assert!(a.contains("afd_cwd_actual"));
            }
            other => panic!("expected WorktreeCwdMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&expected);
        let _ = std::fs::remove_dir_all(&actual);
    }
}
