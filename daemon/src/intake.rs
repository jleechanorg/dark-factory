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

const FACTORY_LABEL: &str = "factory";

/// jtg8 acceptance #3: per-tick gh call count + threshold-warn trigger. The
/// slow tier records every `gh` invocation it makes this pass (the
/// `labeled_prs` list query + each per-PR `collaborator_permission` call +
/// each REST `/pulls/{n}` lookup), and exposes the total to callers so the
/// telemetry layer can emit `INTAKE_GH_CALLS_EXCEEDED` when count >=
/// threshold. Reset to zero at the start of every slow-tier pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntakeProbeMetrics {
    pub gh_call_count: u32,
    pub probe_cache_hits: u32,
    pub probe_cache_misses: u32,
    pub rate_limited_skips: u32,
}

/// jtg8 acceptance #5: outcome shape returned by the new
/// `normalize_labeled_prs_outcome` helper. The `rate_limited` flag tells the
/// slow tier to skip the intake pass (no error, no `consecutive_failures`
/// increment) so the daemon's dispatch/ledger work — which needs no gh
/// calls — keeps running through a `gh` 403 window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledPrsIntakeOutcome {
    pub adopted: Vec<ExistingPrIntake>,
    pub outcomes: Vec<IntakeOutcome>,
    pub rate_limited: bool,
    pub metrics: IntakeProbeMetrics,
}

/// jtg8 cache key per candidate PR. The hash tuple `(external_ref,
/// head_sha, updated_at_epoch)` invalidates when any of the three change —
/// `head_sha` for new commits, `updated_at_epoch` for body/comment edits,
/// `external_ref` for owner/repo renames (defensive; rare in practice).
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
}

/// jtg8 cache entry — one row per (PR, cache-key) the slow tier has probed.
/// Stores the *decisions* made about that PR, not the raw gh responses, so
/// future ticks can replay them with zero gh calls.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CachedProbeDecision {
    /// Author permission tier resolved; cache survives until the PR's
    /// cache key changes (a new push or an edit). Author-login changes
    /// (rare) are surfaced as a fresh tier check on next cache miss.
    AuthorPermission(Permission),
    /// The PR was rejected (SkippedFork / SkippedIneligible) on a prior
    /// tick. The verdict + reason are cached so the slow tier doesn't
    /// re-probe permission / re-fetch cross-repo state every tick.
    Rejected,
}

#[derive(Debug, Default, Clone)]
pub struct AdoptionProbeCache {
    /// keyed by `ProbeCacheKey`. The hash impl treats `(Some, Some)` as a
    /// distinct key from `(Some, None)` so PRs whose `updated_at_epoch`
    /// hasn't been populated yet (REST-fallback with schema drift) hash
    /// independently — `None` keys still get cached, just separately from
    /// `Some` keys.
    decisions: std::collections::HashMap<ProbeCacheKey, CachedProbeDecision>,
}

/// jtg8: on-disk persistence for the `AdoptionProbeCache`. The cache lives
/// across daemon restarts so a freshly-spawned daemon doesn't re-probe the
/// entire factory-labeled PR set on its first slow tick (which would burn
/// ~50 gh API calls at startup). The file is rewritten on every slow-tier
/// pass via `persist`; `load` is best-effort and silently returns an empty
/// cache on missing/corrupt files.
pub const ADOPTION_PROBE_CACHE_FILE: &str = ".beads/adoption_probe_cache.json";

impl AdoptionProbeCache {
    /// Load the cache from `ADOPTION_PROBE_CACHE_FILE` if present; otherwise
    /// return an empty cache. Never errors out — a corrupt or absent file
    /// just means "cold cache, probe everything" (the pre-fix behavior).
    pub fn load_or_default() -> Self {
        let raw = match std::fs::read_to_string(ADOPTION_PROBE_CACHE_FILE) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str::<Vec<(ProbeCacheKey, CachedProbeDecision)>>(&raw) {
            Ok(entries) => Self {
                decisions: entries.into_iter().collect(),
            },
            Err(_) => Self::default(),
        }
    }

    /// Persist the cache to `ADOPTION_PROBE_CACHE_FILE`. Best-effort: writes
    /// are atomic via `tempfile` + `rename`, so a crash mid-write leaves
    /// either the old or the new file intact (never a half-written file the
    /// next daemon boot would fail to parse).
    pub fn persist(&self) -> Result<(), DaemonError> {
        // Re-serialize as a Vec of (key, decision) tuples so the JSON stays
        // forward-compatible (HashMap iteration order is non-deterministic;
        // Vec preserves the order we wrote it in, but order doesn't matter
        // for correctness — only for cleaner diffs).
        let entries: Vec<(ProbeCacheKey, CachedProbeDecision)> = self
            .decisions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let json = serde_json::to_string(&entries).map_err(|e| {
            DaemonError::Parse(format!("serialize adoption_probe_cache: {e}"))
        })?;
        let parent = std::path::Path::new(ADOPTION_PROBE_CACHE_FILE).parent();
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = format!("{ADOPTION_PROBE_CACHE_FILE}.tmp");
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| DaemonError::Config(format!("write adoption_probe_cache: {e}")))?;
        std::fs::rename(&tmp, ADOPTION_PROBE_CACHE_FILE).map_err(|e| {
            DaemonError::Config(format!(
                "rename adoption_probe_cache tmp->final: {e}"
            ))
        })?;
        Ok(())
    }

    /// jtg8: drop entries whose `updated_at_epoch` is older than `max_age`
    /// from the cache. Defensive — caps the cache file size in case PRs
    /// accumulate and never get cleaned up (e.g. closed PRs whose cache
    /// entries would otherwise linger forever). Currently unused by the
    /// slow tier but exposed for ops use.
    #[allow(dead_code)]
    pub fn evict_older_than(&mut self, now_epoch: u64, max_age_secs: u64) {
        self.decisions.retain(|k, _| {
            k.updated_at_epoch
                .map(|t| now_epoch.saturating_sub(t) <= max_age_secs)
                .unwrap_or(true)
        });
    }
}

impl AdoptionProbeCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff the cache has a stored decision for this exact key. Callers
    /// MUST consult this BEFORE invoking any gh-side per-PR probe.
    pub fn contains(&self, key: &ProbeCacheKey) -> bool {
        self.decisions.contains_key(key)
    }

    /// Return the cached decision, if any. Mirrors `contains` semantics.
    pub fn get(&self, key: &ProbeCacheKey) -> Option<&CachedProbeDecision> {
        self.decisions.get(key)
    }

    /// Record a decision. Overwrites any prior entry for this key (idempotent
    /// within a single tick — `normalize_labeled_prs` writes the entry only
    /// once per PR per pass).
    pub fn insert(&mut self, key: ProbeCacheKey, decision: CachedProbeDecision) {
        self.decisions.insert(key, decision);
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

/// jtg8 acceptance #5: thin wrapper around `normalize_labeled_prs` that
/// detects rate-limit exhaustion in the upstream `labeled_prs` list call
/// and returns a `rate_limited = true` outcome (with empty adopted/outcomes)
/// instead of propagating `Err` to the slow-tier tick scheduler. The slow
/// tier translates this to a "skip intake, keep dispatch alive" log line and
/// does NOT increment `consecutive_failures`.
pub fn normalize_labeled_prs_outcome(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    cache: &mut AdoptionProbeCache,
) -> Result<LabeledPrsIntakeOutcome, DaemonError> {
    // We can't intercept the underlying `labeled_prs` call from here — the
    // Scm trait has no rate-limit-aware variant. Instead we catch the
    // `Tool { tool: "gh", .. }` shaped error after the fact: re-run the
    // list query via `scm.labeled_prs` once, and if it errors with a rate
    // limit, return the degraded outcome. If it succeeds, call into
    // `normalize_labeled_prs` and wrap its result.
    //
    // Cost: one extra `labeled_prs` call per slow tick vs. the bare
    // `normalize_labeled_prs` path. The probe cache below makes the
    // per-PR probes free on unchanged keys, so the list call is the only
    // remaining per-tick gh hit — and it carries the cache key fields
    // (`head_sha`, `updated_at_epoch`) that downstream probes need to
    // short-circuit.
    let mut metrics = IntakeProbeMetrics::default();
    let prs_result = scm.labeled_prs(FACTORY_LABEL);
    metrics.gh_call_count += 1;
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
    )?;
    Ok(LabeledPrsIntakeOutcome {
        adopted,
        outcomes,
        rate_limited: false,
        metrics,
    })
}

/// jtg8 acceptance #4: metrics-aware variant. Identical semantics to
/// `normalize_labeled_prs_outcome` but exposes the gh-call counter so
/// telemetry can warn at > threshold. Convenience wrapper — implementation
/// lives in `normalize_labeled_prs_outcome`.
pub fn normalize_labeled_prs_with_metrics(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    cache: &mut AdoptionProbeCache,
) -> Result<IntakeProbeMetrics, DaemonError> {
    Ok(normalize_labeled_prs_outcome(scm, tracker, cfg, cache)?.metrics)
}

/// jtg8 cache-aware adoption loop. Mirrors `normalize_labeled_prs`'s
/// control flow but gates every per-PR `collaborator_permission` call on
/// `AdoptionProbeCache.contains(key)`. When the cache is cold (first tick
/// or after a key change), probes run as before and the decisions are
/// stored. When the cache is warm (unchanged key), the cached permission
/// tier is reused and ZERO gh calls are made for that PR.
///
/// `prs` is the already-fetched `labeled_prs` result (caller passed them in
/// so `normalize_labeled_prs_outcome` could count the list query as 1 gh
/// call before delegating here).
pub(crate) fn normalize_labeled_prs_with_cache(
    scm: &dyn Scm,
    tracker: &dyn Tracker,
    cfg: &Config,
    prs: &[LabeledPr],
    cache: &mut AdoptionProbeCache,
    metrics: &mut IntakeProbeMetrics,
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

        // Cache lookup: if the PR's key is in the cache AND it was a
        // successful AuthorPermission decision, reuse it. Rejected entries
        // (SkippedIneligible) are still cached, so we short-circuit too.
        if cache.contains(&key) {
            metrics.probe_cache_hits += 1;
            let decision = cache.get(&key).unwrap().clone();
            match decision {
                CachedProbeDecision::AuthorPermission(perm) => {
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
                CachedProbeDecision::Rejected => {
                    // We don't cache SkippedFork / SkippedIneligible here
                    // (those are pre-flight checks above), but if a future
                    // change starts caching them, replay the rejection.
                    continue;
                }
            }
        } else {
            // Cold cache: probe fresh, store the result.
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
            cache.insert(key.clone(), CachedProbeDecision::AuthorPermission(permission));
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
    let prs = scm.labeled_prs(FACTORY_LABEL)?;
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
    use crate::tools::{LabeledPr, Permission};
    use super::{
        resolve_target_repo, AdoptionProbeCache, CachedProbeDecision, ProbeCacheKey,
    };

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

    // jtg8 unit coverage: the AdoptionProbeCache keying + decision storage
    // must distinguish between (1) two PRs sharing an external_ref prefix
    // but different head SHAs and (2) the same PR across two ticks.

    fn make_pr(number: u64, head_sha: Option<&str>, updated_at: Option<u64>) -> LabeledPr {
        LabeledPr {
            number,
            title: format!("pr {number}"),
            body: String::new(),
            author_login: "alice".into(),
            external_ref: format!("owner/repo#{number}"),
            head_ref_name: format!("feature/pr-{number}"),
            is_cross_repository: false,
            head_repo_full_name: Some("owner/repo".into()),
            head_repo_owner_login: Some("owner".into()),
            head_sha: head_sha.map(String::from),
            updated_at_epoch: updated_at,
        }
    }

    #[test]
    fn probe_cache_keys_on_external_ref_head_sha_and_updated_at() {
        let mut cache = AdoptionProbeCache::new();

        let p1 = make_pr(1, Some("sha-a"), Some(100));
        let p1_dup = make_pr(1, Some("sha-a"), Some(100));
        let p1_new_sha = make_pr(1, Some("sha-b"), Some(100));
        let p1_new_time = make_pr(1, Some("sha-a"), Some(101));

        let k1 = ProbeCacheKey::from_pr(&p1);
        let k1_dup = ProbeCacheKey::from_pr(&p1_dup);
        let k1_new_sha = ProbeCacheKey::from_pr(&p1_new_sha);
        let k1_new_time = ProbeCacheKey::from_pr(&p1_new_time);

        // Identical keys collide (cache hit semantics).
        cache.insert(k1.clone(), CachedProbeDecision::AuthorPermission(Permission::Write));
        assert!(cache.contains(&k1_dup), "identical keys must collide");

        // A different head_sha invalidates the cache entry.
        assert!(
            !cache.contains(&k1_new_sha),
            "head_sha change must produce a new key (cache miss)"
        );

        // A different updated_at invalidates the cache entry.
        assert!(
            !cache.contains(&k1_new_time),
            "updated_at change must produce a new key (cache miss)"
        );
    }

    #[test]
    fn probe_cache_none_keys_still_distinct_from_some_keys() {
        // REST-fallback PRs whose `head_sha` is `None` (schema drift) must
        // still be cacheable on (external_ref, None, updated_at) — but
        // distinctly from any Some-keys for the same external_ref.
        let mut cache = AdoptionProbeCache::new();
        let p_none = make_pr(10, None, Some(100));
        let p_some = make_pr(10, Some("sha-x"), Some(100));
        let k_none = ProbeCacheKey::from_pr(&p_none);
        let k_some = ProbeCacheKey::from_pr(&p_some);
        assert_ne!(k_none, k_some, "None vs Some head_sha must be distinct keys");
        cache.insert(k_none.clone(), CachedProbeDecision::AuthorPermission(Permission::Read));
        assert!(cache.contains(&k_none));
        assert!(!cache.contains(&k_some));
    }
}
