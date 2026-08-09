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
use crate::tools::{LabeledPr, Permission, Scm, Tracker};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FACTORY_LABEL: &str = "factory";
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// writes are atomic via a sibling temporary file + `rename`, so a crash mid-write
    /// leaves either the old or the new file intact (never a half-written
    /// file the next daemon boot would fail to parse).
    pub fn persist(&self) -> Result<(), DaemonError> {
        // Re-serialize as a Vec of (key, decision) tuples so the JSON
        // stays forward-compatible (HashMap iteration order is
        // non-deterministic; Vec preserves the order we wrote it in, but
        // order doesn't matter for correctness — only for cleaner diffs).
        let entries: Vec<(ProbeCacheKey, CachedProbeDecision)> = self
            .decisions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let json = serde_json::to_string(&entries).map_err(|e| {
            DaemonError::Parse(format!("serialize adoption_probe_cache: {e}"))
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
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
pub fn normalize_labeled_prs_outcome(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    cache: &mut AdoptionProbeCache,
    now_epoch: u64,
) -> Result<LabeledPrsIntakeOutcome, DaemonError> {
    let mut metrics = IntakeProbeMetrics::default();
    // jtg8-r5: plumb the metric into `labeled_prs` so the REST fallback's
    // per-PR `pulls/{n}` calls are reflected in `gh_call_count` (r4 only
    // counted this single list-query increment, so the slow-tier
    // `INTAKE_GH_CALL_WARN_THRESHOLD` warning never fired when the daemon
    // was burning O(N) core API calls via the fallback path).
    let prs_result = scm.labeled_prs(FACTORY_LABEL, &mut metrics.gh_call_count);
    // Even on rate-limit, the impl incremented `gh_call_count` once for
    // the failed list query — surface that in the metric so the warning
    // sees the actual failure mode.
    let prs = match prs_result {
        Ok(p) => p,
        Err(e) if e.is_gh_rate_limit() => {
            metrics.rate_limited_skips += 1;
            return Ok(LabeledPrsIntakeOutcome {
                adopted: Vec::new(),
                outcomes: Vec::new(),
                rate_limited: true,
                metrics,
            });
        }
        Err(e) => return Err(e),
    };
    if prs.is_empty() {
        return Ok(LabeledPrsIntakeOutcome {
            adopted: Vec::new(),
            outcomes: Vec::new(),
            rate_limited: false,
            metrics,
        });
    }

    // Delegate the per-PR decisions to the cache-aware variant below.
    let (adopted, outcomes) = normalize_labeled_prs_with_cache(
        scm,
        tracker,
        cfg,
        &prs,
        cache,
        &mut metrics,
        now_epoch,
    )?;
    Ok(LabeledPrsIntakeOutcome {
        adopted,
        outcomes,
        rate_limited: false,
        metrics,
    })
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
pub(crate) fn normalize_labeled_prs_with_cache(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    prs: &[LabeledPr],
    cache: &mut AdoptionProbeCache,
    metrics: &mut IntakeProbeMetrics,
    now_epoch: u64,
) -> Result<(Vec<ExistingPrIntake>, Vec<IntakeOutcome>), DaemonError> {
    let tracker_candidates = tracker.fetch_candidates()?;
    let known_refs = tracker.fetch_all_external_refs()?;
    let mut intakes = Vec::new();
    let mut outcomes = Vec::new();

    for pr in prs {
        let key = ProbeCacheKey::from_pr(pr);

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
        if !same_repo_pr(pr, cfg) {
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
            let permission = match scm.collaborator_permission(&pr.author_login) {
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
        let title = format!("{} ({})", pr.title, cfg.target_repo);
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
    // jtg8-r5: pass a counter into `labeled_prs` so any REST-fallback
    // per-PR pulls show up in the metric alongside the list query (the
    // legacy `normalize_labeled_prs` path doesn't surface the metric to
    // telemetry, but the trait signature must stay uniform across both
    // callers to avoid drift).
    let mut legacy_gh_calls: u32 = 0;
    let prs = scm.labeled_prs(FACTORY_LABEL, &mut legacy_gh_calls)?;
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
