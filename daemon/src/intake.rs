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
use crate::state::now_iso8601;
use crate::telemetry::{self, TelemetryEvent};
use crate::tools::{Bead, LabeledPr, Permission, Scm, Tracker};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FACTORY_LABEL: &str = "factory";
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct CacheFileLock {
    path: PathBuf,
    file: File,
}

impl CacheFileLock {
    fn acquire(cache_path: &Path) -> Result<Self, DaemonError> {
        let lock_path = cache_path.with_extension("json.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| DaemonError::Config(format!("open adoption_probe_cache lock: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_EX: i32 = 2;
            // flock blocks until the kernel-owned lock is available and is
            // released automatically if the writer process exits.
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
            if result != 0 {
                return Err(DaemonError::Config(format!(
                    "acquire adoption_probe_cache lock: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(Self { path: lock_path, file })
    }
}

impl Drop for CacheFileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_UN: i32 = 8;
            let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
        }
        // Keep the lock inode persistent. Removing it would let a waiting
        // contender open a different inode and bypass an owner that still
        // holds the kernel lock.
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

// jtg8-r4 =====================================================================
//
// Rate-limit-aware PR intake + adoption-probe cache. The slow tier
// `run_slow_tier` invokes `normalize_labeled_prs_outcome` (instead of the
// legacy `normalize_labeled_prs`) and gets back a `LabeledPrsIntakeOutcome`
// with three important signals:
//
//  - `rate_limited`: true iff `gh pr list` returned a 403/API-rate-limit
//    error. The slow tier logs a one-line skip and CONTINUES into routing +
//    dispatch (the r3 fix used `return Ok(())` which starved the rest of
//    run_slow_tier — bead jleechan-jtg8, 2026-07-22 skeptic brief).
//  - `metrics.gh_call_count`: per-pass counter of real `gh` invocations,
//    surfaced for telemetry WARN at threshold.
//  - `metrics.probe_cache_hits/misses`: per-pass counter for cache health.
//
// The on-disk `AdoptionProbeCache` is keyed on
// `(external_ref, head_sha, updated_at_epoch)` and persists in the daemon's
// runtime state directory (`$DARK_FACTORY_STATE_DIR` or `~/.dark-factory`).
// Cache invalidation:
//   - any change to head_sha or updated_at_epoch → cache miss (re-probe)
//   - collaborator tier change between probes → cache miss (r4 fix vs r3's
//     stale Read→Write promotion bug)
//   - daemon restart → file persists, hits survive warm restart
//
// Best-effort: a corrupt or missing file means cold cache (probes fresh
// every tick), preserving pre-fix behavior on schema drift.

/// Per-tick gh call counter + cache-health signals. Reset to zero at the
/// start of every slow-tier pass; carried back to the caller so telemetry
/// can emit `INTAKE_GH_CALLS_EXCEEDED` when count exceeds the threshold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntakeProbeMetrics {
    /// Total gh CLI invocations this pass (list query + per-PR probes +
    /// REST fallbacks). Counts REAL subprocess invocations — every
    /// increment corresponds to a `scm.*` call that crossed the daemon's
    /// tool boundary.
    pub gh_call_count: u32,
    /// Number of per-PR decisions served from the adoption-probe cache
    /// without a fresh `gh` call.
    pub probe_cache_hits: u32,
    /// Number of per-PR probes that fell through to a fresh
    /// `collaborator_permission` call (cache miss or invalidation).
    pub probe_cache_misses: u32,
    /// Number of times this pass was rate-limited (gh 403). Exactly one
    /// per slow-tier call to `normalize_labeled_prs_outcome` even on the
    /// `labeled_prs` list-query path; zero for all other code paths.
    pub rate_limited_skips: u32,
}

/// Outcome of a `normalize_labeled_prs_outcome` call. Carries the
/// adopted/outcomes vectors plus the rate-limit flag and metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledPrsIntakeOutcome {
    pub adopted: Vec<ExistingPrIntake>,
    pub outcomes: Vec<IntakeOutcome>,
    /// True iff the upstream `gh pr list` returned a rate-limit (403). The
    /// slow tier must CONTINUE running (don't abort routing/dispatch) when
    /// this is true; treat the empty `adopted`/`outcomes` as "degraded for
    /// this tick, retry next tick" rather than as a failure.
    pub rate_limited: bool,
    pub metrics: IntakeProbeMetrics,
}

/// Cache key for one PR's adoption-probe decision. The triple
/// `(external_ref, head_sha, updated_at_epoch)` invalidates when any of the
/// three change — `head_sha` for new commits, `updated_at_epoch` for body /
/// comment edits, `external_ref` for owner/repo renames (defensive; rare).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ProbeCacheKey {
    pub external_ref: String,
    pub head_sha: Option<String>,
    pub updated_at_epoch: Option<u64>,
}

impl ProbeCacheKey {
    pub fn from_pr(pr: &LabeledPr) -> Self {
        Self {
            external_ref: pr.external_ref.clone(),
            head_sha: pr.head_sha.clone(),
            updated_at_epoch: pr.updated_at_epoch,
        }
    }

    /// jtg8-r5 (codex P2 "Avoid caching PRs without complete cache keys"):
    /// a cache key is only usable for invalidation when ALL three fields
    /// are populated. Any `None` field means "the upstream `gh pr list`
    /// (or REST fallback) didn't return enough information to detect
    /// changes" — caching under such a key would silently replay a stale
    /// decision across ticks because we have no signal to invalidate on.
    /// The r4 cache stored entries with `None` fields; the r5 fix gates
    /// `cache.insert` on this predicate so incomplete-key PRs are
    /// re-probed every tick (preserving pre-r4 behavior on schema-drifted
    /// upstream payloads).
    pub fn is_complete(&self) -> bool {
        self.head_sha.is_some() && self.updated_at_epoch.is_some()
    }
}

/// Cache entry — one row per (PR, cache-key) the slow tier has probed.
/// Stores the *decision* made about that PR, not the raw gh responses, so
/// future ticks can replay it with zero gh calls. The `cached_at_epoch`
/// timestamp drives the r4 collaborator-change invalidation path: a cached
/// `AuthorPermission` entry older than `MAX_CACHED_PERMISSION_AGE_SECS` is
/// re-probed unconditionally on the next tick (the contributor's tier
/// could have been promoted/demoted in the meantime).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CachedProbeDecision {
    pub decision: CachedDecisionKind,
    /// Epoch seconds when this decision was recorded. The r4 invalidation
    /// path re-probes any `AuthorPermission` decision older than
    /// `MAX_CACHED_PERMISSION_AGE_SECS` so a Read → Write promotion
    /// surfaces within one TTL window instead of replaying the stale skip
    /// forever.
    pub cached_at_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CachedDecisionKind {
    AuthorPermission(Permission),
}

/// r4: cached `AuthorPermission` entries are re-probed after this TTL.
/// Smaller than the r3 cache's effective lifetime (forever) but larger than
/// the slow tick interval (60s) so a steady-state tick still hits cache.
/// Chosen to bound the worst-case stale-skip window after a contributor
/// promotion: 1 hour. Matches the launchd watchdog's recovery cadence.
pub const MAX_CACHED_PERMISSION_AGE_SECS: u64 = 3_600;

#[derive(Debug, Clone)]
pub struct AdoptionProbeCache {
    /// keyed by `ProbeCacheKey`. The hash impl treats `(Some, Some)` as a
    /// distinct key from `(Some, None)` so PRs whose `updated_at_epoch`
    /// hasn't been populated yet (REST-fallback with schema drift) hash
    /// independently — `None` keys still get cached, just separately from
    /// `Some` keys.
    decisions: std::collections::HashMap<ProbeCacheKey, CachedProbeDecision>,
    path: PathBuf,
}

/// Mutable state belongs to the daemon runtime directory, not the immutable
/// installed release or a target repository checkout.
pub fn runtime_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DARK_FACTORY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".dark-factory");
    }
    std::env::temp_dir().join("dark-factory")
}

pub fn adoption_probe_cache_path() -> PathBuf {
    runtime_state_dir().join("adoption_probe_cache.json")
}

impl Default for AdoptionProbeCache {
    fn default() -> Self {
        Self {
            decisions: std::collections::HashMap::new(),
            path: adoption_probe_cache_path(),
        }
    }
}

impl AdoptionProbeCache {
    /// Construct an empty cache (no persistence). Use
    /// `load_or_default` for the disk-backed variant.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_or_default_at(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                return Self {
                    decisions: std::collections::HashMap::new(),
                    path,
                }
            }
        };
        match serde_json::from_str::<Vec<(ProbeCacheKey, CachedProbeDecision)>>(&raw) {
            Ok(entries) => Self {
                decisions: entries.into_iter().collect(),
                path,
            },
            Err(_) => Self {
                decisions: std::collections::HashMap::new(),
                path,
            },
        }
    }

    /// Load the cache from the runtime-state cache path if present;
    /// otherwise return an empty cache. Never errors out — a corrupt or
    /// absent file just means "cold cache, probe everything" (the pre-fix
    /// behavior).
    pub fn load_or_default() -> Self {
        Self::load_or_default_at(adoption_probe_cache_path())
    }

    /// Persist the cache to the runtime-state cache path. Best-effort:
    /// writers serialize through a same-directory lock, reload and merge the
    /// current file, then atomically rename a sibling temporary file, so a crash mid-write
    /// leaves either the old or the new file intact (never a half-written
    /// file the next daemon boot would fail to parse).
    pub fn persist(&self) -> Result<(), DaemonError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                DaemonError::Config(format!("create adoption_probe_cache directory: {error}"))
            })?;
        }
        let _lock = CacheFileLock::acquire(&self.path)?;
        let mut merged = std::collections::HashMap::new();
        if let Ok(raw) = std::fs::read_to_string(&self.path) {
            if let Ok(entries) =
                serde_json::from_str::<Vec<(ProbeCacheKey, CachedProbeDecision)>>(&raw)
            {
                merged.extend(entries);
            }
        }
        merged.extend(self.decisions.iter().map(|(key, value)| (key.clone(), value.clone())));
        let entries: Vec<(ProbeCacheKey, CachedProbeDecision)> = merged.into_iter().collect();
        let json = serde_json::to_string(&entries)
            .map_err(|error| DaemonError::Parse(format!("serialize adoption_probe_cache: {error}")))?;
        let nonce = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.path.with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            nonce
        ));
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| DaemonError::Config(format!("write adoption_probe_cache: {e}")))?;
        if let Err(error) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(DaemonError::Config(format!(
                "rename adoption_probe_cache tmp->final: {error}"
            )));
        }
        Ok(())
    }

    /// True iff the cache has a stored decision for this exact key AND the
    /// decision is fresh enough (r4: < MAX_CACHED_PERMISSION_AGE_SECS old).
    /// Callers MUST consult this BEFORE invoking any gh-side per-PR probe.
    pub fn contains_fresh(&self, key: &ProbeCacheKey, now_epoch: u64) -> bool {
        match self.decisions.get(key) {
            Some(entry) => match &entry.decision {
                CachedDecisionKind::AuthorPermission(_) => {
                    now_epoch.saturating_sub(entry.cached_at_epoch)
                        <= MAX_CACHED_PERMISSION_AGE_SECS
                }
            },
            None => false,
        }
    }

    /// Pure key-presence check, no TTL applied. Used by unit tests to
    /// prove cache-key changes invalidate stored entries without
    /// re-validating freshness (the r4 TTL path is exercised in the
    /// integration tests where `now_epoch` is plumbed through).
    #[allow(dead_code)]
    pub fn contains(&self, key: &ProbeCacheKey) -> bool {
        self.decisions.contains_key(key)
    }

    /// Return the cached decision for `key`, regardless of freshness.
    /// Mirrors `contains_fresh` but skips the TTL check — used by tests
    /// and by the eviction helper.
    pub fn get(&self, key: &ProbeCacheKey) -> Option<&CachedProbeDecision> {
        self.decisions.get(key)
    }

    /// Record a decision. Overwrites any prior entry for this key
    /// (idempotent within a single tick — `normalize_labeled_prs_with_cache`
    /// writes the entry only once per PR per pass).
    ///
    /// jtg8-r5 (codex P2 "Avoid caching PRs without complete cache keys"):
    /// returns `false` and leaves the cache untouched when `key` has any
    /// `None` field. The caller treats the insert as a no-op so the next
    /// tick falls through to a fresh probe (preserves pre-r4 behavior on
    /// schema-drifted upstream payloads that don't carry `head_sha` /
    /// `updated_at_epoch`). Returns `true` on a real insert so callers /
    /// tests can distinguish the two paths.
    pub fn insert(
        &mut self,
        key: ProbeCacheKey,
        decision: CachedDecisionKind,
        now_epoch: u64,
    ) -> bool {
        if !key.is_complete() {
            return false;
        }
        self.decisions.insert(
            key,
            CachedProbeDecision {
                decision,
                cached_at_epoch: now_epoch,
            },
        );
        true
    }

    /// Drop entries older than `max_age_secs` from the cache. Defensive —
    /// caps the cache file size in case PRs accumulate and never get
    /// cleaned up (e.g. closed PRs whose cache entries would otherwise
    /// linger forever). Currently unused by the slow tier but exposed for
    /// ops use.
    #[allow(dead_code)]
    pub fn evict_older_than(&mut self, now_epoch: u64, max_age_secs: u64) {
        self.decisions.retain(|_, entry| {
            now_epoch.saturating_sub(entry.cached_at_epoch) <= max_age_secs
        });
    }

    /// For tests / observability: number of cached decisions.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.decisions.len()
    }

    /// Sister helper for `len` so clippy::len_without_is_empty is silent.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPrIntake {
    pub bead_id: String,
    pub pr_number: u64,
    pub head_ref_name: String,
    pub external_ref: String,
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
/// 1. An explicit `target_repo: <owner>/<name>` field in the bead body —
///    the existing `/auto-factory` drive-existing-pr protocol grammar (see
///    `.claude/skills/auto-factory/SKILL.md` §1c, alongside
///    `existing_branch:`/`existing_pr:`). Applies identically to
///    daemon-created beads and beads created manually via `br`.
/// 2. Else the `owner/repo` prefix of the bead's `external_ref`
///    (`"<owner>/<repo>#<number>"`).
/// 3. Else `None` — legacy/manual bead with no way to determine its repo;
///    callers fall back to the daemon's global `cfg.target_repo` via
///    [`crate::state::BeadOverlay::repo`].
///
/// This is the single call site for body-field/external_ref repo-identity
/// parsing — do not re-implement this precedence elsewhere.
pub fn resolve_target_repo(body: &str, external_ref: Option<&str>) -> Option<String> {
    if let Some(explicit) = parse_target_repo_body_field(body) {
        return Some(explicit);
    }
    external_ref
        .and_then(parse_owner_repo_from_external_ref)
        .map(|(owner_repo, _issue)| owner_repo)
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

/// jleechan-r28r: canonicalize a `LabeledPr`/`Issue` `external_ref` to the
/// short `owner/repo#N` form BEFORE it enters the intake dedup/lookup/create
/// pipeline. Accepts both the short form (returned unchanged) and the
/// full GitHub URL form (`https://github.com/<owner>/<repo>/pull|N` →
/// `owner/repo#N`); any other shape is returned unchanged so legacy/malformed
/// data is preserved for the existing parse-time error path (jleechan-mdgr's
/// defense-in-depth for the `local-` suffix, etc.) — the contract is
/// strictly "best-effort normalize, never invent data".
///
/// Without this, two intake events for the same PR (one URL-shaped, one
/// short-shaped) bypass `br`'s string-equal uniqueness check and produce the
/// live duplicate-pair thrash (e.g. jleechan-jpi vs jleechan-hslx for
/// PR #8058) that fed the 390x/15min ESCALATION_NOTIFICATION_FAILED burst.
/// Once normalized at intake, every downstream comparison
/// (`known_refs.contains`, `tracker_candidates.iter().find(..., pr.external_ref)`,
/// `tracker.create_bead` uniqueness, the `SkippedDuplicate` outcome's
/// `external_ref` field) operates on a single canonical key per PR/issue.
fn to_canonical_external_ref(external_ref: &str) -> String {
    if let Some((owner_repo, num)) = parse_owner_repo_from_external_ref(external_ref) {
        return format!("{owner_repo}#{num}");
    }
    // GitHub URL form: `https://github.com/<owner>/<repo>/(pull|issues)/<n>`.
    // Mirrors `adapters::parse_external_ref`'s URL branch; we don't share
    // that helper because adapters keeps it private and this module
    // deliberately keeps its own copy of the parse logic (see the
    // `parse_owner_repo_from_external_ref` comment above).
    if let Some(rest) = external_ref.strip_prefix("https://github.com/") {
        let segments: Vec<&str> = rest.split('/').collect();
        if let [owner, repo, kind, number] = segments.as_slice() {
            if matches!(*kind, "pull" | "issues") && !owner.is_empty() && !repo.is_empty() && !number.is_empty() {
                return format!("{owner}/{repo}#{number}");
            }
        }
    }
    external_ref.to_string()
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

fn same_repo_pr(pr: &LabeledPr, target_repo: &str) -> bool {
    if pr.is_cross_repository {
        return false;
    }
    if let Some(head_repo) = pr.head_repo_full_name.as_deref() {
        return head_repo.eq_ignore_ascii_case(target_repo);
    }
    if let Some(head_owner) = pr.head_repo_owner_login.as_deref() {
        let target_owner = target_repo.split('/').next().unwrap_or_default();
        return head_owner.eq_ignore_ascii_case(target_owner);
    }
    true
}

pub const MAX_INTAKE_REPOS_PER_SWEEP: usize = 10;
pub const MAX_INTAKE_SWEEP_GH_CALLS: u32 = 100;

/// Case-insensitive-deduped, UNBOUNDED repository universe for this
/// config: `target_repo` first, then every `cfg.repos` key that does not
/// identify the same GitHub repository (case-insensitively — GitHub
/// owner/repo identity is case-insensitive) as `target_repo` or an
/// earlier entry, sorted for determinism. Original casing is preserved in
/// the returned strings (used verbatim for SCM/API calls and
/// `external_ref` construction) — only the *comparison* is case-folded.
///
/// PR #629 follow-up fix (codex P2 "Deduplicate repository names
/// case-insensitively" + CodeRabbit convergent finding): the previous
/// `*r != &cfg.target_repo` filter was case-SENSITIVE, unlike every other
/// repository comparison on this path (`same_repo_pr`, the default
/// `labeled_prs_for_repo` filter in `tools.rs`) — if `cfg.repos` held the
/// target repository under a different ASCII case, the filter didn't
/// remove it, the sweep scanned the same repository twice, and every PR
/// in it produced duplicate `IntakeOutcome`s and duplicate
/// `ExistingPrIntake` adoptions in one tick.
///
/// Callers apply `MAX_INTAKE_REPOS_PER_SWEEP` themselves:
/// `target_repositories_sweep_order` truncates the ROTATED list;
/// `normalize_labeled_prs_outcome` also calls this directly (unbounded)
/// to detect and report truncation.
fn dedup_repo_universe(cfg: &Config) -> Vec<String> {
    let mut secondary: Vec<String> = cfg.repos.keys().cloned().collect();
    secondary.sort();
    let mut seen_lower: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen_lower.insert(cfg.target_repo.to_ascii_lowercase());
    let mut universe = vec![cfg.target_repo.clone()];
    for repo in secondary {
        if seen_lower.insert(repo.to_ascii_lowercase()) {
            universe.push(repo);
        }
    }
    universe
}

/// Bounded, deterministic-within-a-tick, ROTATING sweep order. PR #629
/// follow-up fix (codex P1 "Rotate repositories instead of permanently
/// truncating them"): the pre-fix version always kept `target_repo` plus
/// the same first `MAX_INTAKE_REPOS_PER_SWEEP - 1` secondary repos
/// (alphabetically) and discarded the rest FOREVER — with no cursor or
/// rotation state anywhere in the daemon, factory-labeled PRs in the
/// discarded repos were never scanned on ANY tick.
///
/// `target_repo` always scans first, every tick (it is the daemon's
/// primary configured repo). The SECONDARY window rotates by one repo per
/// slow-tier sweep — the rotation offset advances with
/// `now_epoch / cfg.slow_tick_secs`, so it increments by exactly 1 each
/// time a real slow-tier tick calls this with a fresh `now_epoch` — so
/// every secondary repo eventually enters the scanned window and full
/// coverage is guaranteed within `secondary.len()` sweeps, while the
/// order within any SINGLE tick stays fully deterministic (same
/// `now_epoch` in, same order out).
pub fn target_repositories_sweep_order(cfg: &Config, now_epoch: u64) -> Vec<String> {
    let mut universe = dedup_repo_universe(cfg);
    let secondary_len = universe.len().saturating_sub(1);
    if secondary_len > 0 {
        let period = cfg.slow_tick_secs.max(1);
        let rotation = ((now_epoch / period) as usize) % secondary_len;
        universe[1..].rotate_left(rotation);
    }
    if universe.len() > MAX_INTAKE_REPOS_PER_SWEEP {
        universe.truncate(MAX_INTAKE_REPOS_PER_SWEEP);
    }
    universe
}

/// jtg8-r4: rate-limit-aware variant of `normalize_labeled_prs`. Detects a
/// `gh` rate-limit on the upstream `labeled_prs` list query and returns
/// `rate_limited = true` with empty `adopted`/`outcomes` instead of
/// propagating `Err`. The slow tier treats this as a degraded pass (no
/// `consecutive_failures` increment, NO early-return from run_slow_tier —
/// the routing + dispatch work for already-QUEUED beads from prior ticks
/// continues through the 403 window).
///
/// The per-PR decisions are delegated to `normalize_labeled_prs_with_cache`,
/// which gates every per-PR `collaborator_permission` call on
/// `AdoptionProbeCache.contains_fresh()`. When the cache is warm (key
/// unchanged and decision fresh), the cached decision is replayed and ZERO
/// gh calls are made for that PR. When the cache misses or the entry is
/// stale (older than `MAX_CACHED_PERMISSION_AGE_SECS`), a fresh probe is
/// made and the new decision cached.
///
/// `now_epoch` is the daemon's current epoch seconds, used to record the
/// cache timestamp for the r4 TTL-based invalidation.
///
/// `telemetry_log` receives one `INTAKE_REPO_SWEEP_FAILED` event per
/// per-repo isolation point (hard SCM error OR rate-limit skip) — PR #629
/// follow-up fix: previously these were `eprintln!`-only (stderr/journal),
/// unlike every other per-repo isolation point in this daemon
/// (`dispatch.rs`/`reroll.rs`), which consistently emit a structured
/// telemetry event for equivalent failure paths.
pub fn normalize_labeled_prs_outcome(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    cache: &mut AdoptionProbeCache,
    now_epoch: u64,
    telemetry_log: &Path,
) -> Result<LabeledPrsIntakeOutcome, DaemonError> {
    let mut metrics = IntakeProbeMetrics::default();
    let target_repos = target_repositories_sweep_order(cfg, now_epoch);
    let mut master_adopted = Vec::new();
    let mut master_outcomes = Vec::new();
    let mut any_rate_limited = false;

    // PR #629 follow-up fix (codex P1 + CodeRabbit convergent finding):
    // `target_repositories_sweep_order` now rotates so no configured repo
    // is discarded FOREVER, but a single tick still can't cover every
    // repo when the configured set exceeds `MAX_INTAKE_REPOS_PER_SWEEP` —
    // an operator should be able to see that from telemetry, not just
    // infer it. `dedup_repo_universe` is the same unbounded, deduped list
    // `target_repositories_sweep_order` computes internally before
    // rotating/truncating, so comparing its length against the cap here
    // detects truncation without duplicating the dedup logic.
    let full_universe_len = dedup_repo_universe(cfg).len();
    if full_universe_len > MAX_INTAKE_REPOS_PER_SWEEP {
        eprintln!(
            "auto-factory daemon: WARNING intake sweep repo count={} exceeds maximum ({}); \
             {} configured repositor(ies) rotate out of this tick's scanned window (full \
             coverage is still guaranteed over successive sweeps — see \
             INTAKE_REPO_SWEEP_TRUNCATED)",
            full_universe_len,
            MAX_INTAKE_REPOS_PER_SWEEP,
            full_universe_len - MAX_INTAKE_REPOS_PER_SWEEP
        );
        emit_intake_sweep_truncated(telemetry_log, full_universe_len, MAX_INTAKE_REPOS_PER_SWEEP);
    }

    // PR #629 follow-up fix (round 3, codex P1 "unconditional tracker
    // fetch starves dispatch on an irrelevant failure"): the tracker
    // snapshot is LAZY + MEMOIZED — fetched at most once per sweep, and
    // only once some repo's PR batch actually needs it (i.e. `scm`
    // returned a non-empty `prs` list for that repo). The round-2 version
    // of this fix fetched unconditionally, upfront, before the per-repo
    // loop even started, and propagated a fetch failure via `?` — correct
    // in isolation (no prior repo's results existed yet to lose), but
    // wrong for the CALLER: `run_slow_tier` (tick.rs) calls this function
    // with `?` too, so an upfront tracker error aborted the ENTIRE
    // `normalize_labeled_prs_outcome` call and, transitively, every other
    // phase of that slow tick — issue intake, routing, dispatch — even
    // when EVERY repo in the sweep returned zero PRs or was rate-limited
    // and the tracker was never actually needed. A malformed/unavailable
    // closed-bead listing has nothing to do with issue intake or
    // dispatching already-QUEUED beads, so it must never be able to
    // starve them. `None` = not yet attempted this sweep; `Some(Err(()))`
    // = attempted and failed (remaining repos needing it degrade without
    // re-attempting or re-fetching); `Some(Ok(..))` = fetched once, reused
    // by every subsequent repo's batch.
    let mut tracker_snapshot_state: Option<TrackerSweepFetchResult> = None;

    for repo in &target_repos {
        if metrics.gh_call_count >= MAX_INTAKE_SWEEP_GH_CALLS {
            eprintln!(
                "auto-factory daemon: WARNING intake sweep gh_call_count={} reached maximum limit ({}); bounding sweep across repositories",
                metrics.gh_call_count, MAX_INTAKE_SWEEP_GH_CALLS
            );
            break;
        }

        let prs_result = scm.labeled_prs_for_repo(repo, FACTORY_LABEL, &mut metrics.gh_call_count);
        let prs = match prs_result {
            Ok(p) => p,
            Err(e) if e.is_gh_rate_limit() => {
                metrics.rate_limited_skips += 1;
                any_rate_limited = true;
                eprintln!(
                    "auto-factory daemon: WARNING intake rate-limited for repository {repo}; skipping and continuing with remaining repositories"
                );
                emit_intake_repo_sweep_failed(telemetry_log, repo, "rate_limited", &e.to_string());
                continue;
            }
            Err(e) => {
                eprintln!(
                    "auto-factory daemon: WARNING intake failed for repository {repo}: {e}; continuing with remaining repositories"
                );
                emit_intake_repo_sweep_failed(telemetry_log, repo, "scm_error", &e.to_string());
                continue;
            }
        };

        if prs.is_empty() {
            continue;
        }

        // This repo's batch needs the tracker snapshot — fetch it now if
        // no repo earlier in this sweep already has (memoized: at most
        // one fetch attempt per sweep, success or failure).
        let snapshot_result = tracker_snapshot_state.get_or_insert_with(|| {
            match (tracker.fetch_candidates(), tracker.fetch_all_external_refs()) {
                (Ok(candidates), Ok(refs)) => Ok((candidates, refs)),
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!(
                        "auto-factory daemon: WARNING intake tracker snapshot fetch failed \
                         (first needed by repository {repo}): {e}; degrading PR intake for the \
                         remainder of this sweep — issue intake and dispatch still proceed"
                    );
                    emit_intake_repo_sweep_failed(telemetry_log, repo, "tracker_snapshot", &e.to_string());
                    Err(())
                }
            }
        });

        let (tracker_candidates, known_refs): (&[Bead], &std::collections::HashSet<String>) =
            match snapshot_result {
                Ok((candidates, refs)) => (candidates.as_slice(), &*refs),
                Err(()) => {
                    eprintln!(
                        "auto-factory daemon: WARNING intake skipping repository {repo}: tracker \
                         snapshot unavailable this sweep (see the earlier \
                         INTAKE_REPO_SWEEP_FAILED tracker_snapshot event)"
                    );
                    continue;
                }
            };
        let tracker_snapshot = TrackerSweepSnapshot {
            candidates: tracker_candidates,
            known_refs,
        };

        let (adopted, outcomes) = normalize_labeled_prs_with_cache(
            scm,
            tracker,
            RepoPrBatch { repo: repo.as_str(), prs: &prs },
            &tracker_snapshot,
            AdoptionProbeState { cache, metrics: &mut metrics },
            now_epoch,
        )?;
        master_adopted.extend(adopted);
        master_outcomes.extend(outcomes);
    }

    // PR #629 follow-up fix: `rate_limited` must only collapse this tick's
    // results (see `run_slow_tier` in tick.rs) when NOTHING usable was
    // gathered from ANY repo — not merely when zero PRs happened to be
    // adopted. The previous `any_rate_limited && master_adopted.is_empty()`
    // check conflated "adopted zero PRs" (a normal, healthy outcome that
    // can still carry real skip/reject `IntakeOutcome`s in
    // `master_outcomes`) with "rate-limited and therefore untrustworthy":
    // a repo with zero adoptions but real skip/reject outcomes, combined
    // with an unrelated rate-limited repo, caused `master_adopted.is_empty()`
    // to be true by coincidence, so the tick discarded that repo's
    // legitimate, already-computed telemetry for an unrelated reason.
    let rate_limited =
        any_rate_limited && master_adopted.is_empty() && master_outcomes.is_empty();
    Ok(LabeledPrsIntakeOutcome {
        adopted: master_adopted,
        outcomes: master_outcomes,
        rate_limited,
        metrics,
    })
}

/// jleechan (PR #629 follow-up fix): emit exactly one structured telemetry
/// event for a per-repo intake sweep failure — either a hard SCM error or a
/// rate-limit skip. `bead_id` is the repo id (no bead exists yet for a
/// repo-level sweep failure), mirroring the `bead_id = external_ref`
/// convention `emit_intake_outcome` (tick.rs) already uses for per-PR
/// outcomes — so `grep <repo> daemon.jsonl` finds this line. Best-effort: a
/// telemetry write failure must never abort or degrade the sweep itself —
/// the caller's own fail-soft isolation (`continue` past the failing repo)
/// is the load-bearing behavior; telemetry is an observability side
/// channel only.
fn emit_intake_repo_sweep_failed(telemetry_log: &Path, repo: &str, error_class: &str, error: &str) {
    let event = TelemetryEvent {
        timestamp: now_iso8601(),
        bead_id: repo.to_string(),
        attempt_id: 1,
        lifecycle_state: "INTAKE".to_string(),
        event_type: "INTAKE_REPO_SWEEP_FAILED".to_string(),
        metrics: serde_json::json!({}),
        context: serde_json::json!({
            "repo": repo,
            "error_class": error_class,
            "error": error,
        }),
    };
    if let Err(e) = telemetry::emit(telemetry_log, &event) {
        eprintln!(
            "auto-factory daemon: WARNING failed to emit INTAKE_REPO_SWEEP_FAILED telemetry for {repo}: {e}"
        );
    }
}

/// PR #629 follow-up fix (codex P1 + CodeRabbit convergent finding):
/// structured telemetry counterpart to `target_repositories_sweep_order`'s
/// rotation. Rotation guarantees every configured repo eventually gets
/// scanned, but a single tick still doesn't cover everything when the
/// configured set exceeds `MAX_INTAKE_REPOS_PER_SWEEP` — an operator
/// should be able to see that from the daemon's telemetry stream, not
/// just an `eprintln!`. `bead_id` is a fixed sentinel (no single repo or
/// bead owns a sweep-wide truncation event).
fn emit_intake_sweep_truncated(telemetry_log: &Path, configured_repo_count: usize, cap: usize) {
    let event = TelemetryEvent {
        timestamp: now_iso8601(),
        bead_id: "intake_sweep".to_string(),
        attempt_id: 1,
        lifecycle_state: "INTAKE".to_string(),
        event_type: "INTAKE_REPO_SWEEP_TRUNCATED".to_string(),
        metrics: serde_json::json!({}),
        context: serde_json::json!({
            "configured_repo_count": configured_repo_count,
            "cap": cap,
        }),
    };
    if let Err(e) = telemetry::emit(telemetry_log, &event) {
        eprintln!(
            "auto-factory daemon: WARNING failed to emit INTAKE_REPO_SWEEP_TRUNCATED telemetry: {e}"
        );
    }
}

/// At-most-once-per-sweep snapshot of the tracker's known beads/refs,
/// lazily fetched in `normalize_labeled_prs_outcome` the first time some
/// repo's PR batch actually needs it, and memoized for the rest of the
/// sweep (see that function's doc comment for the starvation bug this
/// closes — round 3 of the PR #629 follow-up).
pub(crate) struct TrackerSweepSnapshot<'a> {
    candidates: &'a [Bead],
    known_refs: &'a std::collections::HashSet<String>,
}

/// Named alias for the lazy tracker-snapshot memo's inner `Result`, purely
/// to keep `normalize_labeled_prs_outcome`'s local binding under clippy's
/// `type_complexity` threshold without an `#[allow]`. `Ok` holds the
/// owned, one-time-fetched `(fetch_candidates, fetch_all_external_refs)`
/// pair; `Err(())` records "already attempted and failed this sweep" —
/// the underlying `DaemonError` is only needed at the point of failure
/// (already consumed into an `eprintln!`/telemetry event there), not
/// after memoization.
type TrackerSweepFetchResult = Result<(Vec<Bead>, std::collections::HashSet<String>), ()>;

/// One repo's already-fetched PR batch — bundled with `repo` purely to keep
/// `normalize_labeled_prs_with_cache`'s argument count under clippy's
/// `too_many_arguments` threshold without an `#[allow]`.
pub(crate) struct RepoPrBatch<'a> {
    repo: &'a str,
    prs: &'a [LabeledPr],
}

/// Mutable per-sweep accumulator state threaded through the per-PR loop —
/// bundled for the same argument-count reason as `RepoPrBatch`.
pub(crate) struct AdoptionProbeState<'a> {
    cache: &'a mut AdoptionProbeCache,
    metrics: &'a mut IntakeProbeMetrics,
}

/// jtg8-r4: cache-aware adoption loop. Mirrors `normalize_labeled_prs`'s
/// control flow but gates every per-PR `collaborator_permission` call on
/// `AdoptionProbeCache.contains_fresh(key, now_epoch)`. When the cache is
/// cold (first tick or after a key change), probes run as before and the
/// decisions are stored. When the cache is warm (unchanged key + decision
/// still fresh), the cached permission tier is reused and ZERO gh calls
/// are made for that PR.
///
/// r4 invalidation: `AuthorPermission` decisions are re-probed after
/// `MAX_CACHED_PERMISSION_AGE_SECS` regardless of whether `head_sha` /
/// `updated_at_epoch` changed, so a contributor's Read → Write promotion
/// surfaces within one TTL window instead of replaying the stale skip
/// forever. (`prs` is the already-fetched `labeled_prs` result — caller
/// passed them in so `normalize_labeled_prs_outcome` could count the list
/// query as 1 gh call before delegating here.)
///
/// `tracker_snapshot` is the ONE-TIME-per-sweep `fetch_candidates`/
/// `fetch_all_external_refs` result — see `normalize_labeled_prs_outcome`'s
/// doc comment. This function no longer fetches the tracker snapshot
/// itself, so a per-repo processing error here can never abort the whole
/// multi-repo sweep the way a tracker-fetch error used to.
pub(crate) fn normalize_labeled_prs_with_cache(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    batch: RepoPrBatch,
    tracker_snapshot: &TrackerSweepSnapshot,
    state: AdoptionProbeState,
    now_epoch: u64,
) -> Result<(Vec<ExistingPrIntake>, Vec<IntakeOutcome>), DaemonError> {
    let repo = batch.repo;
    // jleechan-r28r: own the slice so the per-iteration
    // `pr.external_ref = to_canonical_external_ref(...)` mutation is
    // permitted (`batch.prs` is `&'a [LabeledPr]`, not `Vec<LabeledPr>`).
    let prs: Vec<LabeledPr> = batch.prs.to_vec();
    let tracker_candidates = tracker_snapshot.candidates;
    let known_refs = tracker_snapshot.known_refs;
    let cache = state.cache;
    let metrics = state.metrics;
    let mut intakes = Vec::new();
    let mut outcomes = Vec::new();

    for mut pr in prs {
        // PR #629 follow-up fix (codex P2 "Enforce the call cap within
        // each repository scan"): the sweep-wide `MAX_INTAKE_SWEEP_GH_CALLS`
        // budget was only checked BETWEEN repos, in
        // `normalize_labeled_prs_outcome`'s outer loop — a single repo with
        // many labeled PRs could still blow through the cap here via
        // repeated cache-miss `collaborator_permission_for_repo` probes
        // before the outer loop ever got a chance to check again. Stop
        // incrementally, the moment the cumulative sweep budget is
        // exhausted, exactly like the outer loop already does between
        // repos; the remaining PRs in this repo are simply not processed
        // this tick and get picked up on a future sweep.
        if metrics.gh_call_count >= MAX_INTAKE_SWEEP_GH_CALLS {
            eprintln!(
                "auto-factory daemon: WARNING intake sweep gh_call_count={} reached maximum limit ({}); \
                 stopping mid-scan of repository {repo} with PR(s) unprocessed this tick",
                metrics.gh_call_count, MAX_INTAKE_SWEEP_GH_CALLS
            );
            break;
        }

        // jleechan-r28r: normalize URL form to canonical owner/repo#N
        // BEFORE any cache/dedup comparison so a later short-form event for
        // the same PR hits the same `known_refs` key as an earlier URL
        // event.
        pr.external_ref = to_canonical_external_ref(&pr.external_ref);

        let key = ProbeCacheKey::from_pr(&pr);

        // Pre-flight: empty head_ref_name — uncacheable, never probed.
        if pr.head_ref_name.trim().is_empty() {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedIneligible {
                    precondition: "empty_head_ref_name".to_string(),
                },
            });
            continue;
        }

        // Pre-flight: cross-repo — uncacheable (the rejection comment must
        // be posted fresh so the contributor sees it on this tick).
        if !same_repo_pr(&pr, repo) {
            let comment_body = "🤖 **[dark-factory]** Escalation required: fork/cross-repository PR adoption is not supported in v1. Same-repo factory PRs can be verified automatically; fork remediation lands with bead `jleechan-tfs1`.";
            let _ = tracker.comment_external(&pr.external_ref, comment_body);
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedFork,
            });
            continue;
        }

        // Cache lookup: if the PR's key is in the cache AND the decision
        // is still fresh (< MAX_CACHED_PERMISSION_AGE_SECS old), reuse it.
        // r4 invalidation: a stale AuthorPermission entry is treated as a
        // miss so contributor tier changes surface within the TTL window.
        if cache.contains_fresh(&key, now_epoch) {
            metrics.probe_cache_hits += 1;
            let decision = cache.get(&key).unwrap().decision.clone();
            match decision {
                CachedDecisionKind::AuthorPermission(perm) => {
                    if !perm.is_write_tier() {
                        outcomes.push(IntakeOutcome {
                            external_ref: pr.external_ref.clone(),
                            verdict: IntakeVerdict::SkippedIneligible {
                                precondition: format!(
                                    "author_permission_below_write_tier:{perm:?}"
                                ),
                            },
                        });
                        continue;
                    }
                    // Cache hit with write tier — fall through to the
                    // existing-bead / new-bead branch below.
                }
            }
        } else {
            // Cold cache OR stale entry: probe fresh, store the result
            // with a fresh `cached_at_epoch` timestamp so the r4 TTL
            // window restarts.
            metrics.probe_cache_misses += 1;
            metrics.gh_call_count += 1;
            let permission = match scm.collaborator_permission_for_repo(repo, &pr.author_login) {
                Ok(p) => p,
                Err(e) => {
                    outcomes.push(IntakeOutcome {
                        external_ref: pr.external_ref.clone(),
                        verdict: IntakeVerdict::SkippedIneligible {
                            precondition: format!("probe_error:{e}"),
                        },
                    });
                    continue;
                }
            };
            cache.insert(
                key.clone(),
                CachedDecisionKind::AuthorPermission(permission),
                now_epoch,
            );
            if !permission.is_write_tier() {
                outcomes.push(IntakeOutcome {
                    external_ref: pr.external_ref.clone(),
                    verdict: IntakeVerdict::SkippedIneligible {
                        precondition: format!("author_permission_below_write_tier:{permission:?}"),
                    },
                });
                continue;
            }
        }

        // Existing-bead fast path: a tracked candidate with the matching
        // external_ref already covers this PR. No gh call needed.
        if let Some(bead) = tracker_candidates
            .iter()
            .find(|bead| bead.external_ref.as_deref() == Some(pr.external_ref.as_str()))
        {
            intakes.push(ExistingPrIntake {
                bead_id: bead.id.clone(),
                pr_number: pr.number,
                head_ref_name: pr.head_ref_name.clone(),
                external_ref: pr.external_ref.clone(),
                newly_created: false,
            });
            continue;
        }

        // Already-known-ref path: no create_bead needed.
        if known_refs.contains(&pr.external_ref) {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedDuplicate,
            });
            continue;
        }

        // New PR — create_bead. The existing race-recovery logic from
        // `normalize_labeled_prs` carries over verbatim.
        let title = format!("{} ({})", pr.title, repo);
        let bead_id = match tracker.create_bead(&title, &pr.body, &pr.external_ref) {
            Ok(id) => id,
            Err(e) => {
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
        intakes.push(ExistingPrIntake {
            bead_id,
            pr_number: pr.number,
            head_ref_name: pr.head_ref_name.clone(),
            external_ref: pr.external_ref.clone(),
            newly_created: true,
        });
    }

    Ok((intakes, outcomes))
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

    for mut issue in issues {
        // jleechan-r28r: normalize URL form to canonical owner/repo#N
        // BEFORE the known_refs.contains check so two intake events for the
        // same PR (one URL-shaped, one short-shaped) hit the same dedup
        // key.
        issue.external_ref = to_canonical_external_ref(&issue.external_ref);

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
        let permission = match scm.collaborator_permission_for_repo(&cfg.target_repo, &issue.author_login) {
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
    // jtg8-r5: pass a counter into `labeled_prs` so any REST-fallback
    // per-PR pulls show up in the metric alongside the list query (the
    // legacy `normalize_labeled_prs` path doesn't surface the metric to
    // telemetry, but the trait signature must stay uniform across both
    // callers to avoid drift).
    let mut legacy_gh_calls: u32 = 0;
    let prs = scm.labeled_prs_for_repo(&cfg.target_repo, FACTORY_LABEL, &mut legacy_gh_calls)?;
    if prs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let tracker_candidates = tracker.fetch_candidates()?;
    let known_refs = tracker.fetch_all_external_refs()?;
    let mut intakes = Vec::new();
    let mut outcomes = Vec::new();

    for mut pr in prs {
        // jleechan-r28r: normalize URL form to canonical owner/repo#N
        // BEFORE the head_ref_name / tracker_candidates / known_refs /
        // create_bead checks — see to_canonical_external_ref for the
        // duplicate-pair rationale.
        pr.external_ref = to_canonical_external_ref(&pr.external_ref);

        if pr.head_ref_name.trim().is_empty() {
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedIneligible {
                    precondition: "empty_head_ref_name".to_string(),
                },
            });
            continue;
        }

        if !same_repo_pr(&pr, &cfg.target_repo) {
            let comment_body = "🤖 **[dark-factory]** Escalation required: fork/cross-repository PR adoption is not supported in v1. Same-repo factory PRs can be verified automatically; fork remediation lands with bead `jleechan-tfs1`.";
            let _ = tracker.comment_external(&pr.external_ref, comment_body);
            outcomes.push(IntakeOutcome {
                external_ref: pr.external_ref.clone(),
                verdict: IntakeVerdict::SkippedFork,
            });
            continue;
        }

        let permission = match scm.collaborator_permission_for_repo(&cfg.target_repo, &pr.author_login) {
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
        intakes.push(ExistingPrIntake {
            bead_id,
            pr_number: pr.number,
            head_ref_name: pr.head_ref_name,
            external_ref: pr.external_ref,
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
    use super::{resolve_target_repo, CacheFileLock};

    #[test]
    fn cache_lock_waits_for_live_owner_and_releases_after_owner_drop() {
        let root = std::env::temp_dir().join(format!(
            "afd_cache_lock_owner_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let cache_path = root.join("adoption_probe_cache.json");
        let owner = CacheFileLock::acquire(&cache_path).unwrap();
        let waiter_path = cache_path.clone();
        let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waiter_acquired = acquired.clone();
        let waiter = std::thread::spawn(move || {
            let _lock = CacheFileLock::acquire(&waiter_path).unwrap();
            waiter_acquired.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!acquired.load(std::sync::atomic::Ordering::SeqCst));
        drop(owner);
        waiter.join().unwrap();
        assert!(acquired.load(std::sync::atomic::Ordering::SeqCst));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_lock_three_party_contenders_never_overlap_rmw_owners() {
        let root = std::env::temp_dir().join(format!(
            "afd_cache_lock_three_party_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let cache_path = root.join("adoption_probe_cache.json");
        let owner = CacheFileLock::acquire(&cache_path).unwrap();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut contenders = Vec::new();
        for _ in 0..2 {
            let path = cache_path.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            contenders.push(std::thread::spawn(move || {
                let _lock = CacheFileLock::acquire(&path).unwrap();
                let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_active.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(25));
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(owner);
        for contender in contenders {
            contender.join().unwrap();
        }
        assert_eq!(max_active.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

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
