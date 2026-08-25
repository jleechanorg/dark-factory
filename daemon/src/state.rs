// Task 4: SQLite-backed overlay state store per contracts/schema.sql and
// design doc §3. WAL + busy_timeout=5000 match the discipline used by the
// Python runner's CXDB (runner/cxdb.py), but this is a separate DB file with
// no cross-language schema coupling.
use crate::errors::DaemonError;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Spec §4.2.7 overlay states, incl. r3 pre-PR states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OverlayState {
    Queued,       // bead accepted by intake, awaiting dispatch
    Dispatching, // dispatch intent + branch registered, spawn not yet confirmed (spec §4.2.2/§4.2.4)
    Dispatched,  // worker/pipeline running, no PR yet
    Attested,    // PR open, under verification
    Ready,       // terminal: 7-green, readiness posted, daemon stops driving
    ReRoll,      // re-roll in progress (Stage 2)
    Recovery,    // spec mutated, awaiting re-dispatch (Stage 2)
    Redispatched, // handed back to the queue
    BudgetHeld,  // budget exhaustion (monitoring-only in Stage 1/2)
    HumanHeld,   // terminal until human action
    /// Bead jleechan-zaga / issue #348: gate assessment is red but EVERY
    /// red gate is `Structural` (external reviewer usage-limits, bot
    /// threads on superseded content, evidence floor owned by a different
    /// bead). Re-rolling the coder cannot clear any of them, so the
    /// daemon holds the bead with a per-gate disposition request rather
    /// than superseding it. Distinct from `HumanHeld` because there is
    /// no operator-visible blocker to diagnose; the daemon has surfaced
    /// every red gate's disposition need and is awaiting the conditions
    /// to change (e.g. external reviewer quota reset, bot thread
    /// resolution, the other bead landing). The fast tier continues to
    /// assess on each tick; once any red gate flips `CoderFixable` (or
    /// to `Green`), the bead leaves this state and re-enters the normal
    /// flow. Until then, this state is the floor's "no churn" anchor
    /// that prevents the supersede-→HUMAN_HELD cycle issue #348
    /// documents (v6ud's CORRECT P0 fix PR #342 was superseded by its
    /// own reroll ~25 min after opening because every red gate was
    /// structural — the daemon could never have reached all_green on
    /// any attempt number).
    DispositionRequired,
}

impl OverlayState {
    /// SCREAMING_SNAKE_CASE string mapping matching contracts/schema.sql's CHECK constraint.
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayState::Queued => "QUEUED",
            OverlayState::Dispatching => "DISPATCHING",
            OverlayState::Dispatched => "DISPATCHED",
            OverlayState::Attested => "ATTESTED",
            OverlayState::Ready => "READY",
            OverlayState::ReRoll => "RE_ROLL",
            OverlayState::Recovery => "RECOVERY",
            OverlayState::Redispatched => "REDISPATCHED",
            OverlayState::BudgetHeld => "BUDGET_HELD",
            OverlayState::HumanHeld => "HUMAN_HELD",
            OverlayState::DispositionRequired => "DISPOSITION_REQUIRED",
        }
    }

    // Mirrors `OverlayState::as_str` (SCREAMING_SNAKE_CASE strings tied to
    // contracts/schema.sql's CHECK constraint) rather than the general-purpose
    // parsing `std::str::FromStr` implies, so the inherent method is kept
    // instead of adding a trait impl clippy would consider more idiomatic here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, DaemonError> {
        match s {
            "QUEUED" => Ok(OverlayState::Queued),
            "DISPATCHING" => Ok(OverlayState::Dispatching),
            "DISPATCHED" => Ok(OverlayState::Dispatched),
            "ATTESTED" => Ok(OverlayState::Attested),
            "READY" => Ok(OverlayState::Ready),
            "RE_ROLL" => Ok(OverlayState::ReRoll),
            "RECOVERY" => Ok(OverlayState::Recovery),
            "REDISPATCHED" => Ok(OverlayState::Redispatched),
            "BUDGET_HELD" => Ok(OverlayState::BudgetHeld),
            "HUMAN_HELD" => Ok(OverlayState::HumanHeld),
            "DISPOSITION_REQUIRED" => Ok(OverlayState::DispositionRequired),
            other => Err(DaemonError::Parse(format!(
                "unknown overlay state: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BeadOverlay {
    pub bead_id: String,
    pub state: OverlayState,
    pub attempt: u32, // r<n> counter
    pub reroll_count: u32,
    pub autonomy_secs: u64, // cumulative — nothing on the automated path resets it
    pub spend_usd: f64,     // monitoring-only metric (spec §4.2.8)
    pub pr_number: Option<u64>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    /// Agent Orchestrator project used by the durable worker session. `None`
    /// preserves legacy rows that predate per-session project routing.
    pub session_ao_project: Option<String>,
    /// Adopted-PR provenance (bead jleechan-tfs1): `true` iff `branch` is an
    /// external contributor's own head_ref_name (adopted via
    /// `intake::normalize_labeled_prs`, set in `tick::run_slow_tier` at
    /// adoption time), `false` for factory-fabricated branches. `reroll()`
    /// reads this to pick append-only remediation (adopted) vs. the
    /// fabricate-new-branch + close-old-PR path (factory-fabricated).
    /// Explicit stored flag, NOT inferred from branch name.
    pub is_adopted: bool,
    /// Consecutive transient `Sessions::spawn` failures since the last
    /// confirmed `DISPATCHED` (bead jleechan follow-up to #198: a bead whose
    /// spawn deterministically fails never reaches `DISPATCHED`, so it was
    /// invisible to `query_active_overlays`'s `DISPATCHED`/`ATTESTED` scope,
    /// `autonomy_secs` accumulation, and the 30-minute wedge-detection net —
    /// this dedicated counter closes that livelock gap independently of
    /// `attempt` (which already double-duties as the branch/re-roll suffix
    /// AND the `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` cap; overloading it a third
    /// time here would let raw infra retries corrupt both the `factory/<id>-r<n>`
    /// branch numbering and the recovery-cap semantics). Reset to `0` on a
    /// confirmed `DISPATCHED` save; NOT reset by `recover_human_held` (a bead
    /// that gets auto-recovered and immediately fails to spawn again must
    /// re-trip the cap quickly rather than being granted a fresh budget).
    pub spawn_failure_count: u32,
    /// Pre-remediation-session HEAD SHA of the adopted branch, captured by
    /// `reroll::execute_adopted` immediately before dispatching a coder
    /// session onto it (bead jleechan-tfs1 amendment: post-hoc force-push
    /// detection). `None` for factory-fabricated branches (never set) and
    /// for adopted beads that haven't been through a remediation dispatch
    /// yet. While the bead sits `DISPATCHED`, `tick::run_tick`'s
    /// wedge-detection sweep re-checks every tick that this SHA is still an
    /// ancestor of the branch's current tip (`Vcs::is_ancestor`) — if not,
    /// the branch was force-pushed/rewritten since capture, which is a
    /// direct violation of the append-only guarantee for adopted branches,
    /// and the bead is parked `HUMAN_HELD` with an escalation comment
    /// naming both SHAs rather than silently promoted as if remediation
    /// succeeded.
    pub pre_session_head_sha: Option<String>,
    /// Machine-readable reason the bead most recently transitioned to
    /// `HUMAN_HELD` (bead jleechan-4jn1: live incident jleechan-93ft / PR
    /// worldarchitect.ai#7888 — `recover_human_held` was requeuing
    /// circuit-breaker parks identically to transient parks like
    /// `session_stalled`, causing a 769x re-trigger loop of the same
    /// rejected fix in 30 minutes). Set alongside every `state =
    /// HumanHeld` write (`reroll::execute`/`execute_adopted`,
    /// `dispatch::dispatch_ready`, `tick::run_tick`/`run_recovery_step`).
    /// `recover_human_held` uses a canonical allow-list: only explicitly
    /// retry-safe reasons with `session_id = NULL` requeue. Unknown, legacy
    /// NULL, configuration, circuit-breaker, and possibly-live-session
    /// reasons remain held. Cleared back to `None` after a safe requeue.
    pub park_reason: Option<String>,
    /// Per-bead repo identity (bead jleechan-35y4 / Stage A of the
    /// multi-repo dispatch fix, see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`). `None` is the
    /// "legacy" case — every overlay record written before this field
    /// existed (and every bead whose intake could not resolve an explicit
    /// repo) has no column value here, which means "use the daemon's global
    /// `cfg.target_repo`" for full backward compatibility. Set by
    /// intake (`intake::resolve_target_repo`) from, in order: an explicit
    /// `target_repo:` body field, else the `owner/repo` prefix of the
    /// bead's `external_ref`, else left `None`. Call sites that need "which
    /// repo does this bead belong to" MUST go through [`BeadOverlay::repo`]
    /// rather than re-implementing this None-means-global fallback
    /// themselves — see the accessor's doc comment for why.
    pub target_repo: Option<String>,
    /// Unix-epoch seconds at which the CURRENT attempt's dispatch
    /// reservation completed successfully (`state = DISPATCHED` save).
    /// Set atomically alongside the DISPATCHED save in
    /// `dispatch::dispatch_ready` (bead bze8.3: redispatch must not inherit
    /// elapsed autonomy from prior attempts). `None` means "this attempt
    /// has not been dispatched yet" (intake, queued, or currently being
    /// dispatched); the timebox check in `tick::run_tick` falls back to
    /// cumulative `autonomy_secs` when `None` (legacy / pre-fix rows that
    /// were never re-dispatched). Reset to `None` on every `recover_human_held`
    /// (a new attempt will re-stamp it on its own successful reservation)
    /// and on every `HumanHeld` transition (the next attempt starts fresh).
    /// Stored in epoch seconds (same format as `last_er_runner_attempt_at`)
    /// so the wall-clock comparison `now_epoch - attempt_started_at`
    /// directly yields elapsed seconds without timezone parsing.
    pub attempt_started_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRoutingBinding {
    pub session_id: Option<String>,
    pub branch: Option<String>,
    pub target_repo: Option<String>,
    pub ao_project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedPrIdentity {
    pub repo: String,
    pub default_repo: String,
    pub pr_number: u64,
    pub branch: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptedPrClaim {
    Owned,
    CoalescedActive { owner_bead_id: String },
    ReplacedHumanHeld { owner_bead_id: String },
    RefusedMismatch { owner_bead_id: String, reason: String },
}

impl BeadOverlay {
    /// The single accessor for "which repo does this bead belong to".
    /// Returns the bead's explicit `target_repo` when Stage A intake
    /// resolved one, else falls back to the daemon's global
    /// `cfg.target_repo` (the pre-multi-repo, single-repo behavior).
    /// Every call site that currently reads `cfg.target_repo` directly to
    /// answer "which repo" should be migrated to call `overlay.repo(cfg)`
    /// instead (that full call-site sweep is Stage D / bead jleechan-9xrs —
    /// this accessor is the capability those call sites will adopt one at a
    /// time, not a call-site migration itself).
    pub fn repo<'a>(&'a self, cfg: &'a crate::config::Config) -> &'a str {
        self.target_repo.as_deref().unwrap_or(&cfg.target_repo)
    }
}

pub trait StateStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError>;
    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError>;
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError>;
    fn session_routing_bindings(&self) -> Result<Vec<SessionRoutingBinding>, DaemonError> {
        Ok(Vec::new())
    }
    fn save_dispatched_session(
        &self,
        overlay: &BeadOverlay,
        _ao_project: &str,
    ) -> Result<(), DaemonError> {
        self.save(overlay)
    }
    fn save_dispatch_intent(
        &self,
        overlay: &BeadOverlay,
        ao_project: &str,
    ) -> Result<(), DaemonError> {
        self.save_dispatched_session(overlay, ao_project)
    }
    /// Deletion guard: daemon may delete ONLY refs returned here (spec §4.2.8).
    fn owned_branches(&self) -> Result<Vec<String>, DaemonError>;
    /// Reverse-lookup: branch → bead_id (used by fast_tier to find drive-existing-pr beads).
    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError>;
    fn claim_adopted_pr(
        &self,
        _identity: &AdoptedPrIdentity,
        _candidate: &BeadOverlay,
    ) -> Result<AdoptedPrClaim, DaemonError> {
        Err(DaemonError::Config(
            "state store does not implement atomic adopted-PR claims".into(),
        ))
    }
    /// Return the overlay rows currently in `DISPATCHED` or `ATTESTED`. The
    /// caller decides whether (and when) to bump `autonomy_secs` per row via
    /// [`bump_autonomy_secs`] — splitting these two ops is what lets the
    /// `ci_pending` pause (jleechan-54ky / sub-fix for jleechan-gib) freeze
    /// the autonomy clock for healthy PRs that are waiting on slow CI,
    /// instead of silently burning the 3h timebox against operator/CI
    /// wall-clock time and parking the bead `HUMAN_HELD`.
    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError>;
    /// Increment a single overlay's `autonomy_secs` by `delta_secs` and
    /// refresh `updated_at`. Pair with [`list_active_overlays`] when the
    /// caller needs to skip the bump for specific rows (e.g.
    /// `ci_pending=true`); the SQL-level "increment everything" form that
    /// [`increment_active_autonomy`] used to provide is preserved below as
    /// a default-method convenience for callers that don't need the skip.
    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError>;
    /// Convenience for callers that don't need the ci_pending pause:
    /// list all active overlays AND bump their autonomy_secs by
    /// `elapsed_secs` in one call. Equivalent to `list_active_overlays`
    /// followed by `bump_autonomy_secs` for every returned row.
    fn increment_active_autonomy(
        &self,
        elapsed_secs: u64,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        let overlays = self.list_active_overlays()?;
        for overlay in &overlays {
            if elapsed_secs > 0 {
                self.bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
            }
        }
        // Re-read so the returned rows reflect the just-applied bump; this
        // matches the original "increment then return" semantics the tick
        // loop depended on for the budget-warning crossing check.
        self.list_active_overlays()
    }
    /// Requeue only explicitly retry-safe `HUMAN_HELD` beads below
    /// `max_attempt` whose durable session handle is clear. Unknown, legacy,
    /// permanent, and possibly-live-session holds fail closed.
    /// Returns the recovered overlays so the caller can emit telemetry.
    /// This is the Rust port of the shell overlay's `recover-held`
    /// (daemon/factory-overlay.sh:319), and the fix for jleechan-gib's
    /// "100% of intake dead-ends terminally" blocker: without an automated
    /// exit from `HUMAN_HELD`, every non-green gate assessment parks a
    /// bead forever.
    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        let _ = max_attempt;
        Ok(Vec::new())
    }
    /// Return `HUMAN_HELD` overlays that have reached the automated recovery
    /// cap and therefore require explicit escalation instead of silent retry
    /// suppression.
    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError>;
    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        feedback_text: &str,
    ) -> Result<(), DaemonError>;
    fn load_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<(String, String)>, DaemonError>;
    /// Read back the raw feedback text for a stored rejection (companion to
    /// `load_rejection`, which only returns `(reviewer, feedback_hash)`). The
    /// reroll circuit-breaker's semantic comparison (spec §4.2.6) needs the
    /// actual text, not just the hash, to ask the model whether two rejections
    /// describe the same underlying issue. Default `Ok(None)` so stores that
    /// predate this feature (or fakes that don't need reroll's circuit-breaker
    /// exercised) don't need to implement it; `None` means the circuit-breaker
    /// safely no-ops (never fires) rather than erroring or guessing.
    fn load_rejection_text(
        &self,
        _bead_id: &str,
        _attempt: u32,
    ) -> Result<Option<String>, DaemonError> {
        Ok(None)
    }
    /// Return the adopted remediation attempt whose worker session was
    /// successfully spawned and persisted. This marker is deliberately
    /// separate from `pre_session_head_sha`: that SHA is written before the
    /// external spawn boundary and therefore cannot prove remediation began.
    /// A missing marker means the prior attempt stopped in preflight or spawn
    /// failure and must not trip the semantic circuit breaker.
    fn remediation_session_spawned_attempt(
        &self,
        _bead_id: &str,
    ) -> Result<Option<u32>, DaemonError> {
        Ok(None)
    }
    /// Persist the durable attempt marker after an adopted remediation
    /// session has spawned and the resulting DISPATCHED overlay was saved.
    fn mark_remediation_session_spawned(
        &self,
        _bead_id: &str,
        _attempt: u32,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Atomically persist the post-spawn DISPATCHED overlay and its semantic
    /// remediation marker. Production SQLite overrides this with a single
    /// transaction; the default preserves compatibility for older fakes.
    fn save_remediation_session_spawned(
        &self,
        overlay: &BeadOverlay,
        attempt: u32,
        ao_project: &str,
    ) -> Result<(), DaemonError> {
        self.save_dispatched_session(overlay, ao_project)?;
        self.mark_remediation_session_spawned(&overlay.bead_id, attempt)
    }
    /// Read the `(attempt_count, last_attempt_epoch_secs)` pair for the
    /// `/er` runner (bead jleechan-qqq). Default impl returns `(0, None)`
    /// so test fakes that don't override it get the "never spawned" state.
    fn er_runner_attempt(&self, _bead_id: &str) -> Result<(u32, Option<u64>), DaemonError> {
        Ok((0, None))
    }
    /// Atomically increment the `/er` runner attempt counter for `bead_id`
    /// and stamp `last_attempt_epoch_secs` to `now_epoch`. Returns the
    /// new count. Default impl just returns `1` so fakes that don't
    /// override it still satisfy the call (they aren't used in production).
    fn incr_er_runner_attempt(&self, _bead_id: &str, _now_epoch: u64) -> Result<u32, DaemonError> {
        Ok(1)
    }
    /// Reset `bead_id`'s `/er` attempt counter to 0 (bead jleechan-yoqy /
    /// issue #323 r5 finding 4). Called when the PR body's evidence marker
    /// changed, so genuinely-new evidence is re-reviewed instead of being
    /// suppressed by a prior run's attempt cap. Default no-op for fakes.
    fn reset_er_runner_attempt(&self, _bead_id: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Read the evidence-marker hash recorded at `bead_id`'s last `/er` run
    /// (bead jleechan-yoqy / issue #323). `None` = no run recorded. Used by
    /// `er_runner::maybe_run` to re-trigger `/er` after an evidence-only PR
    /// body update. Default `Ok(None)` so fakes that don't exercise the
    /// retrigger path behave as "never recorded" (respect the existing verdict).
    fn last_er_evidence_hash(&self, _bead_id: &str) -> Result<Option<String>, DaemonError> {
        Ok(None)
    }
    /// Record the evidence-marker hash reviewed by `bead_id`'s latest `/er`
    /// run. Default no-op for fakes.
    fn set_er_evidence_hash(&self, _bead_id: &str, _hash: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Read the consecutive re-roll deferral count for `bead_id` (bead
    /// jleechan-zeij / issue #322 r2). Persisted in its own column rather
    /// than on [`BeadOverlay`] — like `attempt_er_runner_count`, it is a
    /// per-bead retry counter the reroll engine owns, decoupled from the
    /// overlay struct's ~100 construction sites. Default `Ok(0)` so fakes
    /// that don't exercise the fail-closed defer path see "never deferred".
    fn reroll_deferral_count(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(0)
    }
    /// Atomically increment `bead_id`'s consecutive re-roll deferral count and
    /// return the new value. Default `Ok(1)` mirrors `incr_er_runner_attempt`.
    fn incr_reroll_deferral(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(1)
    }
    /// Reset `bead_id`'s consecutive re-roll deferral count to `0` — called on
    /// a confirmed proceed so a later, unrelated re-roll starts fresh. Default
    /// `Ok(())` (no-op) for fakes that don't persist the counter.
    fn reset_reroll_deferral(&self, _bead_id: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Read `bead_id`'s consecutive PERMANENT (non-transient)
    /// `head_sha_within_for_repo` probe-failure count (bead
    /// advice-627-630-20260809 PR #628 finding 2). Persisted in its own
    /// column, decoupled from [`BeadOverlay`] — same shape as
    /// `reroll_deferral_count`, but tracks a narrower signal: only
    /// non-transient probe failures (`DaemonError::is_transient() == false`)
    /// increment it, and any successful probe resets it. Distinguishes "a
    /// bead has been silently deferring on a genuinely permanent VCS error
    /// for N ticks in a row" from ordinary transient hiccups, which never
    /// touch this counter. Default `Ok(0)` so fakes that don't exercise the
    /// permanent-failure escalation path see "never failed permanently".
    fn reroll_head_permanent_failure_count(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(0)
    }
    /// Atomically increment `bead_id`'s consecutive permanent head-probe
    /// failure count and return the new value. Default `Ok(1)` mirrors
    /// `incr_reroll_deferral`.
    fn incr_reroll_head_permanent_failure(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(1)
    }
    /// Reset `bead_id`'s consecutive permanent head-probe failure count to
    /// `0` — called on any successful probe so a later, unrelated run of
    /// permanent failures starts fresh. Default `Ok(())` (no-op) for fakes
    /// that don't persist the counter.
    fn reset_reroll_head_permanent_failure(&self, _bead_id: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Read the earliest epoch at which a bead held `DISPOSITION_REQUIRED` may
    /// be re-assessed (bead jleechan-zaga / issue #348 r3). `None` = no
    /// cooldown recorded (re-assess now). Default `Ok(None)` so fakes that
    /// don't exercise the hold-cooldown path see "re-assess now".
    fn held_recheck_after(&self, _bead_id: &str) -> Result<Option<u64>, DaemonError> {
        Ok(None)
    }
    /// Record the earliest epoch at which `bead_id` may be re-assessed while
    /// held. Default no-op for fakes.
    fn set_held_recheck_after(&self, _bead_id: &str, _epoch: u64) -> Result<(), DaemonError> {
        Ok(())
    }
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Escalation dedup query (1s2q-escalation-dedup): returns `true` if an
    /// ESCALATION_REQUIRED / ESCALATION_NOTIFICATION_FAILED event should be
    /// emitted for `(bead_id, reason)` at `now_epoch` — i.e. no prior record
    /// exists, the context hash changed, or the last emit is older than
    /// `refire_secs`. Returns `false` (suppress) when the same context was
    /// emitted within the backoff window. Default `Ok(true)` so fakes that
    /// don't persist the ledger never suppress (preserve prior behavior).
    fn escalation_should_emit(
        &self,
        _bead_id: &str,
        _reason: &str,
        _context_hash: &str,
        _now_epoch: u64,
        _refire_secs: u64,
    ) -> Result<bool, DaemonError> {
        Ok(true)
    }
    /// Record that an escalation event was just emitted for
    /// `(bead_id, reason)` with `context_hash` at `now_epoch` (upsert the
    /// ledger row). Default no-op for fakes that don't persist the ledger.
    fn record_escalation_emit(
        &self,
        _bead_id: &str,
        _reason: &str,
        _context_hash: &str,
        _now_epoch: u64,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Atomically stamp `attempt_started_at = now_epoch` and reset
    /// `autonomy_secs = 0` for the bead identified by `bead_id`. Called by
    /// `dispatch::dispatch_ready` immediately after the DISPATCHED save
    /// (bead bze8.3) so a redispatched bead cannot inherit elapsed
    /// autonomy from its prior attempt.
    fn stamp_attempt_started_at(
        &self,
        _bead_id: &str,
        _now_epoch: u64,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Clear `attempt_started_at = NULL` for the bead identified by `bead_id`.
    fn clear_attempt_started_at(&self, _bead_id: &str) -> Result<(), DaemonError> {
        Ok(())
    }
    /// jleechan-6l1f: read the boolean the daemon recorded for `bead_id`'s
    /// last gate assessment (`true` iff the last report was all_green).
    /// `None` means "never recorded" — equivalent to `false` for the
    /// regression-detection predicate (a bead that has never been green is
    /// NOT a regression candidate). Owned by `tick::run_fast_tier`, decoupled
    /// from `BeadOverlay` for the same reason as
    /// `last_er_evidence_hash`/`reroll_deferral_count`: this is a per-bead
    /// retry counter the tick engine owns, written by the regression
    /// detection path and consulted before the GATE_ASSESSMENT emit.
    /// Older DBs that pre-date this column get it via the idempotent
    /// `ensure_last_all_green_columns` migration in `SqliteStateStore::open`.
    fn last_all_green(&self, _bead_id: &str) -> Result<Option<bool>, DaemonError> {
        Ok(None)
    }
    /// jleechan-6l1f: stamp `bead_id`'s last-all_green flag. Default no-op
    /// for fakes that don't persist the column.
    fn set_last_all_green(&self, _bead_id: &str, _value: bool) -> Result<(), DaemonError> {
        Ok(())
    }
    /// jleechan-6l1f: read the cumulative green->red transition count for
    /// `bead_id`. Used to enforce `MAX_GATE_REGRESSIONS` (default 3) so a
    /// flapping check cannot ping-pong a bead through reroll forever.
    /// Default `Ok(0)` so fakes that don't exercise the cap path see
    /// "never regressed".
    fn gate_regression_count(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(0)
    }
    /// jleechan-6l1f: atomically increment `bead_id`'s regression count and
    /// return the new value. Default `Ok(1)` mirrors `incr_er_runner_attempt`.
    fn incr_gate_regression_count(&self, _bead_id: &str) -> Result<u32, DaemonError> {
        Ok(1)
    }
    /// 1s2q-escalation-dedup Task 2: mark the `(bead_id, reason)` escalation
    /// ledger row as terminal ("escalation_undeliverable") so
    /// `escalation_should_emit` returns `Ok(false)` for it on every future
    /// tick, regardless of context hash or backoff window.
    fn mark_escalation_undeliverable(
        &self,
        _bead_id: &str,
        _reason: &str,
    ) -> Result<(), DaemonError> {
        Ok(())
    }

    /// Bead jleechan-g1ib / CLAIMED tag coordination. Atomically claim
    /// `bead_id` for `machine` if no live claim exists. A claim is "live"
    /// iff `claimed_by IS NOT NULL AND claimed_at > now_epoch - ttl_secs`.
    /// Returns `Ok(true)` when this call took the claim, `Ok(false)` when a
    /// non-expired claim by `machine` (or any other machine) was already
    /// present. Distinct from `peer_claim_taken` (which checks the peer
    /// daemon's reported claims, NOT this row): a claim attempt must clear
    /// BOTH gates before this returns true. Default no-op for fakes.
    fn try_claim(
        &self,
        _bead_id: &str,
        _machine: &str,
        _now_epoch: u64,
        _ttl_secs: u64,
    ) -> Result<bool, DaemonError> {
        Ok(true)
    }

    /// Bead jleechan-g1ib: clear the local `claimed_by`/`claimed_at` for
    /// `bead_id`. No-op when the row has no claim or the claim belongs to a
    /// different machine (so a malformed `release` call cannot steal a
    /// peer's claim). Default no-op for fakes.
    fn release_claim(&self, _bead_id: &str, _machine: &str) -> Result<(), DaemonError> {
        Ok(())
    }

    /// Bead jleechan-g1ib: heartbeat — refresh the `claimed_at` of an
    /// existing claim to `now_epoch`. Returns `false` when no claim exists
    /// (caller should `try_claim` instead). Default no-op for fakes.
    fn heartbeat_claim(
        &self,
        _bead_id: &str,
        _machine: &str,
        _now_epoch: u64,
        _ttl_secs: u64,
    ) -> Result<bool, DaemonError> {
        Ok(true)
    }

    /// Bead jleechan-g1ib: snapshot every live local claim. Default empty
    /// for fakes (no production code path needs the fake to return data).
    fn list_live_local_claims(
        &self,
        _now_epoch: u64,
        _ttl_secs: u64,
    ) -> Result<Vec<(String, u64, u64)>, DaemonError> {
        Ok(Vec::new())
    }

    /// Bead jleechan-g1ib: replace the cached peer-claim set with `claims`
    /// (each `(machine, bead_id, claimed_at, expires_at)`). Old rows that
    /// are NOT in `claims` are dropped (the peer reports its full live set
    /// every sync). `last_synced_at` is stamped to `now_epoch`. Default
    /// no-op for fakes.
    fn replace_peer_claims(
        &self,
        _claims: &[(String, String, u64, u64)],
        _now_epoch: u64,
    ) -> Result<(), DaemonError> {
        Ok(())
    }

    /// Bead jleechan-g1ib: return true if any peer-reported claim on
    /// `bead_id` is still live (expires_at > now_epoch). Default false.
    fn peer_claim_taken(&self, _bead_id: &str, _now_epoch: u64) -> Result<bool, DaemonError> {
        Ok(false)
    }

    /// Bead jleechan-g1ib: combined dispatch gate — true iff the LOCAL
    /// overlay says `bead_id` is held by a different machine within the TTL
    /// window (i.e. another machine owns this bead, drop it from the
    /// dispatch queue). Defaults to false (no claim, dispatch allowed) so
    /// fakes that don't exercise the path see the pre-gate behavior. The
    /// dispatch loop in `tick::run_slow_tier` consults this just before
    /// `dispatch_ready`.
    fn claim_blocks_dispatch(
        &self,
        _bead_id: &str,
        _now_epoch: u64,
        _ttl_secs: u64,
        _self_machine: &str,
    ) -> Result<bool, DaemonError> {
        Ok(false)
    }
}

/// `StateStore` impl against `~/.dark-factory/daemon-cxdb.sqlite` (WAL mode,
/// 5s busy_timeout — same discipline as `runner/cxdb.py`, but a separate DB
/// file: no cross-language schema coupling with the Python runner's CXDB).
pub struct SqliteStateStore {
    conn: Connection,
}

fn tool_err(op: &str, e: rusqlite::Error) -> DaemonError {
    DaemonError::Tool {
        tool: "sqlite".into(),
        rc: -1,
        stderr: format!("{op}: {e}"),
    }
}

pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Minimal dependency-free ISO-8601 UTC formatting (no chrono per design doc §2's
    // five-dependency budget: rusqlite, serde, serde_json, toml, thiserror).
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

pub const CIRCUIT_BREAKER_PARK_REASON: &str =
    "circuit-breaker triggered: same reviewer and feedback hash as prior attempt";
const ROUTER_PARSE_PARK_REASON_PREFIX: &str = "router_parse_error:";
const ROUTER_ERROR_PARK_REASON_PREFIX: &str = "router_error:";

pub enum HumanHoldReason {
    TransientSpawnRetryCapExceeded,
    /// jleechan-vu3k: a spawn attempt failed because AO itself is running
    /// but the target project is not in its active/polled membership
    /// (`DaemonError::is_ao_not_polling_project()`), distinct from a
    /// generic transient spawn failure. Bare retry composes the identical
    /// spawn request against the identical non-polling AO instance every
    /// time and can never succeed, so this parks IMMEDIATELY on first
    /// detection rather than burning `MAX_TRANSIENT_SPAWN_RETRY` attempts
    /// first (live incident: attempt=10, spawn_failure_count=25, parked
    /// under the generic `transient_spawn_retry_cap_exceeded` reason with
    /// no operator-facing signal that the real fix is an AO-level action,
    /// not a bead-level retry). Permanent — NOT in
    /// `recoverable_exact_values()`, since a plain requeue replays the
    /// exact same non-fix. An operator must ensure a running AO instance
    /// has this project in its active membership (e.g. `ao start`) before
    /// requeuing.
    AoOrchestratorNotRunning,
    AdoptedPreSessionShaCaptureFailed,
    SessionStalled,
    Stage1GateNotGreen,
    SpecValidationFailed,
    RouterParse(String),
    /// A non-`Parse` error from `router::route()` (e.g. `DaemonError::Tool`
    /// or `DaemonError::Timeout` bubbling up from the underlying
    /// `Llm::judge` call). This used to propagate via `?` straight out of
    /// `run_slow_tier`'s routing loop, aborting the ENTIRE tick's routing
    /// phase the instant any single candidate's judge call failed —
    /// silently starving every candidate after it, for every repo, with no
    /// ERROR telemetry (the error is typically `is_transient()`, so the
    /// outer tick loop just retries next tick — forever, if the same
    /// candidate keeps failing first). Parking just this one bead lets the
    /// loop continue to the next candidate instead. Recoverable via the
    /// same prefix-matching discipline as `RouterParse`: the underlying
    /// failure is usually a transient tool/LLM call, so a plain requeue on
    /// the next recovery sweep is safe.
    RouterError(String),
    UnmappedTargetRepo,
    TargetCheckoutUnconfigured,
    /// Bead jleechan-8jxr r2: a manually-created factory bead whose intake
    /// could not resolve ANY repo identity (no `target_repo:` body field,
    /// no `external_ref` with a parseable `owner/repo#N` prefix, and no
    /// adopted-PR context) reached `dispatch_ready` with `overlay.target_repo
    /// = None`. `BeadOverlay::repo()` previously fell back to
    /// `cfg.target_repo`, so the bead silently dispatched into the daemon's
    /// global default repo — even when the bead's body content was
    /// unambiguously about a DIFFERENT repo (e.g. dark-factory internals
    /// while `cfg.target_repo = jleechanorg/worldarchitect.ai`). Confirmed
    /// 5x on 2026-07-18 (beads yvfe/vmy2/46dk/s9ba/txtd → worldarchitect.ai
    /// PRs #8424-#8427 and session wa-3294). Fail-closed: park the bead so
    /// an operator or refiling agent can supply an explicit
    /// `external_ref` or `target_repo:` body field before any worker is
    /// spawned. Permanent (NOT in `recoverable_exact_values()`) — silent
    /// requeue would just re-park with the same failure mode forever.
    UnmappedRepo,
    WorktreeRemoteMismatch,
    WorktreeRemoteUnverifiable,
    SpawnCleanupFailed,
    SpawnFailed,
    SpawnBranchMismatch,
    AmbiguousDispatchingRecovery,
    AutonomyTimeboxExceeded,
    AdoptedBranchHistoryRewriteDetected,
    AdoptedBranchAppendOnlyCheckFailed,
    SessionBranchMismatch,
    CoderSilent,
    RerollSessionAttachFailed,
    RerollSessionStopFailed,
    RerollQuiescenceCheckFailed,
    RerollQuiescenceTimeout,
    /// Bead jleechan-zeij / issue #322 r2: the fail-closed re-roll proceed
    /// predicate deferred this bead the maximum number of consecutive ticks
    /// without ever confirming the previous worker was safe to supersede
    /// (an active session, a moving branch HEAD, or a failed `stop()` every
    /// time). Only at this bounded cap does deferral escalate to a park —
    /// unlike `RerollQuiescenceTimeout` (the removed r0 behavior), a single
    /// unconfirmed poll never parks.
    RerollQuiescenceDeferralCapExceeded,
    /// Bead jleechan-zeij / issue #322 r4 P1: `reroll::execute` returned a
    /// PERMANENT (non-`is_transient()`) error. `execute` persists `RE_ROLL`
    /// before the failure, and the fast tier only re-selects `ATTESTED`
    /// overlays, so a permanent error that the tick loop merely logged-and-
    /// continued would strand the bead in `RE_ROLL` forever (invisible to
    /// recovery). The tick boundary parks it `HUMAN_HELD` with this reason
    /// instead — loud and operator-visible. Not in the auto-recover allow-list
    /// (a permanent error needs a human), unlike transient reroll errors which
    /// keep their log-and-retry-next-tick behavior.
    RerollPermanentError,
    AdoptedMissingBranch,
    AdoptedQuiescenceCheckFailed,
    AdoptedSessionAttachFailed,
    AdoptedSessionAlreadyActive,
    AdoptedSpawnFailed,
    /// The trusted remediation prompt metadata alone exceeded the bounded
    /// AO payload budget. This is permanent until an operator or policy
    /// change addresses the oversized reviewer/branch metadata; silently
    /// retrying would reproduce the same pre-dispatch failure.
    RemediationPromptOverBudget,
    UnknownOnlyGateCapped,
    CircuitBreaker,
    /// Bead jleechan-6l1f: gate regression hit `MAX_GATE_REGRESSIONS`
    /// consecutive green->red transitions. Distinct park_reason so
    /// `recover_human_held` (which allow-lists only retry-safe reasons)
    /// does NOT auto-requeue a flapping bead. NOT in
    /// `recoverable_exact_values()` — the cap is terminal, the operator
    /// must inspect the bead (or the underlying flaky gate) before any
    /// further automated attempt.
    GateRegressionCapped,
    /// jleechan-dp0b (PR #627 /advice finding 2): `dispatch_ready` parks a
    /// bead HUMAN_HELD when `register_branch` rejects a branch with a
    /// non-transient error (a genuine cross-bead branch collision in the
    /// state store, discovered BEFORE any worker is spawned). This is a
    /// distinct failure category from `SpawnBranchMismatch` (a worker
    /// spawn-time mismatch between the branch the daemon told AO to use and
    /// the branch the live session actually bound to) — reusing that reason
    /// mislabeled the park. NOT in `recoverable_exact_values()`: a genuine
    /// branch-registry collision needs an operator to resolve the
    /// conflicting bead/branch assignment, not a bare requeue.
    BranchRegistrationConflict,
    EscalationLocalFallback(String),
    /// Bead jleechan-jw4c: a worker session was spawned with a
    /// `local_checkout` cwd but the actual child process's cwd did not
    /// match the assigned worktree path. Permanent — the silent-acceptance
    /// failure mode is what the bead's RED measurement (worker writing to
    /// shared checkout while assigned worktree existed) describes, so a
    /// bare requeue would replay the same leak. Excluded from
    /// `recoverable_exact_values()`.
    WorktreeCwdMismatch,
}

impl HumanHoldReason {
    pub fn value(&self) -> String {
        match self {
            Self::TransientSpawnRetryCapExceeded => "transient_spawn_retry_cap_exceeded",
            Self::AoOrchestratorNotRunning => "ao_orchestrator_not_running",
            Self::AdoptedPreSessionShaCaptureFailed => "adopted_pre_session_sha_capture_failed",
            Self::SessionStalled => "session_stalled",
            Self::Stage1GateNotGreen => {
                "gate assessment not all-green (stage 1: recorded, not executed)"
            }
            Self::SpecValidationFailed => "spec file validation failed in recovery",
            Self::RouterParse(reason) => {
                return format!("{ROUTER_PARSE_PARK_REASON_PREFIX} {reason}");
            }
            Self::RouterError(reason) => {
                return format!("{ROUTER_ERROR_PARK_REASON_PREFIX} {reason}");
            }
            Self::UnmappedTargetRepo => "unmapped_target_repo",
            Self::TargetCheckoutUnconfigured => "target_checkout_unconfigured",
            Self::UnmappedRepo => "unmapped_repo",
            Self::WorktreeRemoteMismatch => "worktree_remote_mismatch",
            Self::WorktreeRemoteUnverifiable => "worktree_remote_unverifiable",
            Self::SpawnCleanupFailed => "spawn_cleanup_failed",
            Self::SpawnFailed => "spawn_failed",
            Self::SpawnBranchMismatch => "spawn_branch_mismatch",
            Self::AmbiguousDispatchingRecovery => "ambiguous_dispatching_recovery",
            Self::AutonomyTimeboxExceeded => "autonomy_timebox_exceeded",
            Self::AdoptedBranchHistoryRewriteDetected => "adopted_branch_history_rewrite_detected",
            Self::AdoptedBranchAppendOnlyCheckFailed => "adopted_branch_append_only_check_failed",
            Self::SessionBranchMismatch => "session_branch_mismatch",
            Self::CoderSilent => "coder_silent",
            Self::RerollSessionAttachFailed => "reroll_session_attach_failed",
            Self::RerollSessionStopFailed => "reroll_session_stop_failed",
            Self::RerollQuiescenceCheckFailed => "reroll_quiescence_check_failed",
            Self::RerollQuiescenceTimeout => "reroll_quiescence_timeout",
            Self::RerollQuiescenceDeferralCapExceeded => "reroll_quiescence_deferral_cap_exceeded",
            Self::RerollPermanentError => "reroll_permanent_error",
            Self::AdoptedMissingBranch => "adopted_missing_branch",
            Self::AdoptedQuiescenceCheckFailed => "adopted_quiescence_check_failed",
            Self::AdoptedSessionAttachFailed => "adopted_session_attach_failed",
            Self::AdoptedSessionAlreadyActive => "adopted_session_already_active",
            Self::AdoptedSpawnFailed => "adopted_spawn_failed",
            Self::RemediationPromptOverBudget => "remediation_prompt_over_budget",
            Self::UnknownOnlyGateCapped => "unknown_only_gate_report_with_er_runner_capped",
            Self::CircuitBreaker => CIRCUIT_BREAKER_PARK_REASON,
            Self::GateRegressionCapped => "gate_regression_capped",
            Self::BranchRegistrationConflict => "branch_registration_conflict",
            Self::EscalationLocalFallback(reason) => {
                return format!("escalation_local_fallback:{reason}");
            }
            Self::WorktreeCwdMismatch => "worktree_cwd_mismatch",
        }
        .to_string()
    }

    fn is_recoverable_value(reason: &str) -> bool {
        Self::recoverable_exact_values()
            .iter()
            .any(|candidate| candidate == reason)
            || reason.starts_with(ROUTER_PARSE_PARK_REASON_PREFIX)
            || reason.starts_with(ROUTER_ERROR_PARK_REASON_PREFIX)
    }

    /// This park can now auto-recover because operators confirmed the
    /// affected repos resolve cleanly in config today, so a plain requeue is
    /// no longer a replay loop and is safe for existing stuck beads.
    fn recoverable_exact_values() -> [String; 6] {
        [
            Self::TransientSpawnRetryCapExceeded,
            Self::AdoptedPreSessionShaCaptureFailed,
            Self::SessionStalled,
            Self::Stage1GateNotGreen,
            Self::SpecValidationFailed,
            Self::TargetCheckoutUnconfigured,
        ]
        .map(|candidate| candidate.value())
    }
}

pub fn set_human_hold_reason(overlay: &mut BeadOverlay, reason: HumanHoldReason) {
    overlay.park_reason = Some(reason.value());
}

/// Human-held recovery is an allow-list, not a best-effort retry policy.
/// Unknown and legacy NULL reasons fail closed. Even a declared recoverable
/// reason is requeued only when its durable overlay carries no session handle;
/// the park transition must clear that handle in the same save after positive
/// no-spawn/terminal evidence.
pub fn is_permanent_human_hold_reason(reason: Option<&str>) -> bool {
    !reason.is_some_and(HumanHoldReason::is_recoverable_value)
}

/// Howard Hinnant's civil_from_days algorithm (public domain), days since epoch -> (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl SqliteStateStore {
    /// Open (or create) the on-disk store at `path`, apply WAL + busy_timeout=5000,
    /// and ensure the schema from `contracts/schema.sql` exists.
    pub fn open(path: &Path) -> Result<Self, DaemonError> {
        let conn = Connection::open(path).map_err(|e| tool_err("open", e))?;
        Self::configure(&conn, false)?;
        conn.execute_batch(include_str!("../contracts/schema.sql"))
            .map_err(|e| tool_err("apply schema", e))?;
        Self::ensure_er_runner_columns(&conn)?;
        Self::ensure_is_adopted_column(&conn)?;
        Self::ensure_spawn_failure_count_column(&conn)?;
        Self::ensure_pre_session_head_sha_column(&conn)?;
        Self::ensure_park_reason_column(&conn)?;
        Self::ensure_target_repo_column(&conn)?;
        Self::ensure_ao_project_column(&conn)?;
        Self::ensure_session_ao_project_column(&conn)?;
        Self::ensure_reroll_deferral_count_column(&conn)?;
        Self::ensure_reroll_head_permanent_failure_count_column(&conn)?;
        Self::ensure_held_recheck_after_column(&conn)?;
        Self::ensure_last_er_evidence_hash_column(&conn)?;
        Self::ensure_last_all_green_columns(&conn)?;
        Self::ensure_remediation_session_spawned_table(&conn)?;
        Self::ensure_disposition_required_state(&conn)?;
        Self::ensure_escalation_ledger_table(&conn)?;
        Self::ensure_escalation_ledger_terminal_column(&conn)?;
        Self::ensure_claimed_by_columns(&conn)?;
        Self::ensure_peer_claims_table(&conn)?;
        Self::ensure_attempt_started_at_column(&conn)?;
        Ok(Self { conn })
    }

    /// In-memory store for tests: same pragmas, schema supplied by the caller
    /// (normally `include_str!("../contracts/schema.sql")`).
    pub fn open_in_memory_with_schema(schema_sql: &str) -> Result<Self, DaemonError> {
        let conn = Connection::open_in_memory().map_err(|e| tool_err("open", e))?;
        Self::configure(&conn, true)?;
        conn.execute_batch(schema_sql)
            .map_err(|e| tool_err("apply schema", e))?;
        Self::ensure_er_runner_columns(&conn)?;
        Self::ensure_is_adopted_column(&conn)?;
        Self::ensure_spawn_failure_count_column(&conn)?;
        Self::ensure_pre_session_head_sha_column(&conn)?;
        Self::ensure_park_reason_column(&conn)?;
        Self::ensure_target_repo_column(&conn)?;
        Self::ensure_ao_project_column(&conn)?;
        Self::ensure_session_ao_project_column(&conn)?;
        Self::ensure_reroll_deferral_count_column(&conn)?;
        Self::ensure_reroll_head_permanent_failure_count_column(&conn)?;
        Self::ensure_held_recheck_after_column(&conn)?;
        Self::ensure_last_er_evidence_hash_column(&conn)?;
        Self::ensure_last_all_green_columns(&conn)?;
        Self::ensure_remediation_session_spawned_table(&conn)?;
        Self::ensure_disposition_required_state(&conn)?;
        Self::ensure_escalation_ledger_table(&conn)?;
        Self::ensure_escalation_ledger_terminal_column(&conn)?;
        Self::ensure_claimed_by_columns(&conn)?;
        Self::ensure_peer_claims_table(&conn)?;
        Self::ensure_attempt_started_at_column(&conn)?;
        Ok(Self { conn })
    }

    /// Idempotent migration for the multi-machine claim columns (bead
    /// jleechan-g1ib: CLAIMED tag coordination). Adds `claimed_by TEXT` and
    /// `claimed_at INTEGER` to `bead_overlay`. The dispatch loop skips rows
    /// where `claimed_by IS NOT NULL AND claimed_at > now - ttl_secs`, so a
    /// machine crash that left a stale claim eventually frees the bead.
    /// Nullable, defaulted to NULL ("no claim"). Pre-existing rows
    /// legitimately have no claim set.
    fn ensure_claimed_by_columns(conn: &Connection) -> Result<(), DaemonError> {
        let has_claimed_by: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'claimed_by'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_claimed_by_columns: pragma by", e))?;
        if !has_claimed_by {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN claimed_by TEXT", [])
                .map_err(|e| tool_err("ensure_claimed_by_columns: add by", e))?;
        }
        let has_claimed_at: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'claimed_at'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_claimed_by_columns: pragma at", e))?;
        if !has_claimed_at {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN claimed_at INTEGER", [])
                .map_err(|e| tool_err("ensure_claimed_by_columns: add at", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `peer_claims` table (bead jleechan-g1ib).
    /// Stores the last-known-claim set the peer daemon reported via /sync, so
    /// a second machine can refuse a claim locally if the peer already holds
    /// it (within the grace window). Probes `sqlite_master` then
    /// `CREATE TABLE IF NOT EXISTS` — same idempotent pattern as
    /// `ensure_escalation_ledger_table`.
    fn ensure_peer_claims_table(conn: &Connection) -> Result<(), DaemonError> {
        let has_table: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'peer_claims'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_peer_claims_table: probe", e))?;
        if !has_table {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS peer_claims (\
                   machine        TEXT NOT NULL,\
                   bead_id        TEXT NOT NULL,\
                   claimed_at     INTEGER NOT NULL,\
                   expires_at     INTEGER NOT NULL,\
                   last_synced_at INTEGER NOT NULL,\
                   PRIMARY KEY (machine, bead_id)\
                 )",
            )
            .map_err(|e| tool_err("ensure_peer_claims_table: create", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `/er` runner columns (bead jleechan-qqq).
    /// Older on-disk DBs predate the columns declared in the CREATE TABLE
    /// block; SQLite has no `ADD COLUMN IF NOT EXISTS`, so we probe
    /// `pragma_table_info` first and only ALTER when a column is missing.
    /// Safe to call repeatedly — a no-op when both columns are already
    /// present.
    fn ensure_er_runner_columns(conn: &Connection) -> Result<(), DaemonError> {
        let has_count: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'attempt_er_runner_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: pragma count", e))?;
        if !has_count {
            conn.execute(
                "ALTER TABLE bead_overlay \
                 ADD COLUMN attempt_er_runner_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: add count", e))?;
        }
        let has_last: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_runner_attempt_at'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: pragma last", e))?;
        if !has_last {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN last_er_runner_attempt_at INTEGER",
                [],
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: add last", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `is_adopted` column (bead jleechan-tfs1).
    /// Same `pragma_table_info` probe-then-`ALTER` pattern as
    /// `ensure_er_runner_columns` — older on-disk DBs predate the column
    /// declared in the CREATE TABLE block, and SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`. Safe to call repeatedly; defaults every
    /// pre-existing row to `0` (factory-fabricated), which is the correct
    /// conservative default since only adopted-PR intake ever sets it `1`.
    fn ensure_is_adopted_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_is_adopted: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'is_adopted'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_is_adopted_column: pragma", e))?;
        if !has_is_adopted {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN is_adopted INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_is_adopted_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `spawn_failure_count` column (follow-up
    /// to #198's dispatch-batch-isolation fix). Same probe-then-`ALTER`
    /// pattern as `ensure_is_adopted_column`. Defaults every pre-existing
    /// row to `0` — conservative, since a bead already `DISPATCHED` has by
    /// definition not accumulated any unresolved transient spawn failures.
    fn ensure_spawn_failure_count_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_spawn_failure_count: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'spawn_failure_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_spawn_failure_count_column: pragma", e))?;
        if !has_spawn_failure_count {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN spawn_failure_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_spawn_failure_count_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `pre_session_head_sha` column (bead
    /// jleechan-tfs1 amendment: post-hoc force-push detection). Same
    /// probe-then-`ALTER` pattern as `ensure_is_adopted_column`. Nullable —
    /// every pre-existing row (and every non-adopted row) legitimately has
    /// no baseline SHA.
    fn ensure_pre_session_head_sha_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'pre_session_head_sha'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_pre_session_head_sha_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN pre_session_head_sha TEXT",
                [],
            )
            .map_err(|e| tool_err("ensure_pre_session_head_sha_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `park_reason` column (bead jleechan-4jn1:
    /// live incident jleechan-93ft / PR worldarchitect.ai#7888). Same
    /// probe-then-`ALTER` pattern as `ensure_is_adopted_column`. Nullable —
    /// every pre-existing row (and every row that has never been parked)
    /// legitimately has no reason recorded.
    fn ensure_park_reason_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'park_reason'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_park_reason_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN park_reason TEXT", [])
                .map_err(|e| tool_err("ensure_park_reason_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `target_repo` column (bead jleechan-35y4,
    /// Stage A of the multi-repo dispatch fix). Same probe-then-`ALTER`
    /// pattern as `ensure_park_reason_column`. Nullable, and every
    /// pre-existing row legitimately has no value here — `None` means "use
    /// the global `cfg.target_repo`" (see `BeadOverlay::repo`), which is
    /// exactly the pre-migration single-repo behavior every existing row
    /// implicitly had.
    fn ensure_target_repo_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'target_repo'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_target_repo_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN target_repo TEXT", [])
                .map_err(|e| tool_err("ensure_target_repo_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the per-session Agent Orchestrator project.
    /// Nullable so rows written before session-level routing was introduced
    /// continue to load as `None`.
    fn ensure_session_ao_project_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'session_ao_project'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_session_ao_project_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN session_ao_project TEXT",
                [],
            )
            .map_err(|e| tool_err("ensure_session_ao_project_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `reroll_deferral_count` column (bead
    /// jleechan-zeij / issue #322 r2). Same probe-then-`ALTER` pattern as
    /// `ensure_target_repo_column`. The consecutive-defer counter the
    /// fail-closed re-roll proceed predicate uses; every pre-existing row
    /// correctly defaults to `0` ("never deferred").
    fn ensure_reroll_deferral_count_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'reroll_deferral_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_reroll_deferral_count_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN reroll_deferral_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_reroll_deferral_count_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `reroll_head_permanent_failure_count`
    /// column (bead advice-627-630-20260809 PR #628 finding 2). Same
    /// probe-then-`ALTER` pattern as `ensure_reroll_deferral_count_column`.
    /// The consecutive-PERMANENT-head-probe-failure counter
    /// `reroll::evaluate_proceed` uses to escalate a loud warning once a
    /// bead has been silently deferring on a genuinely non-transient VCS
    /// error for `reroll_head_permanent_fail_threshold()` ticks in a row;
    /// every pre-existing row correctly defaults to `0` ("never failed
    /// permanently").
    fn ensure_reroll_head_permanent_failure_count_column(
        conn: &Connection,
    ) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'reroll_head_permanent_failure_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| {
                tool_err("ensure_reroll_head_permanent_failure_count_column: pragma", e)
            })?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN \
                 reroll_head_permanent_failure_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| {
                tool_err(
                    "ensure_reroll_head_permanent_failure_count_column: add column",
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Idempotent migration for the `held_recheck_after` column (bead
    /// jleechan-zaga / issue #348 r3). Same probe-then-`ALTER` pattern as
    /// `ensure_reroll_deferral_count_column`. Nullable (NULL = "re-assess
    /// now"). MUST run before `ensure_disposition_required_state` so the
    /// table's column set is complete before the CHECK rebuild.
    fn ensure_held_recheck_after_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'held_recheck_after'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_held_recheck_after_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN held_recheck_after INTEGER",
                [],
            )
            .map_err(|e| tool_err("ensure_held_recheck_after_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `last_er_evidence_hash` column (bead
    /// jleechan-yoqy / issue #323). Same probe-then-`ALTER` pattern. Nullable
    /// (NULL = "no /er run recorded"). MUST run before
    /// `ensure_disposition_required_state` so the column is present for the
    /// CHECK rebuild's column-intersection copy.
    fn ensure_last_er_evidence_hash_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_evidence_hash'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_last_er_evidence_hash_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN last_er_evidence_hash TEXT",
                [],
            )
            .map_err(|e| tool_err("ensure_last_er_evidence_hash_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the regression-detection columns (bead
    /// jleechan-6l1f). Two columns added in one migration because they are
    /// always written together by the fast tier (last_all_green stamps the
    /// latest assessment, gate_regression_count bumps on the green->red
    /// transition). Defaults match the regression-detection predicate:
    /// `last_all_green` defaults to 0 (= "never green" — the predicate
    /// treats this as `false`, which is the safe default since a bead that
    /// has never been green cannot be a regression candidate). Same probe
    /// pattern as the other `ensure_*_column` helpers above (SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`).
    fn ensure_last_all_green_columns(conn: &Connection) -> Result<(), DaemonError> {
        let has_last_all_green: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_all_green'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_last_all_green_columns: pragma last_all_green", e))?;
        if !has_last_all_green {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN last_all_green INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_last_all_green_columns: add last_all_green", e))?;
        }
        let has_gate_regression_count: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'gate_regression_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| {
                tool_err(
                    "ensure_last_all_green_columns: pragma gate_regression_count",
                    e,
                )
            })?;
        if !has_gate_regression_count {
            conn.execute(
                "ALTER TABLE bead_overlay \
                 ADD COLUMN gate_regression_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| {
                tool_err(
                    "ensure_last_all_green_columns: add gate_regression_count",
                    e,
                )
            })?;
        }
        Ok(())
    }

    /// Idempotent migration for the adopted-remediation lifecycle marker.
    /// Unlike `pre_session_head_sha`, which is intentionally persisted before
    /// crossing the external AO spawn boundary, this table is written only
    /// after a successful spawn and a successful DISPATCHED overlay save.
    fn ensure_remediation_session_spawned_table(conn: &Connection) -> Result<(), DaemonError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS remediation_session_spawned (\
               bead_id TEXT PRIMARY KEY,\
               attempt INTEGER NOT NULL,\
               updated_at TEXT NOT NULL\
             )",
        )
        .map_err(|e| tool_err("ensure_remediation_session_spawned_table", e))?;
        Ok(())
    }

    /// Idempotent migration for the `escalation_ledger` table
    /// (1s2q-escalation-dedup). Unlike the `ensure_*_column` migrations above
    /// (which probe `pragma_table_info` for a column), this probes
    /// `sqlite_master` for the table's existence, then issues
    /// `CREATE TABLE IF NOT EXISTS` (idempotent on its own, but the probe keeps
    /// the migration log honest for legacy DBs that already ran the old
    /// schema.sql before this table was added). Safe to call repeatedly.
    /// Runs AFTER `ensure_disposition_required_state` since it is an
    /// independent table (NOT a column on `bead_overlay`) and therefore does
    /// NOT participate in that CHECK rebuild's column-intersection copy.
    fn ensure_escalation_ledger_table(conn: &Connection) -> Result<(), DaemonError> {
        let has_table: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_table: probe", e))?;
        if !has_table {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS escalation_ledger (\
                   bead_id           TEXT NOT NULL,\
                   reason            TEXT NOT NULL,\
                   context_hash      TEXT NOT NULL,\
                   last_emitted_epoch INTEGER NOT NULL,\
                   PRIMARY KEY (bead_id, reason)\
                 )",
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_table: create", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `escalation_ledger.terminal` column
    /// (1s2q-escalation-dedup Task 2). Older on-disk DBs that got the
    /// `escalation_ledger` table from a pre-Task-2 `ensure_escalation_ledger_table`
    /// predate the `terminal` column declared in the CREATE TABLE block; SQLite
    /// has no `ADD COLUMN IF NOT EXISTS`, so we probe `pragma_table_info` first
    /// and only ALTER when the column is missing. Safe to call repeatedly — a
    /// no-op when the column is already present. Defaults every pre-existing
    /// row to `0` (not terminal), preserving the pre-Task-2 dedup behavior for
    /// rows written before the terminal concept existed.
    fn ensure_escalation_ledger_terminal_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('escalation_ledger') \
                 WHERE name = 'terminal'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_terminal_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE escalation_ledger \
                 ADD COLUMN terminal INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_terminal_column: add column", e))?;
        }
        Ok(())
    }

    /// Canonical `bead_overlay` column list (in `schema.sql` order). The
    /// DISPOSITION_REQUIRED CHECK rebuild uses this to build the new table and
    /// to copy data by EXPLICIT column name. Keep in sync with `schema.sql`'s
    /// CREATE TABLE and the `ensure_*_column` migrations above.
    const BEAD_OVERLAY_COLUMNS: &'static [&'static str] = &[
        "bead_id",
        "state",
        "attempt",
        "reroll_count",
        "autonomy_secs",
        "spend_usd",
        "pr_number",
        "branch",
        "session_id",
        "updated_at",
        "attempt_er_runner_count",
        "last_er_runner_attempt_at",
        "is_adopted",
        "spawn_failure_count",
        "pre_session_head_sha",
        "park_reason",
        "target_repo",
        "ao_project",
        "session_ao_project",
        "reroll_deferral_count",
        "held_recheck_after",
        "last_er_evidence_hash",
    ];

    /// The canonical `CREATE TABLE bead_overlay` statement (with the current
    /// state CHECK list, incl. `DISPOSITION_REQUIRED`) as shipped in
    /// `schema.sql`, but under a temp name for the rebuild. Hardcoded rather
    /// than transformed from the live DDL so the migration is robust to
    /// whitespace variants, quoted identifiers, and any other legal DDL
    /// formatting that a string-edit would silently break.
    const REBUILD_TABLE_DDL: &'static str = "CREATE TABLE bead_overlay_disposition_migrated (\
        bead_id TEXT PRIMARY KEY, \
        state TEXT NOT NULL CHECK (state IN \
            ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',\
             'REDISPATCHED','BUDGET_HELD','HUMAN_HELD','DISPOSITION_REQUIRED')), \
        attempt INTEGER NOT NULL DEFAULT 1, \
        reroll_count INTEGER NOT NULL DEFAULT 0, \
        autonomy_secs INTEGER NOT NULL DEFAULT 0, \
        spend_usd REAL NOT NULL DEFAULT 0, \
        pr_number INTEGER, \
        branch TEXT, \
        session_id TEXT, \
        updated_at TEXT NOT NULL, \
        attempt_er_runner_count INTEGER NOT NULL DEFAULT 0, \
        last_er_runner_attempt_at INTEGER, \
        is_adopted INTEGER NOT NULL DEFAULT 0, \
        spawn_failure_count INTEGER NOT NULL DEFAULT 0, \
        pre_session_head_sha TEXT, \
        park_reason TEXT, \
        target_repo TEXT, \
        ao_project TEXT, \
        session_ao_project TEXT, \
        reroll_deferral_count INTEGER NOT NULL DEFAULT 0, \
        held_recheck_after INTEGER, \
        last_er_evidence_hash TEXT)";

    /// Bead jleechan-zaga / issue #348: migrate the `bead_overlay.state` CHECK
    /// constraint to allow `'DISPOSITION_REQUIRED'`. Unlike every other
    /// migration above (which add nullable/defaulted COLUMNS via `ALTER TABLE
    /// … ADD COLUMN`), this changes a CHECK constraint — and SQLite supports
    /// no `ALTER TABLE … DROP/ALTER CONSTRAINT`. `CREATE TABLE IF NOT EXISTS`
    /// in `schema.sql` is a NO-OP on a live DB that already has the table, so
    /// it never updates the constraint: a live daemon started against a
    /// pre-#348 DB would hit `CHECK constraint failed` the first time it tried
    /// to persist a `DISPOSITION_REQUIRED` bead. The only portable fix is the
    /// documented SQLite table-rebuild dance (create-copy-drop-rename).
    ///
    /// Robust migration (r3): the need to migrate is detected by PROBING —
    /// attempting a `DISPOSITION_REQUIRED` INSERT inside a savepoint that is
    /// always rolled back — NOT by string-matching the stored DDL (fragile to
    /// whitespace / quoted identifiers). When a migration IS needed, the new
    /// table is built from a CANONICAL hardcoded CREATE (`REBUILD_TABLE_DDL`),
    /// and data is copied by EXPLICIT column name for the intersection of the
    /// live table's columns and the canonical set — never a positional
    /// `SELECT *` against a transformed copy of the old DDL. Runs after every
    /// `ensure_*_column` migration so the live column set is complete.
    fn ensure_disposition_required_state(conn: &Connection) -> Result<(), DaemonError> {
        // Probe: is `DISPOSITION_REQUIRED` already accepted by the live CHECK?
        // Attempt the INSERT inside a savepoint we ALWAYS roll back so the
        // probe row never persists; a `CHECK constraint failed` means migrate.
        conn.execute_batch("SAVEPOINT drp_probe")
            .map_err(|e| tool_err("ensure_disposition_required_state: savepoint", e))?;
        let probe = conn.execute(
            "INSERT INTO bead_overlay (bead_id, state, updated_at) \
             VALUES ('__drp_migration_probe__', 'DISPOSITION_REQUIRED', '')",
            [],
        );
        conn.execute_batch("ROLLBACK TO drp_probe; RELEASE drp_probe")
            .map_err(|e| tool_err("ensure_disposition_required_state: rollback probe", e))?;
        let needs_migration = match probe {
            Ok(_) => false, // CHECK already allows it (fresh/already-migrated DB).
            Err(ref e) if e.to_string().to_ascii_lowercase().contains("check constraint") => true,
            Err(e) => {
                // A different failure (e.g. a table shape we don't understand)
                // — do not attempt a rebuild we can't reason about; surface it.
                return Err(tool_err("ensure_disposition_required_state: probe", e));
            }
        };
        if !needs_migration {
            return Ok(());
        }

        // Copy only columns that exist in BOTH the live table and the
        // canonical schema, by explicit name (order-independent).
        let mut live_cols: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('bead_overlay')")
                .map_err(|e| tool_err("ensure_disposition_required_state: pragma prepare", e))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| tool_err("ensure_disposition_required_state: pragma query", e))?;
            for name in rows {
                live_cols
                    .insert(name.map_err(|e| tool_err("ensure_disposition_required_state: pragma row", e))?);
            }
        }
        let copy_cols: Vec<&str> = Self::BEAD_OVERLAY_COLUMNS
            .iter()
            .copied()
            .filter(|c| live_cols.contains(*c))
            .collect();
        let col_list = copy_cols.join(", ");

        let batch = format!(
            "BEGIN IMMEDIATE;\n\
             {create};\n\
             INSERT INTO bead_overlay_disposition_migrated ({cols}) SELECT {cols} FROM bead_overlay;\n\
             DROP TABLE bead_overlay;\n\
             ALTER TABLE bead_overlay_disposition_migrated RENAME TO bead_overlay;\n\
             COMMIT;",
            create = Self::REBUILD_TABLE_DDL,
            cols = col_list,
        );
        if let Err(e) = conn.execute_batch(&batch) {
            // Best-effort rollback so a half-applied rebuild doesn't wedge the
            // next open; surface the original error either way.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(tool_err("ensure_disposition_required_state: rebuild", e));
        }
        Ok(())
    }
    /// Idempotent migration for the `attempt_started_at` column (bead
    /// bze8.3: redispatch must not inherit elapsed autonomy from prior
    /// attempts). Same probe-then-`ALTER` pattern as
    /// `ensure_reroll_deferral_count_column`. Nullable — every
    /// pre-existing row legitimately has no value here (the anchor is
    /// stamped atomically by the next successful dispatch reservation;
    /// the timebox check falls back to cumulative `autonomy_secs` while
    /// the column is NULL on legacy rows that have not been re-dispatched
    /// since this column existed). Default `NULL` is the safe value
    /// because the timebox code is explicitly written to fall back to
    /// `autonomy_secs` when the anchor is absent, never to guess.
    fn ensure_attempt_started_at_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'attempt_started_at'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_attempt_started_at_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN attempt_started_at INTEGER",
                [],
            )
            .map_err(|e| tool_err("ensure_attempt_started_at_column: add column", e))?;
        }
        Ok(())
    }

    fn ensure_ao_project_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'ao_project'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_ao_project_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN ao_project TEXT", [])
                .map_err(|e| tool_err("ensure_ao_project_column: add column", e))?;
        }
        Ok(())
    }

    /// `is_memory` distinguishes the two `configure` call sites: `open()` (file-backed,
    /// `is_memory=false`) and `open_in_memory_with_schema()` (`is_memory=true`). WAL is a
    /// documented no-op against `:memory:` connections, so failures/non-"wal" readbacks are
    /// ignored there; on a real file, a failure to switch journal modes (unsupported
    /// filesystem, permissions, NFS, etc.) must not be silently swallowed (jleechan-8in).
    fn configure(conn: &Connection, is_memory: bool) -> Result<(), DaemonError> {
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(|e| tool_err("busy_timeout", e))?;
        let mode: rusqlite::Result<String> =
            conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0));
        if is_memory {
            // WAL is a no-op (and briefly errors) against :memory: connections; ignore there.
            return Ok(());
        }
        match mode {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
            Ok(mode) => Err(DaemonError::Config(format!(
                "PRAGMA journal_mode=WAL did not take effect on file-backed DB: got \"{mode}\""
            ))),
            Err(e) => Err(DaemonError::Config(format!(
                "PRAGMA journal_mode=WAL failed on file-backed DB: {e}"
            ))),
        }
    }

    /// Shared SELECT for `list_active_overlays` and `increment_active_autonomy`'s
    /// "read after bump" return value. Filters to `DISPATCHED` + `ATTESTED`
    /// (the only states where autonomy_secs is allowed to accumulate per
    /// spec §4.2.8).
    fn query_active_overlays(&self, op: &str) -> Result<Vec<BeadOverlay>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo, session_ao_project, attempt_started_at \
                 FROM bead_overlay WHERE state IN ('DISPATCHED', 'ATTESTED')",
            )
            .map_err(|e| tool_err(&format!("{op} prepare"), e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                ))
            })
            .map_err(|e| tool_err(&format!("{op} query"), e))?;
        let mut out = Vec::new();
        for r in rows {
            let (
                bead_id,
                state_str,
                attempt,
                reroll_count,
                autonomy_secs,
                spend_usd,
                pr_number,
                branch,
                session_id,
                is_adopted,
                spawn_failure_count,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at,
            ) = r.map_err(|e| tool_err(&format!("{op} row"), e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at: attempt_started_at.map(|v| v.max(0) as u64),
            });
        }
        Ok(out)
    }
}

fn save_overlay_conn(conn: &Connection, overlay: &BeadOverlay) -> Result<(), DaemonError> {
    let session_ao_project = overlay
        .session_id
        .as_ref()
        .and(overlay.session_ao_project.as_ref());
    conn.execute(
        "INSERT INTO bead_overlay \
         (bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, updated_at, is_adopted, spawn_failure_count, pre_session_head_sha, park_reason, target_repo, session_ao_project, attempt_started_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
         ON CONFLICT(bead_id) DO UPDATE SET \
           state=excluded.state, attempt=excluded.attempt, reroll_count=excluded.reroll_count, \
           autonomy_secs=excluded.autonomy_secs, spend_usd=excluded.spend_usd, \
           pr_number=excluded.pr_number, branch=excluded.branch, session_id=excluded.session_id, updated_at=excluded.updated_at, \
           is_adopted=excluded.is_adopted, spawn_failure_count=excluded.spawn_failure_count, pre_session_head_sha=excluded.pre_session_head_sha, \
           park_reason=excluded.park_reason, target_repo=excluded.target_repo, session_ao_project=excluded.session_ao_project, attempt_started_at=excluded.attempt_started_at",
        params![
            overlay.bead_id,
            overlay.state.as_str(),
            overlay.attempt,
            overlay.reroll_count,
            overlay.autonomy_secs,
            overlay.spend_usd,
            overlay.pr_number.map(|v| v as i64),
            overlay.branch,
            overlay.session_id,
            now_iso8601(),
            overlay.is_adopted as i64,
            overlay.spawn_failure_count,
            overlay.pre_session_head_sha,
            overlay.park_reason,
            overlay.target_repo,
            session_ao_project,
            overlay.attempt_started_at.map(|v| v as i64),
        ],
    )
    .map_err(|e| tool_err("save", e))?;
    Ok(())
}

impl StateStore for SqliteStateStore {
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        let reason = HumanHoldReason::AmbiguousDispatchingRecovery.value();
        self.conn
            .execute(
                "UPDATE bead_overlay \
                 SET state = 'HUMAN_HELD', park_reason = ?1 \
                 WHERE state = 'DISPATCHING'",
                params![reason],
            )
            .map_err(|e| tool_err("reconcile_dispatching", e))?;
        Ok(())
    }

    fn session_routing_bindings(&self) -> Result<Vec<SessionRoutingBinding>, DaemonError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, branch, target_repo, ao_project FROM bead_overlay \
             WHERE session_id IS NOT NULL OR branch IS NOT NULL",
        ).map_err(|e| tool_err("session_routing_bindings prepare", e))?;
        let rows = stmt.query_map([], |row| Ok(SessionRoutingBinding {
            session_id: row.get(0)?,
            branch: row.get(1)?,
            target_repo: row.get(2)?,
            ao_project: row.get(3)?,
        })).map_err(|e| tool_err("session_routing_bindings query", e))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| tool_err("session_routing_bindings row", e))
    }

    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
        self.conn
            .query_row(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo, session_ao_project, attempt_started_at \
                 FROM bead_overlay WHERE bead_id = ?1",
                params![bead_id],
                |row| {
                    let state_str: String = row.get(1)?;
                    let is_adopted: i64 = row.get(9)?;
                    let spawn_failure_count: i64 = row.get(10)?;
                    let pre_session_head_sha: Option<String> = row.get(11)?;
                    let park_reason: Option<String> = row.get(12)?;
                    let target_repo: Option<String> = row.get(13)?;
                    let session_ao_project: Option<String> = row.get(14)?;
                    let attempt_started_at: Option<i64> = row.get(15)?;
                    Ok((
                        state_str,
                        BeadOverlay {
                            bead_id: row.get(0)?,
                            state: OverlayState::Queued, // placeholder, replaced below
                            attempt: row.get(2)?,
                            reroll_count: row.get(3)?,
                            autonomy_secs: row.get(4)?,
                            spend_usd: row.get(5)?,
                            pr_number: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                            branch: row.get(7)?,
                            session_id: row.get(8)?,
                            is_adopted: is_adopted != 0,
                            spawn_failure_count: spawn_failure_count as u32,
                            pre_session_head_sha,
                            park_reason,
                            target_repo,
                            session_ao_project,
                            attempt_started_at: attempt_started_at.map(|v| v.max(0) as u64),
                        },
                    ))
                },
            )
            .optional()
            .map_err(|e| tool_err("load", e))?
            .map(|(state_str, mut overlay)| {
                overlay.state = OverlayState::from_str(&state_str)?;
                Ok(overlay)
            })
            .transpose()
    }

    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
        save_overlay_conn(&self.conn, overlay)
    }

    fn save_dispatched_session(
        &self,
        overlay: &BeadOverlay,
        ao_project: &str,
    ) -> Result<(), DaemonError> {
        self.conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| tool_err("save_dispatched_session begin", e))?;
        let result = (|| {
            save_overlay_conn(&self.conn, overlay)?;
            self.conn.execute(
                "UPDATE bead_overlay SET ao_project = ?2 WHERE bead_id = ?1",
                params![overlay.bead_id, ao_project],
            ).map_err(|e| tool_err("save_dispatched_session project", e))?;
            Ok::<(), DaemonError>(())
        })();
        match result {
            Ok(()) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(tool_err("save_dispatched_session commit", error))
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn stamp_attempt_started_at(
        &self,
        bead_id: &str,
        now_epoch: u64,
    ) -> Result<(), DaemonError> {
        // Bead bze8.3: stamped AFTER the DISPATCHED save so a redispatch
        // (a bead whose prior attempt was previously parked with a stale
        // `autonomy_secs` carrying over) starts the budget clock at the
        // successful-reservation moment, not at the moment of an attempt
        // that the daemon has since rejected. Same column-tolerance idiom
        // as `incr_er_runner_attempt`: a missing column on a legacy DB
        // is a no-op (the timebox code falls back to cumulative
        // `autonomy_secs` in that case, which is the pre-fix behavior).
        let res = self.conn.execute(
            "UPDATE bead_overlay SET \
                attempt_started_at = ?2, autonomy_secs = 0, updated_at = ?3 \
             WHERE bead_id = ?1",
            params![bead_id, now_epoch as i64, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("stamp_attempt_started_at", e)),
        }
    }

    fn clear_attempt_started_at(&self, bead_id: &str) -> Result<(), DaemonError> {
        // Bead bze8.3: called on HUMAN_HELD transitions and inside
        // `recover_human_held`. `NULL` means "no live attempt" — the
        // timebox check never consults cumulative `autonomy_secs` once an
        // attempt is parked, but the explicit NULL keeps the row honest if
        // the bead is ever re-dispatched (the next reservation re-stamps
        // the anchor atomically). Same legacy-DB tolerance as
        // `stamp_attempt_started_at`.
        let res = self.conn.execute(
            "UPDATE bead_overlay SET attempt_started_at = NULL, updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("clear_attempt_started_at", e)),
        }
    }

    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
        if let Some(existing) = self.bead_id_for_branch(branch)? {
            if existing == bead_id {
                return Ok(());
            }
            return Err(DaemonError::Config(format!(
                "branch {branch} is already registered to bead {existing}; refusing to reassign to {bead_id}"
            )));
        }
        self.conn
            .execute(
                "INSERT INTO branch_registry (branch, bead_id, created_at) VALUES (?1, ?2, ?3)",
                params![branch, bead_id, now_iso8601()],
            )
            .map_err(|e| tool_err("register_branch", e))?;
        Ok(())
    }

    fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare("SELECT branch FROM branch_registry ORDER BY branch")
            .map_err(|e| tool_err("owned_branches prepare", e))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| tool_err("owned_branches query", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| tool_err("owned_branches row", e))?);
        }
        Ok(out)
    }

    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare("SELECT bead_id FROM branch_registry WHERE branch = ?1")
            .map_err(|e| tool_err("bead_id_for_branch prepare", e))?;
        let mut rows = stmt
            .query_map(params![branch], |row| row.get::<_, String>(0))
            .map_err(|e| tool_err("bead_id_for_branch query", e))?;
        if let Some(Ok(bead)) = rows.next() {
            Ok(Some(bead))
        } else {
            Ok(None)
        }
    }

    fn claim_adopted_pr(
        &self,
        identity: &AdoptedPrIdentity,
        candidate: &BeadOverlay,
    ) -> Result<AdoptedPrClaim, DaemonError> {
        if identity.repo.is_empty()
            || identity.branch.is_empty()
            || identity.head_sha.is_empty()
            || identity.pr_number == 0
            || candidate.bead_id.is_empty()
        {
            return Err(DaemonError::Config(
                "adopted PR claim requires non-empty repo/branch/head/bead and positive PR".into(),
            ));
        }
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| tool_err("claim_adopted_pr begin", e))?;
        let result = (|| {
            let owner: Option<String> = self
                .conn
                .query_row(
                    "SELECT bead_id FROM branch_registry WHERE branch = ?1",
                    params![identity.branch],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| tool_err("claim_adopted_pr owner", e))?;
            let binding: Option<(String, u64, String, String)> = self
                .conn
                .query_row(
                    "SELECT repo, pr_number, head_sha, bead_id FROM adopted_pr_binding WHERE branch = ?1",
                    params![identity.branch],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| tool_err("claim_adopted_pr binding", e))?;

            let insert_binding = |owner_bead_id: &str| -> Result<(), DaemonError> {
                self.conn
                    .execute(
                        "INSERT INTO adopted_pr_binding \
                         (branch, repo, pr_number, head_sha, bead_id, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                         ON CONFLICT(branch) DO UPDATE SET repo=excluded.repo, \
                         pr_number=excluded.pr_number, head_sha=excluded.head_sha, \
                         bead_id=excluded.bead_id, updated_at=excluded.updated_at",
                        params![
                            identity.branch,
                            identity.repo,
                            identity.pr_number,
                            identity.head_sha,
                            owner_bead_id,
                            now_iso8601(),
                        ],
                    )
                    .map_err(|e| tool_err("claim_adopted_pr bind", e))?;
                Ok(())
            };

            let Some(owner_bead_id) = owner else {
                self.conn
                    .execute(
                        "INSERT INTO branch_registry (branch, bead_id, created_at) VALUES (?1, ?2, ?3)",
                        params![identity.branch, candidate.bead_id, now_iso8601()],
                    )
                    .map_err(|e| tool_err("claim_adopted_pr register", e))?;
                insert_binding(&candidate.bead_id)?;
                self.save(candidate)?;
                return Ok(AdoptedPrClaim::Owned);
            };

            let owner_overlay = self.load(&owner_bead_id)?;
            let tuple_matches = match &binding {
                Some((repo, pr_number, head_sha, bound_owner)) => {
                    repo == &identity.repo
                        && *pr_number == identity.pr_number
                        && !head_sha.is_empty()
                        && bound_owner == &owner_bead_id
                }
                None => owner_overlay.as_ref().is_some_and(|overlay| {
                    let owner_repo = overlay
                        .target_repo
                        .as_deref()
                        .unwrap_or(&identity.default_repo);
                    owner_repo == identity.repo
                        && overlay.pr_number == Some(identity.pr_number)
                        && overlay.branch.as_deref() == Some(identity.branch.as_str())
                }),
            };
            if !tuple_matches {
                return Ok(AdoptedPrClaim::RefusedMismatch {
                    owner_bead_id,
                    reason: "stored owner identity does not match repo/PR/branch/exact head".into(),
                });
            }
            if owner_bead_id == candidate.bead_id {
                self.save(candidate)?;
                insert_binding(&owner_bead_id)?;
                return Ok(AdoptedPrClaim::Owned);
            }
            let Some(owner_overlay) = owner_overlay else {
                return Ok(AdoptedPrClaim::RefusedMismatch {
                    owner_bead_id,
                    reason: "registered owner overlay is missing and legacy identity is unprovable".into(),
                });
            };
            // The stable identity is repo + PR + branch + owner. A new
            // authoritative intake snapshot may legitimately advance the
            // same PR to a new head, so refresh that mutable proof inside
            // the same transaction only after the retained owner is proven.
            insert_binding(&owner_bead_id)?;
            if owner_overlay.state == OverlayState::HumanHeld && owner_overlay.session_id.is_none() {
                let changed = self
                    .conn
                    .execute(
                        "UPDATE branch_registry SET bead_id = ?1, created_at = ?2 \
                         WHERE branch = ?3 AND bead_id = ?4",
                        params![candidate.bead_id, now_iso8601(), identity.branch, owner_bead_id],
                    )
                    .map_err(|e| tool_err("claim_adopted_pr replace", e))?;
                if changed != 1 {
                    return Err(DaemonError::Config(
                        "adopted PR owner changed during atomic claim".into(),
                    ));
                }
                insert_binding(&candidate.bead_id)?;
                self.save(candidate)?;
                return Ok(AdoptedPrClaim::ReplacedHumanHeld { owner_bead_id });
            }
            if matches!(
                owner_overlay.state,
                OverlayState::Queued
                    | OverlayState::Dispatching
                    | OverlayState::Dispatched
                    | OverlayState::Attested
                    | OverlayState::ReRoll
                    | OverlayState::Recovery
                    | OverlayState::Redispatched
            ) {
                return Ok(AdoptedPrClaim::CoalescedActive { owner_bead_id });
            }
            Ok(AdoptedPrClaim::RefusedMismatch {
                owner_bead_id,
                reason: format!(
                    "registered owner state {} is not replaceable",
                    owner_overlay.state.as_str()
                ),
            })
        })();
        match result {
            Ok(claim) => {
                match self.conn.execute_batch("COMMIT") {
                    Ok(()) => Ok(claim),
                    Err(error) => {
                        let _ = self.conn.execute_batch("ROLLBACK");
                        Err(tool_err("claim_adopted_pr commit", error))
                    }
                }
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn increment_active_autonomy(
        &self,
        elapsed_secs: u64,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        // Default-method wiring in the trait calls list_active_overlays +
        // bump_autonomy_secs; we still need to override here so the bump
        // happens via the existing single-UPDATE path (cheaper than
        // per-row updates when no caller asks for the ci_pending skip).
        if elapsed_secs > 0 {
            self.conn
                .execute(
                    "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + ?1, updated_at = ?2 \
                 WHERE state IN ('DISPATCHED', 'ATTESTED')",
                    params![elapsed_secs, now_iso8601()],
                )
                .map_err(|e| tool_err("increment_active_autonomy update", e))?;
        }
        self.list_active_overlays()
    }

    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.query_active_overlays("list_active_overlays")
    }

    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError> {
        if delta_secs == 0 {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + ?1, updated_at = ?2 \
                 WHERE bead_id = ?3",
                params![delta_secs, now_iso8601(), bead_id],
            )
            .map_err(|e| tool_err("bump_autonomy_secs", e))?;
        Ok(())
    }

    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        // Recovery is a single atomic UPDATE ... RETURNING statement. The
        // write-time predicate revalidates state, attempt cap, typed reason,
        // and absence of a durable session handle immediately before the
        // mutation. A prior SELECT-then-UPDATE-by-id implementation allowed
        // another WAL connection to attach a live session between statements;
        // recovery would then erase that handle and requeue a duplicate worker.
        //
        // bead jleechan-4jn1 (live incident jleechan-93ft / PR
        // worldarchitect.ai#7888): `park_reason LIKE 'circuit-breaker%'`
        // rows are EXCLUDED from automatic requeue. The circuit breaker
        // (reroll.rs) parks a bead HUMAN_HELD specifically to STOP retrying
        // after the same reviewer rejects the same underlying issue twice
        // in a row — treating that park identically to a retry-safe one
        // caused a 769x
        // re-trigger loop of the same rejected fix in 30 minutes in
        // production. Recovery is now fail closed: only declared retry-safe
        // reasons with a durably cleared session handle may requeue. NULL,
        // unknown, and possibly-live-session rows remain HUMAN_HELD.
        //
        // bead jleechan-35y4 (adversarial review of PR #245): `park_reason =
        // 'unmapped_target_repo'` rows are EXCLUDED for the same reason —
        // an unmapped repo is a CONFIG problem (missing `[repos.*]` entry),
        // not something a bare requeue can fix. Without this exclusion the
        // bead would ping-pong HUMAN_HELD -> QUEUED -> re-park identically
        // every recovery cycle (attempt incrementing each time, up to the
        // `max_attempt` cap) instead of staying HUMAN_HELD until an
        // operator adds the missing config entry — silently defeating the
        // "fail loud, never guess" intent `dispatch::dispatch_ready`'s
        // unmapped-repo park is supposed to provide.
        //
        // bead jleechan-bqdv Stage C: `park_reason = 'worktree_remote_mismatch'`
        // rows are EXCLUDED for the same reason as `unmapped_target_repo` —
        // a worktree that cloned with the wrong remote is either a local
        // checkout/config problem (the AO project's repo clone itself is
        // misconfigured) or a genuine near-miss that needs a human's eyes
        // before any coder is spawned into that worktree again; it is never
        // something a bare requeue alone fixes, and mirroring
        // `unmapped_target_repo`'s auto-requeue exclusion keeps the two
        // "spawn-time fail loud" park reasons behaviorally consistent.
        // `worktree_remote_unverifiable` is the same safety class: absence
        // of inspectable remote evidence must not become permission to
        // respawn into the same opaque workspace on the next recovery pass.
        // `spawn_cleanup_failed` is also permanent: the daemon knows a
        // session may still be live because `ao session kill` failed. Auto-
        // requeueing that row would create a duplicate worker while erasing
        // the retained session identity needed for operator cleanup.
        // `ambiguous_dispatching_recovery` is the startup fail-safe for a
        // process crash or state-write failure after spawn may have begun.
        // The retained branch/session fields are the operator's recovery
        // handle; automatically requeueing would risk a duplicate worker.
        // Clear stale PR metadata on requeue.
        // `pr_number` and `session_id` belong to the prior (failed) attempt
        // and would otherwise be carried into the new dispatch — `dispatch_ready`
        // overwrites `branch` but leaves the other fields, so the fast tier
        // would treat the freshly-DISPATCHED row as already ATTESTED against
        // the dead PR and re-park on the same gate. `branch` is kept so the
        // recovered-from telemetry still records what was being worked on;
        // dispatch will rewrite it on the next attempt.
        let recoverable = HumanHoldReason::recoverable_exact_values();
        let now = now_iso8601();
        let mut stmt = self
            .conn
            .prepare(
                "UPDATE bead_overlay \
             SET state = 'QUEUED', attempt = attempt + 1, autonomy_secs = 0, \
                 pr_number = NULL, session_id = NULL, session_ao_project = NULL, park_reason = NULL, \
                 attempt_started_at = NULL, updated_at = ?1 \
             WHERE state = 'HUMAN_HELD' \
               AND attempt < ?2 \
               AND session_id IS NULL \
               AND (park_reason IN (?3, ?4, ?5, ?6, ?7, ?8) \
                    OR substr(park_reason, 1, length(?9)) = ?9 \
                    OR substr(park_reason, 1, length(?10)) = ?10) \
             RETURNING bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, \
                 pre_session_head_sha, park_reason, target_repo, session_ao_project, attempt_started_at",
            )
            .map_err(|e| tool_err("recover_human_held prepare", e))?;
        let rows = stmt
            .query_map(
                params![
                    now,
                    max_attempt as i64,
                    &recoverable[0],
                    &recoverable[1],
                    &recoverable[2],
                    &recoverable[3],
                    &recoverable[4],
                    &recoverable[5],
                    ROUTER_PARSE_PARK_REASON_PREFIX,
                    ROUTER_ERROR_PARK_REASON_PREFIX,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, f64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, Option<i64>>(15)?,
                    ))
                },
            )
            .map_err(|e| tool_err("recover_human_held query", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (
                bead_id,
                state_str,
                attempt,
                reroll_count,
                autonomy_secs,
                spend_usd,
                pr_number,
                branch,
                session_id,
                is_adopted,
                spawn_failure_count,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at,
            ) = r.map_err(|e| tool_err("recover_human_held row", e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at: attempt_started_at.map(|v| v.max(0) as u64),
            });
        }
        Ok(out)
    }

    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo, session_ao_project, attempt_started_at \
                 FROM bead_overlay WHERE state = 'HUMAN_HELD' AND attempt >= ?1",
            )
            .map_err(|e| tool_err("human_held_at_or_above_attempt prepare", e))?;
        let rows = stmt
            .query_map(params![max_attempt as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                ))
            })
            .map_err(|e| tool_err("human_held_at_or_above_attempt query", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (
                bead_id,
                state_str,
                attempt,
                reroll_count,
                autonomy_secs,
                spend_usd,
                pr_number,
                branch,
                session_id,
                is_adopted,
                spawn_failure_count,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at,
            ) = r.map_err(|e| tool_err("human_held_at_or_above_attempt row", e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
                session_ao_project,
                attempt_started_at: attempt_started_at.map(|v| v.max(0) as u64),
            });
        }
        Ok(out)
    }

    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        feedback_text: &str,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO review_rejection (bead_id, attempt, reviewer, feedback_hash, feedback_text, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(bead_id, attempt) DO UPDATE SET \
                   reviewer=excluded.reviewer, feedback_hash=excluded.feedback_hash, \
                   feedback_text=excluded.feedback_text, created_at=excluded.created_at",
                params![
                    bead_id,
                    attempt,
                    reviewer,
                    feedback_hash,
                    feedback_text,
                    now_iso8601(),
                ],
            )
            .map_err(|e| tool_err("save_rejection", e))?;
        Ok(())
    }

    fn load_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<(String, String)>, DaemonError> {
        self.conn
            .query_row(
                "SELECT reviewer, feedback_hash FROM review_rejection WHERE bead_id = ?1 AND attempt = ?2",
                params![bead_id, attempt],
                |row| {
                    let reviewer: String = row.get(0)?;
                    let feedback_hash: String = row.get(1)?;
                    Ok((reviewer, feedback_hash))
                },
            )
            .optional()
            .map_err(|e| tool_err("load_rejection", e))
    }

    fn load_rejection_text(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<String>, DaemonError> {
        self.conn
            .query_row(
                "SELECT feedback_text FROM review_rejection WHERE bead_id = ?1 AND attempt = ?2",
                params![bead_id, attempt],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| tool_err("load_rejection_text", e))
    }

    fn remediation_session_spawned_attempt(
        &self,
        bead_id: &str,
    ) -> Result<Option<u32>, DaemonError> {
        self.conn
            .query_row(
                "SELECT attempt FROM remediation_session_spawned WHERE bead_id = ?1",
                params![bead_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.map(|attempt| attempt.max(0) as u32))
            .map_err(|e| tool_err("remediation_session_spawned_attempt", e))
    }

    fn mark_remediation_session_spawned(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO remediation_session_spawned (bead_id, attempt, updated_at) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(bead_id) DO UPDATE SET \
                   attempt = excluded.attempt, updated_at = excluded.updated_at",
                params![bead_id, attempt, now_iso8601()],
            )
            .map_err(|e| tool_err("mark_remediation_session_spawned", e))?;
        Ok(())
    }

    fn save_remediation_session_spawned(
        &self,
        overlay: &BeadOverlay,
        attempt: u32,
        ao_project: &str,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| tool_err("save_remediation_session_spawned begin", e))?;
        let result = (|| {
            save_overlay_conn(&self.conn, overlay)?;
            self.conn.execute(
                "UPDATE bead_overlay SET ao_project = ?2 WHERE bead_id = ?1",
                params![overlay.bead_id, ao_project],
            ).map_err(|e| tool_err("save_remediation_session_spawned project", e))?;
            self.conn
                .execute(
                    "INSERT INTO remediation_session_spawned (bead_id, attempt, updated_at) \
                     VALUES (?1, ?2, ?3) \
                     ON CONFLICT(bead_id) DO UPDATE SET \
                       attempt = excluded.attempt, updated_at = excluded.updated_at",
                    params![overlay.bead_id, attempt, now_iso8601()],
                )
                .map_err(|e| tool_err("save_remediation_session_spawned marker", e))?;
            Ok::<(), DaemonError>(())
        })();
        match result {
            Ok(()) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(tool_err("save_remediation_session_spawned commit", error))
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn er_runner_attempt(&self, bead_id: &str) -> Result<(u32, Option<u64>), DaemonError> {
        // Schema migration: the columns were added after the initial release;
        // older DB files won't have them. Detect that by attempting the
        // SELECT and falling back to (0, None) if the column is missing
        // (sqlite returns "no such column" rather than an empty result).
        let row: Result<(i64, Option<i64>), rusqlite::Error> = self.conn.query_row(
            "SELECT attempt_er_runner_count, last_er_runner_attempt_at \
             FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match row {
            Ok((count, last_at)) => Ok((count.max(0) as u32, last_at.map(|v| v.max(0) as u64))),
            Err(e) if no_such_column(&e) => Ok((0, None)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, None)),
            Err(e) => Err(tool_err("er_runner_attempt", e)),
        }
    }

    fn incr_er_runner_attempt(&self, bead_id: &str, now_epoch: u64) -> Result<u32, DaemonError> {
        // Try the UPDATE first (modern schema). If the column is missing
        // (legacy DB), fall back to no-op + return 1 so the runner's
        // attempt cap still fires on the in-memory counter.
        let res = self.conn.execute(
            "UPDATE bead_overlay SET \
                attempt_er_runner_count = COALESCE(attempt_er_runner_count, 0) + 1, \
                last_er_runner_attempt_at = ?2, \
                updated_at = ?3 \
             WHERE bead_id = ?1",
            params![bead_id, now_epoch as i64, now_iso8601()],
        );
        match res {
            Ok(_) => {
                let (count, _) = self.er_runner_attempt(bead_id)?;
                Ok(count)
            }
            Err(e) if no_such_column(&e) => Ok(1),
            Err(e) => Err(tool_err("incr_er_runner_attempt", e)),
        }
    }

    fn reset_er_runner_attempt(&self, bead_id: &str) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET attempt_er_runner_count = 0, updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("reset_er_runner_attempt", e)),
        }
    }

    fn reroll_deferral_count(&self, bead_id: &str) -> Result<u32, DaemonError> {
        // Same legacy-DB tolerance as `er_runner_attempt`: a pre-migration DB
        // lacks the column, so a "no such column" SELECT error means "never
        // deferred" (0) rather than a hard failure.
        let row: Result<i64, rusqlite::Error> = self.conn.query_row(
            "SELECT reroll_deferral_count FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(count) => Ok(count.max(0) as u32),
            Err(e) if no_such_column(&e) => Ok(0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(tool_err("reroll_deferral_count", e)),
        }
    }

    fn incr_reroll_deferral(&self, bead_id: &str) -> Result<u32, DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET \
                reroll_deferral_count = COALESCE(reroll_deferral_count, 0) + 1, \
                updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => self.reroll_deferral_count(bead_id),
            // Legacy DB without the column: fall back to 1 so the deferral
            // cap still fires (mirrors `incr_er_runner_attempt`).
            Err(e) if no_such_column(&e) => Ok(1),
            Err(e) => Err(tool_err("incr_reroll_deferral", e)),
        }
    }

    fn reset_reroll_deferral(&self, bead_id: &str) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET reroll_deferral_count = 0, updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("reset_reroll_deferral", e)),
        }
    }

    fn reroll_head_permanent_failure_count(&self, bead_id: &str) -> Result<u32, DaemonError> {
        // Same legacy-DB tolerance as `reroll_deferral_count`: a
        // pre-migration DB lacks the column, so a "no such column" SELECT
        // error means "never failed permanently" (0) rather than a hard
        // failure.
        let row: Result<i64, rusqlite::Error> = self.conn.query_row(
            "SELECT reroll_head_permanent_failure_count FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(count) => Ok(count.max(0) as u32),
            Err(e) if no_such_column(&e) => Ok(0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(tool_err("reroll_head_permanent_failure_count", e)),
        }
    }

    fn incr_reroll_head_permanent_failure(&self, bead_id: &str) -> Result<u32, DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET \
                reroll_head_permanent_failure_count = \
                    COALESCE(reroll_head_permanent_failure_count, 0) + 1, \
                updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => self.reroll_head_permanent_failure_count(bead_id),
            // Legacy DB without the column: fall back to 1 so the
            // escalation threshold still fires (mirrors `incr_reroll_deferral`).
            Err(e) if no_such_column(&e) => Ok(1),
            Err(e) => Err(tool_err("incr_reroll_head_permanent_failure", e)),
        }
    }

    fn reset_reroll_head_permanent_failure(&self, bead_id: &str) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET reroll_head_permanent_failure_count = 0, updated_at = ?2 \
             WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("reset_reroll_head_permanent_failure", e)),
        }
    }

    fn held_recheck_after(&self, bead_id: &str) -> Result<Option<u64>, DaemonError> {
        let row: Result<Option<i64>, rusqlite::Error> = self.conn.query_row(
            "SELECT held_recheck_after FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(v) => Ok(v.map(|n| n.max(0) as u64)),
            Err(e) if no_such_column(&e) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(tool_err("held_recheck_after", e)),
        }
    }

    fn set_held_recheck_after(&self, bead_id: &str, epoch: u64) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET held_recheck_after = ?2, updated_at = ?3 WHERE bead_id = ?1",
            params![bead_id, epoch as i64, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("set_held_recheck_after", e)),
        }
    }

    fn last_er_evidence_hash(&self, bead_id: &str) -> Result<Option<String>, DaemonError> {
        let row: Result<Option<String>, rusqlite::Error> = self.conn.query_row(
            "SELECT last_er_evidence_hash FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(v) => Ok(v),
            Err(e) if no_such_column(&e) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(tool_err("last_er_evidence_hash", e)),
        }
    }

    fn set_er_evidence_hash(&self, bead_id: &str, hash: &str) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET last_er_evidence_hash = ?2, updated_at = ?3 WHERE bead_id = ?1",
            params![bead_id, hash, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("set_er_evidence_hash", e)),
        }
    }

    fn last_all_green(&self, bead_id: &str) -> Result<Option<bool>, DaemonError> {
        let row: Result<Option<i64>, rusqlite::Error> = self.conn.query_row(
            "SELECT last_all_green FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(v) => Ok(v.map(|b| b != 0)),
            Err(e) if no_such_column(&e) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(tool_err("last_all_green", e)),
        }
    }

    fn set_last_all_green(&self, bead_id: &str, value: bool) -> Result<(), DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET last_all_green = ?2, updated_at = ?3 WHERE bead_id = ?1",
            params![bead_id, value as i64, now_iso8601()],
        );
        match res {
            Ok(_) => Ok(()),
            Err(e) if no_such_column(&e) => Ok(()),
            Err(e) => Err(tool_err("set_last_all_green", e)),
        }
    }

    fn gate_regression_count(&self, bead_id: &str) -> Result<u32, DaemonError> {
        let row: Result<Option<i64>, rusqlite::Error> = self.conn.query_row(
            "SELECT gate_regression_count FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| row.get(0),
        );
        match row {
            Ok(v) => Ok(v.unwrap_or(0).max(0) as u32),
            Err(e) if no_such_column(&e) => Ok(0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(tool_err("gate_regression_count", e)),
        }
    }

    fn incr_gate_regression_count(&self, bead_id: &str) -> Result<u32, DaemonError> {
        let res = self.conn.execute(
            "UPDATE bead_overlay SET gate_regression_count = COALESCE(gate_regression_count, 0) + 1, updated_at = ?2 WHERE bead_id = ?1",
            params![bead_id, now_iso8601()],
        );
        match res {
            Ok(_) => {
                // Re-read so callers get the canonical post-increment value
                // (mirrors `incr_er_runner_attempt`'s SELECT-after-UPDATE
                // pattern).
                self.gate_regression_count(bead_id)
            }
            Err(e) if no_such_column(&e) => Ok(1),
            Err(e) => Err(tool_err("incr_gate_regression_count", e)),
        }
    }

    fn escalation_should_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
        refire_secs: u64,
    ) -> Result<bool, DaemonError> {
        let row: Result<(String, i64, i64), rusqlite::Error> = self.conn.query_row(
            "SELECT context_hash, last_emitted_epoch, terminal FROM escalation_ledger \
             WHERE bead_id = ?1 AND reason = ?2",
            params![bead_id, reason],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        );
        match row {
            // No prior record — emit.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
            Err(e) => Err(tool_err("escalation_should_emit: load", e)),
            Ok((prior_hash, last_epoch, terminal)) => {
                // 1s2q-escalation-dedup Task 2: a terminal row means the
                // escalation was classified undeliverable (permanent gh
                // error). Never re-emit, regardless of hash or backoff.
                if terminal != 0 {
                    return Ok(false);
                }
                // Hash changed — re-emit regardless of backoff.
                if prior_hash != context_hash {
                    return Ok(true);
                }
                // Same hash: re-emit only if the backoff window has elapsed.
                let last = last_epoch.max(0) as u64;
                Ok(now_epoch.saturating_sub(last) >= refire_secs)
            }
        }
    }

    fn record_escalation_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO escalation_ledger (bead_id, reason, context_hash, last_emitted_epoch) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(bead_id, reason) DO UPDATE SET \
                   context_hash = excluded.context_hash, \
                   last_emitted_epoch = excluded.last_emitted_epoch",
                params![bead_id, reason, context_hash, now_epoch as i64],
            )
            .map_err(|e| tool_err("record_escalation_emit: upsert", e))?;
        Ok(())
    }

    fn mark_escalation_undeliverable(
        &self,
        bead_id: &str,
        reason: &str,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO escalation_ledger (bead_id, reason, context_hash, last_emitted_epoch, terminal) \
                 VALUES (?1, ?2, '', 0, 1) \
                 ON CONFLICT(bead_id, reason) DO UPDATE SET terminal = 1",
                params![bead_id, reason],
            )
            .map_err(|e| tool_err("mark_escalation_undeliverable: upsert", e))?;
        Ok(())
    }

    /// Bead jleechan-g1ib: atomic claim. Single-statement conditional
    /// UPDATE — a row whose live claim belongs to another machine OR whose
    /// claim has expired beyond the TTL will be claimed by `machine`; a row
    /// that already has a live claim by `machine` is treated as a heartbeat
    /// (refresh the timestamp) and also returns true. A row with a live
    /// claim by another machine returns false. The DB's per-connection
    /// mutex on `&self.conn` makes this atomic with respect to other
    /// local connections in WAL mode.
    fn try_claim(
        &self,
        bead_id: &str,
        machine: &str,
        now_epoch: u64,
        ttl_secs: u64,
    ) -> Result<bool, DaemonError> {
        let expired_threshold = now_epoch.saturating_sub(ttl_secs) as i64;
        // Step 1: stale-claim sweep — free the row first so the conditional
        // claim sees a clean slate. A no-op when the row is already free.
        self.conn
            .execute(
                "UPDATE bead_overlay SET claimed_by = NULL, claimed_at = NULL \
                 WHERE bead_id = ?1 \
                   AND claimed_by IS NOT NULL \
                   AND claimed_at IS NOT NULL \
                   AND claimed_at < ?2",
                params![bead_id, expired_threshold],
            )
            .map_err(|e| tool_err("try_claim: expire sweep", e))?;
        // Step 2: ensure a row exists for this bead (idempotent insert).
        // The INSERT explicitly sets `claimed_by = NULL` and `claimed_at =
        // NULL` (overriding the columns' absence in the CREATE TABLE block
        // pre-migration) so step 3 sees a row to claim. `ON CONFLICT DO
        // NOTHING` is intentional: an existing row keeps its current claim
        // (which may belong to another machine).
        self.conn
            .execute(
                "INSERT INTO bead_overlay (bead_id, state, updated_at, claimed_by, claimed_at) \
                 VALUES (?1, 'QUEUED', ?2, NULL, NULL) \
                 ON CONFLICT(bead_id) DO NOTHING",
                params![bead_id, now_iso8601()],
            )
            .map_err(|e| tool_err("try_claim: insert fresh", e))?;
        // Step 3: claim iff currently unclaimed (NULL fields).
        let updated = self
            .conn
            .execute(
                "UPDATE bead_overlay SET claimed_by = ?1, claimed_at = ?2, updated_at = ?3 \
                 WHERE bead_id = ?4 AND claimed_by IS NULL",
                params![machine, now_epoch as i64, now_iso8601(), bead_id],
            )
            .map_err(|e| tool_err("try_claim: conditional claim", e))?;
        Ok(updated > 0)
    }

    /// Bead jleechan-g1ib: release iff the local row's claim belongs to
    /// `machine`. A `release` for a different machine's claim is a no-op
    /// (cannot steal). A `release` on an unclaimed row is also a no-op.
    fn release_claim(&self, bead_id: &str, machine: &str) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "UPDATE bead_overlay SET claimed_by = NULL, claimed_at = NULL, updated_at = ?1 \
                 WHERE bead_id = ?2 AND claimed_by = ?3",
                params![now_iso8601(), bead_id, machine],
            )
            .map_err(|e| tool_err("release_claim", e))?;
        Ok(())
    }

    /// Bead jleechan-g1ib: refresh `claimed_at` iff the row's current claim
    /// belongs to `machine`. Returns false when the row is unclaimed or
    /// belongs to another machine (caller should `try_claim` instead).
    fn heartbeat_claim(
        &self,
        bead_id: &str,
        machine: &str,
        now_epoch: u64,
        _ttl_secs: u64,
    ) -> Result<bool, DaemonError> {
        let updated = self
            .conn
            .execute(
                "UPDATE bead_overlay SET claimed_at = ?1, updated_at = ?2 \
                 WHERE bead_id = ?3 AND claimed_by = ?4",
                params![now_epoch as i64, now_iso8601(), bead_id, machine],
            )
            .map_err(|e| tool_err("heartbeat_claim", e))?;
        Ok(updated > 0)
    }

    /// Bead jleechan-g1ib: live local claims — rows whose `claimed_by` is
    /// set AND whose `claimed_at > now - ttl_secs`. Returned as
    /// `(bead_id, claimed_at, expires_at)` triples.
    fn list_live_local_claims(
        &self,
        now_epoch: u64,
        ttl_secs: u64,
    ) -> Result<Vec<(String, u64, u64)>, DaemonError> {
        let threshold = now_epoch.saturating_sub(ttl_secs) as i64;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, claimed_at FROM bead_overlay \
                 WHERE claimed_by IS NOT NULL AND claimed_at IS NOT NULL \
                   AND claimed_at >= ?1",
            )
            .map_err(|e| tool_err("list_live_local_claims prepare", e))?;
        let rows = stmt
            .query_map(params![threshold], |row| {
                let bead: String = row.get(0)?;
                let at: i64 = row.get(1)?;
                Ok((bead, at))
            })
            .map_err(|e| tool_err("list_live_local_claims query", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (bead, at) = r.map_err(|e| tool_err("list_live_local_claims row", e))?;
            let at_u = at.max(0) as u64;
            out.push((bead, at_u, at_u.saturating_add(ttl_secs)));
        }
        Ok(out)
    }

    /// Bead jleechan-g1ib: replace the entire cached peer-claim set. The
    /// peer daemon reports its full live set on every /sync, so a missing
    /// entry means the peer dropped the claim (TTL expired or explicit
    /// release). We trust the latest snapshot — delete-then-insert in one
    /// transaction.
    fn replace_peer_claims(
        &self,
        claims: &[(String, String, u64, u64)],
        now_epoch: u64,
    ) -> Result<(), DaemonError> {
        let tx = self.conn.unchecked_transaction().map_err(|e| tool_err("replace_peer_claims: tx", e))?;
        tx.execute("DELETE FROM peer_claims", [])
            .map_err(|e| tool_err("replace_peer_claims: delete", e))?;
        for (machine, bead_id, claimed_at, expires_at) in claims {
            tx.execute(
                "INSERT INTO peer_claims (machine, bead_id, claimed_at, expires_at, last_synced_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(machine, bead_id) DO UPDATE SET \
                   claimed_at = excluded.claimed_at, \
                   expires_at = excluded.expires_at, \
                   last_synced_at = excluded.last_synced_at",
                params![
                    machine,
                    bead_id,
                    *claimed_at as i64,
                    *expires_at as i64,
                    now_epoch as i64,
                ],
            )
            .map_err(|e| tool_err("replace_peer_claims: insert", e))?;
        }
        tx.commit().map_err(|e| tool_err("replace_peer_claims: commit", e))?;
        Ok(())
    }

    /// Bead jleechan-g1ib: is this bead claimed by any peer within the
    /// grace window? We use `expires_at > now_epoch` (the peer's own
    /// assertion of when the claim dies) rather than a local recompute so
    /// the peer's TTL choice is honored.
    fn peer_claim_taken(&self, bead_id: &str, now_epoch: u64) -> Result<bool, DaemonError> {
        let row: Result<i64, rusqlite::Error> = self.conn.query_row(
            "SELECT COUNT(*) FROM peer_claims WHERE bead_id = ?1 AND expires_at > ?2",
            params![bead_id, now_epoch as i64],
            |row| row.get(0),
        );
        match row {
            Ok(n) => Ok(n > 0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(tool_err("peer_claim_taken", e)),
        }
    }

    /// Bead jleechan-g1ib: dispatch gate. Returns true when the local
    /// overlay holds a claim by ANOTHER machine (NOT `self_machine`) whose
    /// `claimed_at > now - ttl_secs`. A row with no claim, or a claim by
    /// `self_machine`, is dispatchable.
    fn claim_blocks_dispatch(
        &self,
        bead_id: &str,
        now_epoch: u64,
        ttl_secs: u64,
        self_machine: &str,
    ) -> Result<bool, DaemonError> {
        let threshold = now_epoch.saturating_sub(ttl_secs) as i64;
        let row: Result<Option<String>, rusqlite::Error> = self.conn.query_row(
            "SELECT claimed_by FROM bead_overlay \
             WHERE bead_id = ?1 AND claimed_by IS NOT NULL AND claimed_at >= ?2",
            params![bead_id, threshold],
            |row| row.get(0),
        );
        match row {
            Ok(Some(holder)) => Ok(holder != self_machine),
            Ok(None) => Ok(false),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(tool_err("claim_blocks_dispatch", e)),
        }
    }
}

/// True when `err` is the SQLite "no such column" schema-mismatch signal.
/// `rusqlite::Error` does not expose a typed `message` field on every
/// feature combo, so we stringify + match — this is the same trick
/// the JSON parsers in tools.rs use for `find('{')` fallback.
fn no_such_column(err: &rusqlite::Error) -> bool {
    err.to_string().to_lowercase().contains("no such column")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    static RECOVERY_BUSY_HANDLER_ENTERED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    fn signal_recovery_busy(_attempt: i32) -> bool {
        RECOVERY_BUSY_HANDLER_ENTERED.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    }

    fn store() -> SqliteStateStore {
        SqliteStateStore::open_in_memory_with_schema(include_str!("../contracts/schema.sql"))
            .unwrap()
    }

    #[test]
    fn every_production_park_reason_flows_through_the_typed_policy() {
        let mut production_code = String::new();
        for (file, source) in [
            ("dispatch.rs", include_str!("dispatch.rs")),
            ("reroll.rs", include_str!("reroll.rs")),
            ("tick.rs", include_str!("tick.rs")),
            ("state.rs", include_str!("state.rs")),
        ] {
            let production = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(source);
            production_code.push_str(file);
            for line in production.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                production_code.extend(line.chars().filter(|character| !character.is_whitespace()));
            }
        }

        assert_eq!(
            production_code.matches(".park_reason=Some").count(),
            1,
            "only set_human_hold_reason may directly assign Some"
        );
        assert_eq!(
            production_code.matches("park_reason:Some").count(),
            0,
            "production constructors must not bypass the typed policy"
        );
        assert_eq!(
            production_code.matches("park_reason='").count(),
            0,
            "production SQL must bind typed policy values"
        );
        assert!(is_permanent_human_hold_reason(None));
        assert!(is_permanent_human_hold_reason(Some(
            "future_unknown_reason"
        )));
    }

    #[test]
    fn reroll_deferral_counter_increments_resets_and_persists() {
        // Bead jleechan-zeij / issue #322 r2: the fail-closed defer/cap path
        // depends on this counter surviving between ticks (separate
        // `reroll::execute` calls). Exercise the REAL SqliteStateStore, not
        // just the fake.
        let s = store();
        let o = BeadOverlay {
            bead_id: "defer-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(42),
            branch: Some("factory/defer-bead-r1".into()),
            session_id: Some("sess-live".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();

        // Never deferred yet.
        assert_eq!(s.reroll_deferral_count("defer-bead").unwrap(), 0);
        // Consecutive increments accumulate and are returned.
        assert_eq!(s.incr_reroll_deferral("defer-bead").unwrap(), 1);
        assert_eq!(s.incr_reroll_deferral("defer-bead").unwrap(), 2);
        assert_eq!(s.reroll_deferral_count("defer-bead").unwrap(), 2);
        // A confirmed proceed resets the streak.
        s.reset_reroll_deferral("defer-bead").unwrap();
        assert_eq!(s.reroll_deferral_count("defer-bead").unwrap(), 0);
        // Incrementing/reading a bead with no overlay row is a no-op read of 0
        // (the UPDATE matches nothing) rather than an error.
        assert_eq!(s.reroll_deferral_count("no-such-bead").unwrap(), 0);
    }

    /// advice-627-630-20260809 PR #628 finding 2: the consecutive PERMANENT
    /// (non-transient) head-probe failure counter must survive between ticks
    /// (separate `reroll::execute` calls) exactly like `reroll_deferral_count`
    /// -- exercise the REAL SqliteStateStore, not just the fake.
    #[test]
    fn reroll_head_permanent_failure_counter_increments_resets_and_persists() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "permfail-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(43),
            branch: Some("factory/permfail-bead-r1".into()),
            session_id: Some("sess-live".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();

        // Never failed permanently yet.
        assert_eq!(
            s.reroll_head_permanent_failure_count("permfail-bead").unwrap(),
            0
        );
        // Consecutive increments accumulate and are returned.
        assert_eq!(
            s.incr_reroll_head_permanent_failure("permfail-bead").unwrap(),
            1
        );
        assert_eq!(
            s.incr_reroll_head_permanent_failure("permfail-bead").unwrap(),
            2
        );
        assert_eq!(
            s.reroll_head_permanent_failure_count("permfail-bead").unwrap(),
            2
        );
        // A successful probe resets the streak.
        s.reset_reroll_head_permanent_failure("permfail-bead").unwrap();
        assert_eq!(
            s.reroll_head_permanent_failure_count("permfail-bead").unwrap(),
            0
        );
        // Incrementing/reading a bead with no overlay row is a no-op read of 0
        // (the UPDATE matches nothing) rather than an error.
        assert_eq!(
            s.reroll_head_permanent_failure_count("no-such-bead").unwrap(),
            0
        );
    }

    /// Bead jleechan-zaga / issue #348 r3: the held-recheck cooldown epoch must
    /// round-trip through the REAL SqliteStateStore column.
    #[test]
    fn held_recheck_after_round_trips() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "held-bead".into(),
            state: OverlayState::DispositionRequired,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(42),
            branch: Some("alice/feature".into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();
        // Unset by default.
        assert_eq!(s.held_recheck_after("held-bead").unwrap(), None);
        s.set_held_recheck_after("held-bead", 1_800_000_000).unwrap();
        assert_eq!(s.held_recheck_after("held-bead").unwrap(), Some(1_800_000_000));
        // No overlay row -> None, not an error.
        assert_eq!(s.held_recheck_after("no-such-bead").unwrap(), None);
    }

    /// Bead jleechan-zaga / issue #348 r3: the CHECK migration must be robust
    /// to ANY legal DDL formatting, because the r3 detection is a PROBE (a
    /// rolled-back INSERT), not a string-match on the stored DDL. Runs one
    /// legacy `bead_overlay` DDL through the migration and asserts: (a) the
    /// pre-existing row is preserved, (b) DISPOSITION_REQUIRED is accepted
    /// afterward, (c) a second run is idempotent. `expect_rejected_before`
    /// asserts the legacy CHECK rejected the new state pre-migration (false for
    /// the already-migrated fixture, which accepts it from the start).
    fn run_disposition_migration_case(bead_overlay_ddl: &str, expect_rejected_before: bool) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(bead_overlay_ddl).unwrap();
        conn.execute(
            "INSERT INTO bead_overlay (bead_id, state, updated_at) \
             VALUES ('b-legacy', 'ATTESTED', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let insert_new = |c: &Connection| {
            c.execute(
                "INSERT INTO bead_overlay (bead_id, state, updated_at) \
                 VALUES ('b-new', 'DISPOSITION_REQUIRED', '2026-01-01T00:00:00Z')",
                [],
            )
        };
        if expect_rejected_before {
            assert!(
                insert_new(&conn).is_err(),
                "legacy CHECK must reject DISPOSITION_REQUIRED before migration"
            );
        }

        SqliteStateStore::ensure_disposition_required_state(&conn).unwrap();

        // (a) row preserved.
        let preserved: String = conn
            .query_row(
                "SELECT state FROM bead_overlay WHERE bead_id = 'b-legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved, "ATTESTED");
        // (b) new state accepted.
        insert_new(&conn).expect("post-migration CHECK must accept DISPOSITION_REQUIRED");
        // (c) idempotent second run preserves both rows and the usable CHECK.
        SqliteStateStore::ensure_disposition_required_state(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bead_overlay", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        conn.execute(
            "INSERT INTO bead_overlay (bead_id, state, updated_at) \
             VALUES ('b-new2', 'DISPOSITION_REQUIRED', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("CHECK must remain usable after an idempotent second migration");
    }

    #[test]
    fn disposition_migration_exact_production_ddl() {
        run_disposition_migration_case(
            "CREATE TABLE bead_overlay (\
               bead_id TEXT PRIMARY KEY, \
               state TEXT NOT NULL CHECK (state IN \
                 ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',\
                  'REDISPATCHED','BUDGET_HELD','HUMAN_HELD')), \
               attempt INTEGER NOT NULL DEFAULT 1, \
               updated_at TEXT NOT NULL)",
            true,
        );
    }

    #[test]
    fn disposition_migration_whitespace_variant_ddl() {
        // Newlines and irregular spacing inside the CHECK — a string-edit of
        // `'HUMAN_HELD')` would silently miss this; the probe does not.
        run_disposition_migration_case(
            "CREATE TABLE bead_overlay (\n  bead_id TEXT PRIMARY KEY,\n  state TEXT NOT NULL\n    CHECK ( state IN (\n      'QUEUED', 'DISPATCHING', 'DISPATCHED', 'ATTESTED', 'READY',\n      'RE_ROLL', 'RECOVERY', 'REDISPATCHED', 'BUDGET_HELD', 'HUMAN_HELD'\n    ) ),\n  updated_at TEXT NOT NULL\n)",
            true,
        );
    }

    #[test]
    fn disposition_migration_quoted_identifier_ddl() {
        // Quoted table/column identifiers — `CREATE TABLE "bead_overlay"` and
        // `"state"` would break a `replacen("CREATE TABLE bead_overlay", …)`.
        run_disposition_migration_case(
            "CREATE TABLE \"bead_overlay\" (\
               \"bead_id\" TEXT PRIMARY KEY, \
               \"state\" TEXT NOT NULL CHECK (\"state\" IN \
                 ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',\
                  'REDISPATCHED','BUDGET_HELD','HUMAN_HELD')), \
               \"updated_at\" TEXT NOT NULL)",
            true,
        );
    }

    #[test]
    fn disposition_migration_already_migrated_ddl_is_noop() {
        // A DB whose CHECK already lists DISPOSITION_REQUIRED: the probe
        // succeeds, so the migration is a no-op and the table stays usable.
        run_disposition_migration_case(
            "CREATE TABLE bead_overlay (\
               bead_id TEXT PRIMARY KEY, \
               state TEXT NOT NULL CHECK (state IN \
                 ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',\
                  'REDISPATCHED','BUDGET_HELD','HUMAN_HELD','DISPOSITION_REQUIRED')), \
               updated_at TEXT NOT NULL)",
            false,
        );
    }

    /// End-to-end via the public open path: a store opened against a
    /// legacy-constraint schema string can persist and reload a
    /// DISPOSITION_REQUIRED overlay (the migration runs inside
    /// `open_in_memory_with_schema`).
    #[test]
    fn open_migrates_legacy_check_and_persists_disposition_required_overlay() {
        let legacy = include_str!("../contracts/schema.sql")
            .replace("'HUMAN_HELD','DISPOSITION_REQUIRED')", "'HUMAN_HELD')");
        let s = SqliteStateStore::open_in_memory_with_schema(&legacy).unwrap();
        let o = BeadOverlay {
            bead_id: "held-bead".into(),
            state: OverlayState::DispositionRequired,
            attempt: 2,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(708),
            branch: Some("alice/feature".into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).expect("DISPOSITION_REQUIRED must persist after open-time migration");
        let got = s.load("held-bead").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::DispositionRequired);
        assert_eq!(got.pr_number, Some(708));
    }

    #[test]
    fn overlay_roundtrip() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "b1".into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();
        let got = s.load("b1").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::Queued);
        assert_eq!(got.attempt, 1);
        assert_eq!(got.bead_id, "b1");
        assert_eq!(got.pr_number, None);
        assert_eq!(got.branch, None);
    }

    #[test]
    fn session_ao_project_roundtrips_and_legacy_rows_load_none() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "session-project".into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
            session_id: Some("session-project-id".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: Some("dark-factory".into()),
        };
        s.save(&o).unwrap();
        let restored = s.load(&o.bead_id).unwrap().unwrap();
        assert_eq!(restored.session_id.as_deref(), Some("session-project-id"));
        assert_eq!(restored.session_ao_project.as_deref(), Some("dark-factory"));

        s.conn
            .execute(
                "INSERT INTO bead_overlay (bead_id, state, updated_at) VALUES ('legacy-project', 'QUEUED', 'now')",
                [],
            )
            .unwrap();
        assert_eq!(s.load("legacy-project").unwrap().unwrap().session_ao_project, None);
    }

    #[test]
    fn overlay_roundtrip_updates_on_conflict() {
        let s = store();
        let mut o = BeadOverlay {
            bead_id: "b2".into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 10,
            spend_usd: 0.5,
            pr_number: None,
            branch: None,
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();
        o.state = OverlayState::Attested;
        o.attempt = 2;
        o.pr_number = Some(42);
        o.branch = Some("factory/b2-r2".into());
        s.save(&o).unwrap();
        let got = s.load("b2").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::Attested);
        assert_eq!(got.attempt, 2);
        assert_eq!(got.pr_number, Some(42));
        assert_eq!(got.branch, Some("factory/b2-r2".into()));
    }

    #[test]
    fn load_missing_bead_returns_none() {
        let s = store();
        assert!(s.load("nope").unwrap().is_none());
    }

    #[test]
    fn reconcile_dispatching_parks_ambiguity_and_preserves_recovery_handles() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "b-stale".into(),
            state: OverlayState::Dispatching,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("stale-branch".into()),
            session_id: Some("stale-session-id".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };

        s.save(&o).unwrap();
        s.reconcile_dispatching().unwrap();

        let got = s.load("b-stale").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::HumanHeld);
        assert_eq!(got.session_id.as_deref(), Some("stale-session-id"));
        assert_eq!(got.branch.as_deref(), Some("stale-branch"));
        assert_eq!(
            got.park_reason.as_deref(),
            Some("ambiguous_dispatching_recovery")
        );
        assert!(
            s.recover_human_held(10).unwrap().is_empty(),
            "ambiguous dispatch must never auto-requeue and spawn a duplicate"
        );
    }

    #[test]
    fn reconcile_dispatching_preserves_cleanup_failure_hold_and_live_session() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "cleanup-held".into(),
            state: OverlayState::HumanHeld,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/cleanup-held-r1".into()),
            session_id: Some("known-live-session".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: Some("spawn_cleanup_failed".into()),
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };

        s.save(&o).unwrap();
        s.reconcile_dispatching().unwrap();

        let got = s.load("cleanup-held").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::HumanHeld);
        assert_eq!(got.session_id.as_deref(), Some("known-live-session"));
        assert_eq!(got.branch.as_deref(), Some("factory/cleanup-held-r1"));
        assert_eq!(got.park_reason.as_deref(), Some("spawn_cleanup_failed"));
    }

    #[test]
    fn session_routing_restores_sessions_branches_and_exact_spawn_project() {
        let s = store();
        let mut routed = BeadOverlay {
            bead_id: "routed".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/routed-r1".into()),
            session_id: Some("wa-404".into()),
            session_ao_project: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: Some("jleechanorg/worldarchitect.ai".into()),
            attempt_started_at: None,
        };
        routed.session_id = None;
        routed.state = OverlayState::Dispatching;
        s.save_dispatch_intent(&routed, "worldarchitect-old").unwrap();
        let intent = s.session_routing_bindings().unwrap().into_iter()
            .find(|binding| binding.branch.as_deref() == Some("factory/routed-r1"))
            .unwrap();
        assert_eq!(intent.ao_project.as_deref(), Some("worldarchitect-old"));

        routed.session_id = Some("wa-404".into());
        routed.state = OverlayState::Dispatched;
        s.save_dispatched_session(&routed, "worldarchitect-old").unwrap();
        routed.state = OverlayState::Attested;
        s.save(&routed).unwrap();

        let mut branch_only = routed.clone();
        branch_only.bead_id = "branch-only".into();
        branch_only.branch = Some("contributor/fix".into());
        branch_only.session_id = None;
        s.save(&branch_only).unwrap();

        let bindings = s.session_routing_bindings().unwrap();
        let session = bindings.iter().find(|binding| {
            binding.session_id.as_deref() == Some("wa-404")
        }).unwrap();
        assert_eq!(session.ao_project.as_deref(), Some("worldarchitect-old"));
        assert!(bindings.iter().any(|binding| {
            binding.session_id.is_none()
                && binding.branch.as_deref() == Some("contributor/fix")
        }));
    }

    #[test]
    fn illegal_state_string_rejected_by_schema() {
        let s = store();
        let r = s.conn.execute(
            "INSERT INTO bead_overlay (bead_id,state,updated_at) VALUES ('x','BOGUS','now')",
            [],
        );
        assert!(r.is_err(), "CHECK constraint must reject unknown states");
    }

    #[test]
    fn all_ten_overlay_states_accepted_by_schema() {
        let s = store();
        for (i, state) in [
            OverlayState::Queued,
            OverlayState::Dispatching,
            OverlayState::Dispatched,
            OverlayState::Attested,
            OverlayState::Ready,
            OverlayState::ReRoll,
            OverlayState::Recovery,
            OverlayState::Redispatched,
            OverlayState::BudgetHeld,
            OverlayState::HumanHeld,
        ]
        .into_iter()
        .enumerate()
        {
            let o = BeadOverlay {
                bead_id: format!("bead-{i}"),
                state,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            };
            s.save(&o).unwrap();
            let got = s.load(&o.bead_id).unwrap().unwrap();
            assert_eq!(got.state, state);
        }
    }

    #[test]
    fn owned_branches_lists_only_registered() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        assert_eq!(
            s.owned_branches().unwrap(),
            vec!["factory/b1-r1".to_string()]
        );
    }

    #[test]
    fn owned_branches_deletion_guard_survives_bead_overlay_delete() {
        // Spec §4.2.8: the daemon may delete ONLY refs recorded in branch_registry.
        // There is intentionally no FK between bead_overlay and branch_registry in
        // contracts/schema.sql, so deleting a bead_overlay row must NOT cascade,
        // error, or silently orphan/mutate branch_registry — owned_branches() must
        // keep returning exactly what was registered, independent of overlay state.
        let s = store();
        let o = BeadOverlay {
            bead_id: "b1".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(7),
            branch: Some("factory/b1-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
            session_ao_project: None,
        };
        s.save(&o).unwrap();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        assert_eq!(
            s.owned_branches().unwrap(),
            vec!["factory/b1-r1".to_string()]
        );

        // Delete the overlay row directly (simulates a bead being purged/reset).
        s.conn
            .execute("DELETE FROM bead_overlay WHERE bead_id = ?1", params!["b1"])
            .unwrap();

        // branch_registry must be unaffected: still exactly the registered branch,
        // proving the guard is enforced at the DB layer (no FK cascade exists to
        // exploit) rather than merely documented in a comment.
        assert_eq!(
            s.owned_branches().unwrap(),
            vec!["factory/b1-r1".to_string()],
            "deleting bead_overlay must not orphan/violate branch_registry"
        );
        assert!(s.load("b1").unwrap().is_none());
    }

    #[test]
    fn register_branch_rejects_conflicting_owner() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        s.register_branch("b1", "factory/b1-r1").unwrap(); // idempotent for same bead
        let err = s.register_branch("b2", "factory/b1-r1").unwrap_err();
        assert!(
            err.to_string().contains("already registered to bead b1"),
            "unexpected conflict error: {err}"
        );
        let branches = s.owned_branches().unwrap();
        assert_eq!(branches, vec!["factory/b1-r1".to_string()]);
        assert_eq!(
            s.bead_id_for_branch("factory/b1-r1").unwrap(),
            Some("b1".to_string())
        );
    }

    /// jleechan-8in: file-backed `open()` must not silently swallow a failed
    /// `PRAGMA journal_mode=WAL`. This asserts the happy path actually took
    /// effect (readback == "wal") on a real temp-file-backed connection —
    /// proving `configure(is_memory=false)` verifies rather than discards.
    #[test]
    fn open_on_real_file_actually_sets_wal_mode() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-state-test-{}-{}.sqlite",
            std::process::id(),
            now_iso8601().replace([':', '-', 'T', 'Z'], "")
        ));
        let _cleanup = TempFileGuard(path.clone());

        let s = SqliteStateStore::open(&path).unwrap();
        let mode: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    /// jleechan-8in: a non-"wal" (or errored) readback on a file-backed connection
    /// must propagate as `DaemonError::Config`, not be discarded. Exercises
    /// `configure` directly with `is_memory=false` against an in-memory connection
    /// (WAL cannot take effect there), which is exactly the failure mode the bead
    /// describes for real files with unsupported filesystems/permissions/NFS.
    #[test]
    fn configure_file_backed_propagates_wal_failure_as_config_error() {
        let conn = Connection::open_in_memory().unwrap();
        let err = SqliteStateStore::configure(&conn, false).unwrap_err();
        match err {
            DaemonError::Config(msg) => {
                assert!(
                    msg.contains("journal_mode=WAL"),
                    "error message should mention the failing pragma: {msg}"
                );
            }
            other => panic!("expected DaemonError::Config, got {other:?}"),
        }
    }

    /// Cleans up the temp file (+ WAL/SHM sidecars) created by
    /// `open_on_real_file_actually_sets_wal_mode`, even on panic.
    struct TempFileGuard(std::path::PathBuf);
    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    /// jleechan-qqq: an on-disk DB that lacks the `/er` runner columns must
    /// be auto-migrated by `open()` and `open_in_memory_with_schema()`, so
    /// the runner's first `incr_er_runner_attempt` doesn't fail with
    /// "no such column". This pins the idempotency of the migration: a
    /// second `open()` against the same file must NOT error on the
    /// re-applied `ALTER TABLE` (a hard crash here would also block every
    /// daemon restart, since `execute_batch(schema.sql)` re-runs).
    #[test]
    fn open_migrates_legacy_db_missing_er_runner_columns() {
        use rusqlite::Connection;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-er-migrate-{}-{}.sqlite",
            std::process::id(),
            now_iso8601().replace([':', '-', 'T', 'Z'], "")
        ));
        let _cleanup = TempFileGuard(path.clone());

        // Build a "legacy" DB: just `bead_overlay` and `branch_registry`,
        // no `/er` runner columns.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE bead_overlay (bead_id TEXT PRIMARY KEY, state TEXT NOT NULL, \
                 attempt INTEGER NOT NULL DEFAULT 1, reroll_count INTEGER NOT NULL DEFAULT 0, \
                 autonomy_secs INTEGER NOT NULL DEFAULT 0, spend_usd REAL NOT NULL DEFAULT 0, \
                 pr_number INTEGER, branch TEXT, session_id TEXT, updated_at TEXT NOT NULL); \
                 CREATE TABLE branch_registry (branch TEXT PRIMARY KEY, bead_id TEXT NOT NULL, \
                 created_at TEXT NOT NULL);",
            )
            .unwrap();
        }

        // First open: must apply the migration without error.
        let _store = SqliteStateStore::open(&path).expect("legacy DB should auto-migrate");

        // Re-open: must NOT fail on the second ALTER (idempotency).
        let _store2 = SqliteStateStore::open(&path).expect("second open must be idempotent");

        // Both columns are present, with the expected defaults.
        let conn = Connection::open(&path).unwrap();
        let count_col: i64 = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'attempt_er_runner_count'",
                [],
                |row| row.get(0),
            )
            .expect("attempt_er_runner_count column should exist after migration");
        let last_col: i64 = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_runner_attempt_at'",
                [],
                |row| row.get(0),
            )
            .expect("last_er_runner_attempt_at column should exist after migration");
        assert_eq!(count_col, 1);
        assert_eq!(last_col, 1);
    }

    /// Cross-model review P2 #2 (bead jleechan-n6mk, follow-up to PR #447):
    /// the existing `open_migrates_legacy_db_missing_er_runner_columns` test
    /// uses a 3-column `bead_overlay` stub — that hides migration regressions
    /// in the long tail of `ensure_*_column` migrations and the disposition
    /// CHECK rebuild, because a 3-column table can't exercise them. This test
    /// uses a production-shaped fixture: a SQLite DB built from the EXACT
    /// pre-PR-#447 schema (every column the current `schema.sql` declares,
    /// EXCEPT the `escalation_ledger` table + its `terminal` column which
    /// PR #447 introduces) populated with realistic rows in every table,
    /// then asserts that `open()` migrates it to the current schema without
    /// dropping, reordering, or rewriting any pre-existing row.
    ///
    /// The pre-PR-#447 fixture has the disposition-state CHECK missing
    /// `DISPOSITION_REQUIRED` (which PR #387 introduced), so this test
    /// simultaneously exercises the disposition-rebuild migration. Pre-PR-#447
    /// legacy DBs are precisely the population that would hit that rebuild
    /// path in production.
    #[test]
    fn open_migrates_production_shaped_legacy_db_preserves_every_row() {
        use rusqlite::Connection;

        // Tuple alias for the per-row overlay snapshot. The 11-element tuple
        // is verbose enough to trip `clippy::type_complexity`, so we alias it
        // here. The field order matches the SELECT in `pre_overlays` /
        // `post_overlays`.
        type OverlaySnapshotRow = (
            String, // bead_id
            String, // state
            i64,    // attempt
            Option<i64>,        // pr_number
            Option<String>,     // branch
            Option<String>,     // session_id
            Option<String>,     // target_repo
            Option<String>,     // park_reason
            Option<String>,     // last_er_evidence_hash
            Option<i64>,        // held_recheck_after
            Option<String>,     // pre_session_head_sha
        );

        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-prod-shaped-migrate-{}-{}.sqlite",
            std::process::id(),
            now_iso8601().replace([':', '-', 'T', 'Z'], "")
        ));
        let _cleanup = TempFileGuard(path.clone());

        // Build a production-shaped "legacy" DB: every table + column the
        // current schema.sql declares, EXCEPT (a) `escalation_ledger` (PR
        // #447 introduces it), and (b) the `terminal` column on
        // `escalation_ledger` (PR #447 introduces it as a follow-up). The
        // disposition CHECK is the pre-#387 variant (no
        // `DISPOSITION_REQUIRED`) so the rebuild migration has real work to do.
        //
        // Column order in `bead_overlay` matches the canonical schema so
        // any column-add migration is a `tail-append` (SQLite-safe) and
        // doesn't trigger an unintended rebuild.
        const LEGACY_SCHEMA: &str = r#"
            CREATE TABLE bead_overlay (
              bead_id       TEXT PRIMARY KEY,
              state         TEXT NOT NULL CHECK (state IN
                              ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',
                               'REDISPATCHED','BUDGET_HELD','HUMAN_HELD')),
              attempt       INTEGER NOT NULL DEFAULT 1,
              reroll_count  INTEGER NOT NULL DEFAULT 0,
              autonomy_secs INTEGER NOT NULL DEFAULT 0,
              spend_usd     REAL    NOT NULL DEFAULT 0,
              pr_number     INTEGER,
              branch        TEXT,
              session_id    TEXT,
              updated_at    TEXT    NOT NULL,
              attempt_er_runner_count INTEGER NOT NULL DEFAULT 0,
              last_er_runner_attempt_at INTEGER,
              is_adopted INTEGER NOT NULL DEFAULT 0,
              spawn_failure_count INTEGER NOT NULL DEFAULT 0,
              pre_session_head_sha TEXT,
              park_reason TEXT,
              target_repo TEXT,
              reroll_deferral_count INTEGER NOT NULL DEFAULT 0,
              held_recheck_after INTEGER,
              last_er_evidence_hash TEXT,
              last_all_green INTEGER NOT NULL DEFAULT 0,
              gate_regression_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE branch_registry (
              branch     TEXT PRIMARY KEY,
              bead_id    TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            CREATE TABLE review_rejection (
              bead_id       TEXT NOT NULL,
              attempt       INTEGER NOT NULL,
              reviewer      TEXT NOT NULL,
              feedback_hash TEXT NOT NULL,
              feedback_text TEXT NOT NULL,
              created_at    TEXT NOT NULL,
              PRIMARY KEY (bead_id, attempt)
            );
            -- NOTE: no `escalation_ledger` table — that's what PR #447 adds.
        "#;

        // Seed realistic rows in every table. Each row is chosen so any of
        // the migrations would visibly mangle it if they did a wrong table
        // rebuild (e.g. NULL DEFAULTs being clobbered, CHECK constraint
        // reordering altering the surviving set, etc.).
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(LEGACY_SCHEMA).unwrap();

        // bead_overlay: cover every state value the legacy CHECK allows,
        // plus every NOT-NULL column with a non-default value, plus every
        // nullable column set to a non-NULL value (so a migration that
        // dropped/reordered columns would change the visible row).
        let mut inserted_overlays = Vec::new();
        let mut seed_overlay = |bead_id: &str,
                                state: &str,
                                attempt: i64,
                                pr_number: Option<i64>,
                                branch: Option<&str>,
                                session_id: Option<&str>,
                                target_repo: Option<&str>,
                                park_reason: Option<&str>,
                                last_er_evidence_hash: Option<&str>,
                                held_recheck_after: Option<i64>,
                                pre_session_head_sha: Option<&str>| {
            conn.execute(
                "INSERT INTO bead_overlay \
                 (bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                  pr_number, branch, session_id, updated_at, \
                  attempt_er_runner_count, last_er_runner_attempt_at, is_adopted, \
                  spawn_failure_count, pre_session_head_sha, park_reason, target_repo, \
                  reroll_deferral_count, held_recheck_after, last_er_evidence_hash) \
                 VALUES (?1, ?2, ?3, 0, 0, 0.0, ?4, ?5, ?6, '2026-07-22T00:00:00Z', \
                         3, 1700000000, 1, 2, ?7, ?8, ?9, 4, ?10, ?11)",
                rusqlite::params![
                    bead_id, state, attempt, pr_number, branch, session_id,
                    pre_session_head_sha, park_reason, target_repo,
                    held_recheck_after, last_er_evidence_hash
                ],
            )
            .unwrap();
            inserted_overlays.push((
                bead_id.to_string(),
                state.to_string(),
                attempt,
                pr_number,
                branch.map(String::from),
                session_id.map(String::from),
                target_repo.map(String::from),
                park_reason.map(String::from),
                last_er_evidence_hash.map(String::from),
                held_recheck_after,
                pre_session_head_sha.map(String::from),
            ));
        };

        seed_overlay("bead-1", "QUEUED", 1, Some(101), Some("factory/bead-1-r1"), Some("sess-a"), Some("owner/repo1"), None, None, None, None);
        seed_overlay("bead-2", "DISPATCHED", 2, Some(102), Some("factory/bead-2-r2"), Some("sess-b"), Some("owner/repo1"), None, Some("abc123"), None, None);
        seed_overlay("bead-3", "ATTESTED", 3, Some(103), Some("factory/bead-3-r3"), Some("sess-c"), Some("owner/repo2"), None, None, Some(1700001000), Some("deadbeef1234"));
        seed_overlay("bead-4", "READY", 4, Some(104), Some("factory/bead-4-r4"), None, None, None, Some("def456"), None, None);
        seed_overlay("bead-5", "HUMAN_HELD", 10, Some(105), Some("factory/bead-5-r10"), None, None, Some("circuit-breaker-triggered"), None, None, None);
        seed_overlay("bead-6", "DISPATCHING", 1, None, None, None, None, None, None, None, None);
        seed_overlay("bead-7", "RE_ROLL", 2, Some(107), Some("factory/bead-7-r2"), Some("sess-g"), Some("owner/repo3"), None, None, None, None);
        seed_overlay("bead-8", "RECOVERY", 3, Some(108), Some("factory/bead-8-r3"), None, Some("owner/repo3"), None, None, None, None);
        seed_overlay("bead-9", "REDISPATCHED", 4, Some(109), Some("factory/bead-9-r4"), Some("sess-i"), Some("owner/repo1"), None, None, None, None);
        seed_overlay("bead-10", "BUDGET_HELD", 5, Some(110), Some("factory/bead-10-r5"), None, None, Some("autonomy_timebox_exceeded"), None, Some(1700002000), None);

        // branch_registry: a few rows that exercise the deletion-guard table.
        conn.execute(
            "INSERT INTO branch_registry (branch, bead_id, created_at) VALUES \
             ('factory/bead-1-r1', 'bead-1', '2026-07-22T00:00:00Z'), \
             ('factory/bead-2-r2', 'bead-2', '2026-07-22T00:00:01Z'), \
             ('factory/bead-5-r10', 'bead-5', '2026-07-22T00:00:02Z')",
            [],
        ).unwrap();

        // review_rejection: rows that exercise the per-bead rejection tracking.
        conn.execute(
            "INSERT INTO review_rejection (bead_id, attempt, reviewer, feedback_hash, feedback_text, created_at) VALUES \
             ('bead-1', 1, 'cursor', 'hash-cursor-1', 'fake feedback 1', '2026-07-22T00:00:00Z'), \
             ('bead-3', 3, 'coderabbit', 'hash-cr-3', 'fake feedback 3', '2026-07-22T00:00:01Z'), \
             ('bead-5', 10, 'bugbot', 'hash-bb-10', 'fake feedback 5', '2026-07-22T00:00:02Z')",
            [],
        ).unwrap();

        // Snapshot the entire DB before migration.
        let pre_overlays: Vec<OverlaySnapshotRow> = {
            let mut stmt = conn.prepare(
                "SELECT bead_id, state, attempt, pr_number, branch, session_id, target_repo, \
                        park_reason, last_er_evidence_hash, held_recheck_after, pre_session_head_sha \
                 FROM bead_overlay ORDER BY bead_id",
            ).unwrap();
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            }).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            pre_overlays.len(),
            inserted_overlays.len(),
            "fixture seed must round-trip pre-migration snapshot"
        );

        let pre_branches: Vec<(String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT branch, bead_id, created_at FROM branch_registry ORDER BY branch",
            ).unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
        };
        let pre_rejections: Vec<(String, i64, String, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT bead_id, attempt, reviewer, feedback_hash, feedback_text, created_at \
                 FROM review_rejection ORDER BY bead_id, attempt",
            ).unwrap();
            stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))).unwrap().map(|r| r.unwrap()).collect()
        };
        drop(conn);

        // ── Migrate: open() must apply every ensure_*_column + the disposition
        //    rebuild + the escalation_ledger CREATE TABLE + the terminal
        //    column ADD without losing any pre-existing row.
        let _store = SqliteStateStore::open(&path).expect("production-shaped legacy DB must auto-migrate");

        // Re-open: idempotency guard — second open() must NOT fail.
        let _store2 = SqliteStateStore::open(&path).expect("second open must be idempotent");

        let conn = Connection::open(&path).unwrap();

        // 1) Every pre-existing bead_overlay row must be present with the
        //    same per-column values. The migration only appends columns
        //    with sensible defaults, so NULL values stay NULL and the
        //    legacy CHECK migration can rewrite the table without losing
        //    rows that survive the new (wider) CHECK constraint.
        let post_overlays: Vec<OverlaySnapshotRow> = {
            let mut stmt = conn.prepare(
                "SELECT bead_id, state, attempt, pr_number, branch, session_id, target_repo, \
                        park_reason, last_er_evidence_hash, held_recheck_after, pre_session_head_sha \
                 FROM bead_overlay ORDER BY bead_id",
            ).unwrap();
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            }).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            post_overlays, pre_overlays,
            "every pre-existing bead_overlay row must be preserved byte-for-byte after migration; \
             got pre={pre_overlays:?} post={post_overlays:?}"
        );

        // 2) branch_registry rows preserved.
        let post_branches: Vec<(String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT branch, bead_id, created_at FROM branch_registry ORDER BY branch",
            ).unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            post_branches, pre_branches,
            "branch_registry rows must be preserved; got pre={pre_branches:?} post={post_branches:?}"
        );

        // 3) review_rejection rows preserved.
        let post_rejections: Vec<(String, i64, String, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT bead_id, attempt, reviewer, feedback_hash, feedback_text, created_at \
                 FROM review_rejection ORDER BY bead_id, attempt",
            ).unwrap();
            stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(
            post_rejections, pre_rejections,
            "review_rejection rows must be preserved; got pre={pre_rejections:?} post={post_rejections:?}"
        );

        // 4) The new escalation_ledger table is present and empty
        //    (migration does NOT backfill from review_rejection — the
        //    ledger is populated lazily by `record_escalation_emit` on
        //    the next escalation event).
        let ledger_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM escalation_ledger", [], |row| row.get(0))
            .unwrap();
        assert_eq!(ledger_count, 0, "escalation_ledger must start empty after migration");

        // 5) The terminal column exists with the documented default (0).
        //    `pragma_table_info.dflt_value` is TEXT (the literal SQL fragment
        //    — "0", "NULL", or NULL for "no default"). For an `INTEGER NOT
        //    NULL DEFAULT 0` column, that fragment is the string "0".
        let terminal_default: String = conn
            .query_row(
                "SELECT COALESCE((SELECT \"dflt_value\" FROM pragma_table_info('escalation_ledger') \
                                  WHERE name = 'terminal'), 'MISSING')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            terminal_default, "0",
            "escalation_ledger.terminal must default to 0 (non-terminal)"
        );

        // 6) The bead_overlay CHECK constraint was widened by the disposition
        //    rebuild — `DISPOSITION_REQUIRED` must now be insertable (it's
        //    the only state value that proves the wider CHECK is live).
        conn.execute(
            "INSERT INTO bead_overlay \
             (bead_id, state, attempt, updated_at) \
             VALUES ('bead-new-disp', 'DISPOSITION_REQUIRED', 1, '2026-07-22T00:00:03Z')",
            [],
        ).expect("DISPOSITION_REQUIRED must be insertable after the disposition-rebuild migration");

        // 7) The bead_overlay CHECK constraint still rejects garbage — pins
        //    the constraint is wired up (not silently dropped).
        let res = conn.execute(
            "INSERT INTO bead_overlay \
             (bead_id, state, attempt, updated_at) \
             VALUES ('bead-bogus', 'NOT_A_REAL_STATE', 1, '2026-07-22T00:00:04Z')",
            [],
        );
        assert!(
            res.is_err(),
            "CHECK constraint must still reject invalid state values after migration"
        );

        // 8) Row counts — the migration must NOT delete anything.
        let overlay_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bead_overlay", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            overlay_count as usize,
            inserted_overlays.len() + 1,
            "bead_overlay must have {} seeded rows + 1 DISPOSITION_REQUIRED test row; got {}",
            inserted_overlays.len(),
            overlay_count
        );

        let branch_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM branch_registry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            branch_count as usize,
            pre_branches.len(),
            "branch_registry must retain all pre-existing rows"
        );

        let rejection_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM review_rejection", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            rejection_count as usize,
            pre_rejections.len(),
            "review_rejection must retain all pre-existing rows"
        );
    }

    /// The real-SQLite policy requeues only retry-safe no-session rows under
    /// the cap and leaves capped or permanent rows held. The shell command
    /// delegates to this implementation so there is one recovery policy.
    #[test]
    fn recover_human_held_requeues_below_cap_leaves_at_or_above_alone() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "below".to_string(),
            BeadOverlay {
                bead_id: "below".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 9999,
                spend_usd: 0.0,
                pr_number: Some(11),
                branch: Some("factory/below-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("transient_spawn_retry_cap_exceeded".into()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "at-cap".to_string(),
            BeadOverlay {
                bead_id: "at-cap".into(),
                state: OverlayState::HumanHeld,
                attempt: 10,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(22),
                branch: Some("factory/at-cap-r10".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "over-cap".to_string(),
            BeadOverlay {
                bead_id: "over-cap".into(),
                state: OverlayState::HumanHeld,
                attempt: 12,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(33),
                branch: Some("factory/over-cap-r12".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for (bead_id, session_id, park_reason) in [
            ("legacy-null", None, None),
            (
                "recoverable-reason-live-session",
                Some("possibly-live".to_string()),
                Some("session_stalled".to_string()),
            ),
            (
                "unknown-reason",
                None,
                Some("future_unknown_reason".to_string()),
            ),
        ] {
            overlays.insert(
                bead_id.to_string(),
                BeadOverlay {
                    bead_id: bead_id.to_string(),
                    state: OverlayState::HumanHeld,
                    attempt: 2,
                    reroll_count: 0,
                    autonomy_secs: 100,
                    spend_usd: 0.0,
                    pr_number: None,
                    branch: Some(format!("factory/{bead_id}-r2")),
                    session_id,
                    is_adopted: false,
                    spawn_failure_count: 0,
                    pre_session_head_sha: None,
                    park_reason,
                    target_repo: None,
                    attempt_started_at: None,
                    session_ao_project: None,
                },
            );
        }
        // Non-HUMAN_HELD rows must never be touched.
        overlays.insert(
            "dispatched".to_string(),
            BeadOverlay {
                bead_id: "dispatched".into(),
                state: OverlayState::Dispatched,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 100,
                spend_usd: 0.0,
                pr_number: Some(44),
                branch: Some("factory/dispatched-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "ready".to_string(),
            BeadOverlay {
                bead_id: "ready".into(),
                state: OverlayState::Ready,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(55),
                branch: Some("factory/ready-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(recovered.len(), 1, "only `below` should be recovered");
        assert_eq!(recovered[0].bead_id, "below");

        // `below` was requeued and reset
        let below = store.load("below").unwrap().unwrap();
        assert_eq!(below.state, OverlayState::Queued);
        assert_eq!(below.attempt, 3);
        assert_eq!(below.autonomy_secs, 0);
        // P2 (Codex): stale PR metadata must be cleared on requeue so the
        // next dispatch doesn't conflate the fresh attempt with the dead PR.
        assert_eq!(
            below.pr_number, None,
            "recover_human_held must clear pr_number to prevent stale-PR churn"
        );
        assert_eq!(
            below.session_id, None,
            "recover_human_held must clear session_id (belonged to the prior attempt)"
        );
        // branch is intentionally preserved so the recovered_from telemetry
        // can still record what was being worked on; dispatch will rewrite
        // it on the next attempt.
        assert_eq!(
            below.branch.as_deref(),
            Some("factory/below-r2"),
            "recover_human_held keeps branch as a stale-attempt breadcrumb"
        );

        // `at-cap` and `over-cap` are still HUMAN_HELD
        let at_cap = store.load("at-cap").unwrap().unwrap();
        assert_eq!(at_cap.state, OverlayState::HumanHeld);
        assert_eq!(at_cap.attempt, 10);
        let over_cap = store.load("over-cap").unwrap().unwrap();
        assert_eq!(over_cap.state, OverlayState::HumanHeld);
        assert_eq!(over_cap.attempt, 12);
        let capped = store.human_held_at_or_above_attempt(10).unwrap();
        let capped_ids: std::collections::HashSet<_> = capped
            .iter()
            .map(|overlay| overlay.bead_id.as_str())
            .collect();
        assert_eq!(capped_ids.len(), 2);
        assert!(capped_ids.contains("at-cap"));
        assert!(capped_ids.contains("over-cap"));

        for bead_id in [
            "legacy-null",
            "recoverable-reason-live-session",
            "unknown-reason",
        ] {
            let held = store.load(bead_id).unwrap().unwrap();
            assert_eq!(held.state, OverlayState::HumanHeld, "{bead_id}");
            assert_eq!(held.attempt, 2, "{bead_id}");
        }

        // Non-HUMAN_HELD rows are untouched
        let dispatched = store.load("dispatched").unwrap().unwrap();
        assert_eq!(dispatched.state, OverlayState::Dispatched);
        assert_eq!(dispatched.autonomy_secs, 100);
        let ready = store.load("ready").unwrap().unwrap();
        assert_eq!(ready.state, OverlayState::Ready);
    }

    /// bead jleechan-4jn1 (live incident jleechan-93ft / PR
    /// worldarchitect.ai#7888): a bead parked HUMAN_HELD by the circuit
    /// breaker (`park_reason` starting with `"circuit-breaker"`) must NOT be
    /// requeued by `recover_human_held`, even though its `attempt` is well
    /// under the recovery cap — the circuit breaker parks it specifically to
    /// STOP the same reviewer/feedback loop from re-triggering. A bead
    /// parked for a transient reason (`session_stalled`, mirroring
    /// `tick::run_tick`'s wedge-detection park) at the exact same attempt
    /// must still be recovered normally. This is the behavioral difference
    /// this bead's fix depends on: before the fix, `recover_human_held`'s
    /// SQL (`WHERE state = 'HUMAN_HELD' AND attempt < ?1`) could not tell
    /// the two apart and requeued both identically.
    #[test]
    fn recover_human_held_excludes_circuit_breaker_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "circuit-broken".to_string(),
            BeadOverlay {
                bead_id: "circuit-broken".into(),
                state: OverlayState::HumanHeld,
                attempt: 6,
                reroll_count: 3,
                autonomy_secs: 500,
                spend_usd: 0.0,
                pr_number: Some(7888),
                branch: Some("factory/circuit-broken-r6".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some(crate::reroll::CIRCUIT_BREAKER_PARK_REASON.to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "transient-stalled".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; the circuit-breaker park must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled");

        // The circuit-breaker-parked bead is untouched: still HUMAN_HELD,
        // same attempt, park_reason preserved. This is the exact regression
        // this bead fixes — production requeued this bead 21 seconds after
        // park and re-triggered the same rejected fix 769 times in 30
        // minutes.
        let circuit_broken = store.load("circuit-broken").unwrap().unwrap();
        assert_eq!(
            circuit_broken.state,
            OverlayState::HumanHeld,
            "circuit-breaker park must NOT be auto-requeued"
        );
        assert_eq!(circuit_broken.attempt, 6, "attempt must not be bumped");
        assert_eq!(
            circuit_broken.park_reason.as_deref(),
            Some(crate::reroll::CIRCUIT_BREAKER_PARK_REASON),
            "park_reason must survive an excluded recovery pass"
        );

        // The transient park recovers exactly like the pre-existing
        // `session_stalled` / `autonomy_timebox_exceeded` behavior: QUEUED,
        // attempt bumped, autonomy reset, park_reason cleared.
        let transient = store.load("transient-stalled").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
        assert_eq!(transient.autonomy_secs, 0);
        assert_eq!(
            transient.park_reason, None,
            "recover_human_held clears park_reason once a bead is back in play"
        );
    }

    /// Adversarial /er review of PR #834: `HumanHoldReason::RouterError`
    /// was added to `recoverable_prefix_values()`/`is_recoverable_value()`
    /// (the in-memory predicate) but `SqliteStateStore::recover_human_held`'s
    /// real SQL query bound only `ROUTER_PARSE_PARK_REASON_PREFIX` as its
    /// prefix parameter — never updated to add
    /// `ROUTER_ERROR_PARK_REASON_PREFIX`. The shipped unit test used
    /// `FakeStateStore` (which delegates to the in-memory predicate and so
    /// passed regardless), masking that a `router_error:`-parked bead could
    /// never actually recover against the real store: permanently stuck
    /// from attempt #1, with no path to the attempt-cap escalation either
    /// (since `attempt` never increments without a recovery). This test
    /// exercises the real `SqliteStateStore` directly, mirroring the
    /// circuit-breaker/unmapped-repo tests above rather than the Fake.
    #[test]
    fn recover_human_held_recovers_router_error_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        store
            .save(&BeadOverlay {
                bead_id: "router-error-parked".into(),
                state: OverlayState::HumanHeld,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 60,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/router-error-parked-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some(
                    HumanHoldReason::RouterError("simulated non-Parse judge failure".into())
                        .value(),
                ),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            })
            .unwrap();

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "a router_error: park must be recovered by the real SqliteStateStore, \
             not just the FakeStateStore's in-memory predicate"
        );
        assert_eq!(recovered[0].bead_id, "router-error-parked");
        assert_eq!(recovered[0].state, OverlayState::Queued);
        assert_eq!(recovered[0].attempt, 2);
        assert_eq!(recovered[0].park_reason, None);
    }

    /// jleechan-35y4 (adversarial review of PR #245): a bead parked
    /// HUMAN_HELD with `park_reason = "unmapped_target_repo"` (bead
    /// jleechan-35y4's own dispatch-time park — see
    /// `dispatch::dispatch_ready`) must NOT be auto-requeued, for the same
    /// reason circuit-breaker parks aren't: an unmapped repo is a config
    /// problem no bare requeue fixes. Without this exclusion the bead would
    /// ping-pong HUMAN_HELD -> QUEUED -> re-park identically every recovery
    /// cycle instead of staying HUMAN_HELD until an operator fixes the
    /// config. A same-attempt transient park (`session_stalled`) must still
    /// recover normally, exactly like the circuit-breaker test above.
    #[test]
    fn recover_human_held_excludes_unmapped_target_repo_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "unmapped-repo".to_string(),
            BeadOverlay {
                bead_id: "unmapped-repo".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("unmapped_target_repo".to_string()),
                target_repo: Some("someorg/unrelated-repo".to_string()),
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "transient-stalled".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; the unmapped-repo park must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled");

        let unmapped = store.load("unmapped-repo").unwrap().unwrap();
        assert_eq!(
            unmapped.state,
            OverlayState::HumanHeld,
            "unmapped_target_repo park must NOT be auto-requeued"
        );
        assert_eq!(unmapped.attempt, 2, "attempt must not be bumped");
        assert_eq!(
            unmapped.park_reason.as_deref(),
            Some("unmapped_target_repo"),
            "park_reason must survive an excluded recovery pass"
        );

        let transient = store.load("transient-stalled").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
    }

    #[test]
    fn recover_human_held_recovers_target_checkout_unconfigured_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "target-checkout-unconfigured-recovered".to_string(),
            BeadOverlay {
                bead_id: "target-checkout-unconfigured-recovered".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/target-checkout-reconfigured-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("target_checkout_unconfigured".to_string()),
                target_repo: Some("jleechanorg/dark-factory".to_string()),
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "target-checkout-unconfigured-capped".to_string(),
            BeadOverlay {
                bead_id: "target-checkout-unconfigured-capped".into(),
                state: OverlayState::HumanHeld,
                attempt: 10,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/target-checkout-reconfigured-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("target_checkout_unconfigured".to_string()),
                target_repo: Some("jleechanorg/worldarchitect.ai".to_string()),
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "exactly one target_checkout_unconfigured overlay should be recovered"
        );
        assert_eq!(
            recovered[0].bead_id,
            "target-checkout-unconfigured-recovered",
            "the below-cap attempt should be the recovered overlay"
        );

        let recovered_overlay = store
            .load("target-checkout-unconfigured-recovered")
            .unwrap()
            .unwrap();
        assert_eq!(recovered_overlay.state, OverlayState::Queued);
        assert_eq!(recovered_overlay.attempt, 3);
        assert_eq!(
            recovered_overlay.park_reason, None,
            "recover_human_held clears park_reason on recovered rows"
        );

        let capped = store
            .load("target-checkout-unconfigured-capped")
            .unwrap()
            .unwrap();
        assert_eq!(capped.state, OverlayState::HumanHeld);
        assert_eq!(capped.attempt, 10);
    }

    /// jleechan-8jxr r2: a bead parked HUMAN_HELD with
    /// `park_reason = "unmapped_repo"` (dispatch.rs's "no repo identity at
    /// all" gate — distinct from `unmapped_target_repo` which means "I
    /// resolved a repo and it's not in [repos]") must NOT be auto-requeued.
    /// Requeueing would re-park with the same reason forever (intake did
    /// not change `overlay.target_repo`), burning recovery cycles without
    /// making progress until an operator supplies an explicit
    /// `target_repo:` body field or `external_ref` on the bead. A
    /// transient park (`session_stalled`) at the same attempt must still
    /// recover normally — same shape as the `unmapped_target_repo` /
    /// `worktree_remote_mismatch` exclusion tests.
    #[test]
    fn recover_human_held_excludes_unmapped_repo_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "no-identity".to_string(),
            BeadOverlay {
                bead_id: "no-identity".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("unmapped_repo".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "transient-stalled".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: Some("owner/repo".to_string()),
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; the unmapped_repo park must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled");

        let no_identity = store.load("no-identity").unwrap().unwrap();
        assert_eq!(
            no_identity.state,
            OverlayState::HumanHeld,
            "unmapped_repo park must NOT be auto-requeued"
        );
        assert_eq!(no_identity.attempt, 2, "attempt must not be bumped");
        assert_eq!(
            no_identity.park_reason.as_deref(),
            Some("unmapped_repo"),
            "park_reason must survive an excluded recovery pass"
        );

        let transient = store.load("transient-stalled").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
    }

    /// jleechan-bqdv Stage C: `worktree_remote_mismatch` parks must be
    /// excluded from `recover_human_held`'s auto-requeue exactly like
    /// `unmapped_target_repo` — this is the "fail loud, never guess"
    /// spawn-time park for a coder worktree whose git remote doesn't match
    /// the bead's resolved repo (jleechan-9sh5 discipline). Without this
    /// exclusion the bead would ping-pong HUMAN_HELD -> QUEUED -> re-spawn
    /// into the same (still misconfigured) worktree every recovery cycle.
    #[test]
    fn recover_human_held_excludes_worktree_remote_mismatch_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "wrong-remote".to_string(),
            BeadOverlay {
                bead_id: "wrong-remote".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/wrong-remote-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("worktree_remote_mismatch".to_string()),
                target_repo: Some("jleechanorg/worldarchitect.ai".to_string()),
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "transient-stalled-2".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled-2".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-2-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "cleanup-failed".to_string(),
            BeadOverlay {
                bead_id: "cleanup-failed".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/cleanup-failed-r2".into()),
                session_id: Some("still-live-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("spawn_cleanup_failed".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "remote-unverifiable".to_string(),
            BeadOverlay {
                bead_id: "remote-unverifiable".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/remote-unverifiable-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("worktree_remote_unverifiable".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        overlays.insert(
            "branch-mismatch-cleanup-failed".to_string(),
            BeadOverlay {
                bead_id: "branch-mismatch-cleanup-failed".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/branch-mismatch-cleanup-failed-r2".into()),
                session_id: Some("still-live-wrong-branch-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("spawn_branch_mismatch".to_string()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; permanent remote/cleanup safety parks must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled-2");

        let wrong_remote = store.load("wrong-remote").unwrap().unwrap();
        assert_eq!(
            wrong_remote.state,
            OverlayState::HumanHeld,
            "worktree_remote_mismatch park must NOT be auto-requeued"
        );
        assert_eq!(wrong_remote.attempt, 2, "attempt must not be bumped");
        assert_eq!(
            wrong_remote.park_reason.as_deref(),
            Some("worktree_remote_mismatch"),
            "park_reason must survive an excluded recovery pass"
        );

        let cleanup_failed = store.load("cleanup-failed").unwrap().unwrap();
        assert_eq!(cleanup_failed.state, OverlayState::HumanHeld);
        assert_eq!(
            cleanup_failed.session_id.as_deref(),
            Some("still-live-session"),
            "the known live session identity must survive recovery"
        );
        assert_eq!(
            cleanup_failed.park_reason.as_deref(),
            Some("spawn_cleanup_failed")
        );

        let remote_unverifiable = store.load("remote-unverifiable").unwrap().unwrap();
        assert_eq!(remote_unverifiable.state, OverlayState::HumanHeld);
        assert_eq!(
            remote_unverifiable.park_reason.as_deref(),
            Some("worktree_remote_unverifiable")
        );

        let branch_mismatch = store
            .load("branch-mismatch-cleanup-failed")
            .unwrap()
            .unwrap();
        assert_eq!(branch_mismatch.state, OverlayState::HumanHeld);
        assert_eq!(
            branch_mismatch.session_id.as_deref(),
            Some("still-live-wrong-branch-session")
        );
        assert_eq!(
            branch_mismatch.park_reason.as_deref(),
            Some("spawn_branch_mismatch")
        );

        let transient = store.load("transient-stalled-2").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
    }

    /// P2 (Codex review): `recover_human_held` must return ONLY the rows
    /// that were actually requeued — not every QUEUED bead with
    /// `autonomy_secs = 0` (which is what the original heuristic-query did).
    /// Pre-existing QUEUED beads with `autonomy_secs = 0` would otherwise
    /// be reported as RECOVERED_FROM_HELD and pollute telemetry + the
    /// tick summary. Run with a populated QUEUED backlog + a single
    /// recoverable HUMAN_HELD bead, and assert the returned vec contains
    /// only the bead we just recovered.
    #[test]
    fn recover_human_held_returns_only_rows_actually_recovered() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        // Pre-existing QUEUED backlog with autonomy_secs=0 (these are
        // exactly the rows that would have been incorrectly reported as
        // RECOVERED_FROM_HELD before the fix).
        for (id, attempt) in [("queued-a", 1), ("queued-b", 2), ("queued-c", 3)] {
            store
                .save(&BeadOverlay {
                    bead_id: id.into(),
                    state: OverlayState::Queued,
                    attempt,
                    reroll_count: 0,
                    autonomy_secs: 0,
                    spend_usd: 0.0,
                    pr_number: None,
                    branch: None,
                    session_id: None,
                    is_adopted: false,
                    spawn_failure_count: 0,
                    pre_session_head_sha: None,
                    park_reason: None,
                    target_repo: None,
                    attempt_started_at: None,
                    session_ao_project: None,
                })
                .unwrap();
        }

        // One recoverable HUMAN_HELD bead.
        store
            .save(&BeadOverlay {
                bead_id: "held-only".into(),
                state: OverlayState::HumanHeld,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 4321,
                spend_usd: 0.0,
                pr_number: Some(7777),
                branch: Some("factory/held-only-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("transient_spawn_retry_cap_exceeded".into()),
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            })
            .unwrap();

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the HUMAN_HELD row should be reported as recovered"
        );
        assert_eq!(recovered[0].bead_id, "held-only");

        // Pre-existing QUEUED rows are untouched.
        for id in ["queued-a", "queued-b", "queued-c"] {
            let o = store.load(id).unwrap().unwrap();
            assert_eq!(o.state, OverlayState::Queued, "{id} must stay QUEUED");
            assert_eq!(o.autonomy_secs, 0, "{id} autonomy_secs unchanged");
        }
    }

    #[test]
    fn recover_human_held_router_prefix_is_case_sensitive_and_not_a_like_pattern() {
        let store = store();
        for (bead_id, park_reason) in [
            (
                "valid-router-prefix",
                HumanHoldReason::RouterParse("valid typed reason".into()).value(),
            ),
            (
                "underscore-wildcard-lookalike",
                "routerXparseYerror: not a typed reason".into(),
            ),
            (
                "uppercase-lookalike",
                "ROUTER_PARSE_ERROR: not a typed reason".into(),
            ),
        ] {
            store
                .save(&BeadOverlay {
                    bead_id: bead_id.into(),
                    state: OverlayState::HumanHeld,
                    attempt: 2,
                    reroll_count: 0,
                    autonomy_secs: 10,
                    spend_usd: 0.0,
                    pr_number: None,
                    branch: Some(format!("factory/{bead_id}-r2")),
                    session_id: None,
                    is_adopted: false,
                    spawn_failure_count: 0,
                    pre_session_head_sha: None,
                    park_reason: Some(park_reason),
                    target_repo: None,
                    attempt_started_at: None,
                    session_ao_project: None,
                })
                .unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].bead_id, "valid-router-prefix");

        for bead_id in ["underscore-wildcard-lookalike", "uppercase-lookalike"] {
            let held = store.load(bead_id).unwrap().unwrap();
            assert_eq!(held.state, OverlayState::HumanHeld, "{bead_id}");
            assert_eq!(held.attempt, 2, "{bead_id}");
            assert_eq!(held.session_id, None, "{bead_id}");
        }
    }

    #[test]
    fn recover_human_held_revalidates_a_concurrent_live_session_at_write_time() {
        let path = std::env::temp_dir().join(format!(
            "afd_recovery_wal_race_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let recovery_store = SqliteStateStore::open(&path).unwrap();
        let writer_store = SqliteStateStore::open(&path).unwrap();
        let overlay = BeadOverlay {
            bead_id: "wal-race".into(),
            state: OverlayState::HumanHeld,
            attempt: 2,
            reroll_count: 0,
            autonomy_secs: 60,
            spend_usd: 0.0,
            pr_number: Some(293),
            branch: Some("factory/wal-race-r2".into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some("pre-spawn-sha".into()),
            park_reason: Some(HumanHoldReason::SessionStalled.value()),
            target_repo: Some("jleechanorg/dark-factory".into()),
            attempt_started_at: None,
            session_ao_project: None,
        };
        recovery_store.save(&overlay).unwrap();

        // Connection B commits the external fact recovery must not erase:
        // an active session is now durably attached. Hold BEGIN IMMEDIATE
        // open so connection A's atomic recovery reaches SQLite and blocks.
        let journal_mode: String = writer_store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        writer_store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer_store
            .conn
            .execute(
                "UPDATE bead_overlay \
                 SET session_id = ?1, park_reason = ?2 \
                 WHERE bead_id = 'wal-race'",
                params![
                    "live-session-from-connection-b",
                    HumanHoldReason::AdoptedSessionAlreadyActive.value(),
                ],
            )
            .unwrap();

        RECOVERY_BUSY_HANDLER_ENTERED.store(false, std::sync::atomic::Ordering::SeqCst);
        recovery_store
            .conn
            .busy_handler(Some(signal_recovery_busy))
            .unwrap();
        let recovery_thread = std::thread::spawn(move || {
            let result = recovery_store.recover_human_held(10);
            (recovery_store, result)
        });

        // The busy callback is the deterministic synchronization point: the
        // recovery UPDATE has attempted to acquire the WAL writer lock while
        // connection B's live-session write is still uncommitted.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !RECOVERY_BUSY_HANDLER_ENTERED.load(std::sync::atomic::Ordering::SeqCst)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert!(
            RECOVERY_BUSY_HANDLER_ENTERED.load(std::sync::atomic::Ordering::SeqCst),
            "recovery never contended on connection B's WAL writer lock"
        );
        writer_store.conn.execute_batch("COMMIT").unwrap();

        let (recovery_store, recovered) = recovery_thread.join().unwrap();
        let recovered = recovered.unwrap();
        assert!(
            recovered.is_empty(),
            "the atomic write predicate must revalidate after the concurrent commit"
        );
        let held = recovery_store.load("wal-race").unwrap().unwrap();
        assert_eq!(held.state, OverlayState::HumanHeld);
        assert_eq!(held.attempt, 2);
        assert_eq!(
            held.session_id.as_deref(),
            Some("live-session-from-connection-b")
        );
        assert_eq!(
            held.park_reason.as_deref(),
            Some("adopted_session_already_active")
        );

        drop(writer_store);
        drop(recovery_store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    }

    /// jleechan-54ky: `list_active_overlays` returns the active set
    /// unchanged (no implicit increment). `bump_autonomy_secs` advances a
    /// single row. Together they replace the old
    /// `increment_active_autonomy` SQL update + select in a way that lets
    /// the caller skip rows whose PR has `ci_pending=true` (the
    /// sub-fix-for-gib autonomy pause).
    #[test]
    fn list_active_overlays_does_not_bump_and_bump_autonomy_secs_targets_one_row() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        store
            .save(&BeadOverlay {
                bead_id: "d".into(),
                state: OverlayState::Dispatched,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 100,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/d-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            })
            .unwrap();
        store
            .save(&BeadOverlay {
                bead_id: "a".into(),
                state: OverlayState::Attested,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 200,
                spend_usd: 0.0,
                pr_number: Some(7),
                branch: Some("factory/a-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            })
            .unwrap();
        store
            .save(&BeadOverlay {
                bead_id: "h".into(),
                state: OverlayState::HumanHeld,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 300,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/h-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
                session_ao_project: None,
            })
            .unwrap();

        let active = store.list_active_overlays().unwrap();
        assert_eq!(active.len(), 2, "DISPATCHED + ATTESTED only");
        let names: Vec<&str> = active.iter().map(|o| o.bead_id.as_str()).collect();
        assert!(names.contains(&"d"));
        assert!(names.contains(&"a"));

        // list_active_overlays must NOT have bumped autonomy_secs
        let d_before = store.load("d").unwrap().unwrap();
        assert_eq!(
            d_before.autonomy_secs, 100,
            "list_active_overlays must not bump autonomy_secs"
        );

        // bump_autonomy_secs targets exactly the row named
        store.bump_autonomy_secs("d", 50).unwrap();
        store.bump_autonomy_secs("a", 75).unwrap();
        store.bump_autonomy_secs("h", 9999).unwrap(); // HUMAN_HELD row still bumps

        let d_after = store.load("d").unwrap().unwrap();
        assert_eq!(d_after.autonomy_secs, 150);
        let a_after = store.load("a").unwrap().unwrap();
        assert_eq!(a_after.autonomy_secs, 275);
        let h_after = store.load("h").unwrap().unwrap();
        assert_eq!(h_after.autonomy_secs, 10299);
    }

    // 1s2q-escalation-dedup: the escalation_ledger migration + dedup logic.

    #[test]
    fn escalation_ledger_table_present_after_open() {
        let s = store();
        let count: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "escalation_ledger table must exist after open");
    }

    #[test]
    fn ensure_escalation_ledger_table_migrates_legacy_db() {
        // Simulate a legacy DB that pre-dates the escalation_ledger table: open
        // an in-memory connection, create ONLY bead_overlay (minimal), then run
        // the migration helper and verify the table appears.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE bead_overlay (\
               bead_id TEXT PRIMARY KEY, state TEXT NOT NULL, updated_at TEXT NOT NULL\
             )",
        )
        .unwrap();
        // No escalation_ledger yet.
        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0);
        SqliteStateStore::ensure_escalation_ledger_table(&conn).unwrap();
        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1);
        // Idempotent: running again is a no-op.
        SqliteStateStore::ensure_escalation_ledger_table(&conn).unwrap();
        let still: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(still, 1);
    }

    #[test]
    fn escalation_dedup_no_prior_record_emits() {
        let s = store();
        // No prior ledger row for this (bead_id, reason) — should emit.
        assert!(s
            .escalation_should_emit("bead-a", "some_reason", "hash-1", 1000, 3600)
            .unwrap());
    }

    #[test]
    fn escalation_dedup_same_hash_within_backoff_suppresses() {
        let s = store();
        s.record_escalation_emit("bead-a", "some_reason", "hash-1", 1000)
            .unwrap();
        // Same hash, only 100s later (< 3600s backoff) — suppress.
        assert!(!s
            .escalation_should_emit("bead-a", "some_reason", "hash-1", 1100, 3600)
            .unwrap());
    }

    #[test]
    fn escalation_dedup_same_hash_past_backoff_re_emits() {
        let s = store();
        s.record_escalation_emit("bead-a", "some_reason", "hash-1", 1000)
            .unwrap();
        // Same hash, but 4000s later (>= 3600s backoff) — re-emit.
        assert!(s
            .escalation_should_emit("bead-a", "some_reason", "hash-1", 5000, 3600)
            .unwrap());
    }

    #[test]
    fn escalation_dedup_hash_changed_re_emits_regardless_of_backoff() {
        let s = store();
        s.record_escalation_emit("bead-a", "some_reason", "hash-1", 1000)
            .unwrap();
        // Different hash, only 10s later — re-emit (context changed).
        assert!(s
            .escalation_should_emit("bead-a", "some_reason", "hash-2", 1010, 3600)
            .unwrap());
    }

    #[test]
    fn escalation_dedup_record_upserts_ledger_row() {
        let s = store();
        s.record_escalation_emit("bead-a", "some_reason", "hash-1", 1000)
            .unwrap();
        // Upsert: same (bead_id, reason), new hash + epoch.
        s.record_escalation_emit("bead-a", "some_reason", "hash-2", 5000)
            .unwrap();
        // The new hash should now be the stored one; same-hash within backoff
        // suppresses for hash-2, while hash-1 is gone.
        assert!(!s
            .escalation_should_emit("bead-a", "some_reason", "hash-2", 5100, 3600)
            .unwrap());
        // hash-1 is now a "changed" hash relative to the stored hash-2 → emit.
        assert!(s
            .escalation_should_emit("bead-a", "some_reason", "hash-1", 5100, 3600)
            .unwrap());
    }

    #[test]
    fn escalation_dedup_keys_are_per_reason() {
        let s = store();
        s.record_escalation_emit("bead-a", "reason-x", "hash-1", 1000)
            .unwrap();
        // Same bead, DIFFERENT reason — no prior record for that reason → emit.
        assert!(s
            .escalation_should_emit("bead-a", "reason-y", "hash-1", 1010, 3600)
            .unwrap());
    }

    // 1s2q-escalation-dedup Task 2: terminal ("escalation_undeliverable") rows.

    #[test]
    fn mark_escalation_undeliverable_sets_terminal_flag_on_fresh_row() {
        let s = store();
        // No prior ledger row — mark_escalation_undeliverable inserts a
        // terminal row.
        s.mark_escalation_undeliverable("bead-perm", "human_held_recovery_attempt_cap_reached")
            .unwrap();
        // escalation_should_emit must now return false unconditionally.
        assert!(!s
            .escalation_should_emit(
                "bead-perm",
                "human_held_recovery_attempt_cap_reached",
                "any-hash",
                999_999,
                0,
            )
            .unwrap());
    }

    #[test]
    fn mark_escalation_undeliverable_flips_existing_row_to_terminal() {
        let s = store();
        // A prior non-terminal row exists (e.g. a transient failure was
        // dedup-recorded on a previous tick).
        s.record_escalation_emit("bead-perm", "some_reason", "hash-1", 1000)
            .unwrap();
        // Same hash within backoff would suppress normally...
        assert!(!s
            .escalation_should_emit("bead-perm", "some_reason", "hash-1", 1100, 3600)
            .unwrap());
        // ...but a changed hash would re-emit. After marking terminal, even a
        // changed hash must NOT re-emit.
        s.mark_escalation_undeliverable("bead-perm", "some_reason")
            .unwrap();
        assert!(!s
            .escalation_should_emit("bead-perm", "some_reason", "totally-new-hash", 999_999, 0)
            .unwrap());
    }

    #[test]
    fn terminal_row_suppresses_regardless_of_hash_or_backoff() {
        let s = store();
        s.mark_escalation_undeliverable("bead-t", "reason-t")
            .unwrap();
        // Changed hash, zero backoff window — would normally emit, but terminal
        // flag suppresses unconditionally.
        assert!(!s
            .escalation_should_emit("bead-t", "reason-t", "new-hash", 0, 0)
            .unwrap());
        // Same/any epoch, any backoff — still suppressed.
        assert!(!s
            .escalation_should_emit("bead-t", "reason-t", "another-hash", 10_000_000, 1)
            .unwrap());
    }

    #[test]
    fn terminal_marker_is_per_reason() {
        let s = store();
        s.mark_escalation_undeliverable("bead-t", "reason-t")
            .unwrap();
        // A DIFFERENT reason on the same bead is unaffected — still emits.
        assert!(s
            .escalation_should_emit("bead-t", "reason-other", "hash-1", 100, 3600)
            .unwrap());
    }

    #[test]
    fn record_escalation_emit_does_not_clear_terminal() {
        let s = store();
        s.mark_escalation_undeliverable("bead-t", "reason-t")
            .unwrap();
        // A late record_escalation_emit (e.g. from a race) must not flip
        // terminal back off.
        s.record_escalation_emit("bead-t", "reason-t", "hash-late", 5000)
            .unwrap();
        assert!(!s
            .escalation_should_emit("bead-t", "reason-t", "hash-late", 999_999, 0)
            .unwrap());
    }

    #[test]
    fn ensure_escalation_ledger_terminal_column_migrates_legacy_db() {
        // Simulate a legacy DB that got escalation_ledger from a pre-Task-2
        // ensure_escalation_ledger_table (no terminal column).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE escalation_ledger (\
               bead_id TEXT NOT NULL,\
               reason TEXT NOT NULL,\
               context_hash TEXT NOT NULL,\
               last_emitted_epoch INTEGER NOT NULL,\
               PRIMARY KEY (bead_id, reason)\
             )",
        )
        .unwrap();
        // Insert a pre-existing row without the terminal column.
        conn.execute(
            "INSERT INTO escalation_ledger (bead_id, reason, context_hash, last_emitted_epoch) \
             VALUES ('b-legacy', 'r1', 'h1', 100)",
            [],
        )
        .unwrap();
        // No terminal column yet.
        let has_col_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('escalation_ledger') \
                 WHERE name = 'terminal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_col_before, 0);

        SqliteStateStore::ensure_escalation_ledger_terminal_column(&conn).unwrap();

        // Column now present, defaulting existing rows to 0 (not terminal).
        let terminal_val: i64 = conn
            .query_row(
                "SELECT terminal FROM escalation_ledger WHERE bead_id = 'b-legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(terminal_val, 0);
        // Idempotent: a second run is a no-op.
        SqliteStateStore::ensure_escalation_ledger_terminal_column(&conn).unwrap();
        let terminal_val2: i64 = conn
            .query_row(
                "SELECT terminal FROM escalation_ledger WHERE bead_id = 'b-legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(terminal_val2, 0);
    }

    #[test]
    fn escalation_ledger_terminal_column_present_after_open() {
        let s = store();
        let has_col: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('escalation_ledger') \
                 WHERE name = 'terminal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_col, 1, "terminal column must exist after open");
    }

    /// Bead jleechan-g1ib: claim/release/heartbeat round-trip and atomic
    /// contention (a second machine cannot steal a live claim).
    #[test]
    fn claim_lifecycle_round_trips_and_rejects_concurrent_claim() {
        let s = store();
        // Fresh claim succeeds.
        assert!(s.try_claim("bead-1", "jeff-ubuntu", 100, 60).unwrap());
        // Different machine within TTL is refused (the row is already
        // claimed; same-machine re-claim ALSO returns false — callers
        // should use `heartbeat_claim` to refresh).
        assert!(!s.try_claim("bead-1", "mac", 110, 60).unwrap());
        // Heartbeat by another machine is also refused.
        assert!(!s.heartbeat_claim("bead-1", "mac", 115, 60).unwrap());
        // Owner heartbeat refreshes.
        assert!(s.heartbeat_claim("bead-1", "jeff-ubuntu", 120, 60).unwrap());
        // Release by another machine is a no-op.
        s.release_claim("bead-1", "mac").unwrap();
        // Owner release succeeds; the row is now unclaimed.
        s.release_claim("bead-1", "jeff-ubuntu").unwrap();
        assert!(s.try_claim("bead-1", "mac", 130, 60).unwrap());
    }

    /// Bead jleechan-g1ib: a stale claim (claimed_at < now - ttl_secs) is
    /// expired by try_claim, so another machine can claim a crashed
    /// owner's bead.
    #[test]
    fn stale_claim_is_expired_by_try_claim() {
        let s = store();
        // Owner claims at epoch 100 with ttl=60.
        assert!(s.try_claim("bead-1", "jeff-ubuntu", 100, 60).unwrap());
        // 200 seconds later (well past 100+60=160), another machine claims.
        assert!(s.try_claim("bead-1", "mac", 200, 60).unwrap());
    }

    /// Bead jleechan-g1ib: a peer-reported claim refuses a local attempt
    /// (peer_claim_taken short-circuits try_claim).
    #[test]
    fn peer_claim_taken_blocks_local_attempt() {
        let s = store();
        let now = 1000;
        let claims = vec![(
            "jeff-ubuntu".to_string(),
            "bead-shared".to_string(),
            now,
            now + 60,
        )];
        s.replace_peer_claims(&claims, now).unwrap();
        // Within the peer's reported TTL: peer_claim_taken is true.
        assert!(s.peer_claim_taken("bead-shared", now + 30).unwrap());
        // Past the peer's TTL: false.
        assert!(!s.peer_claim_taken("bead-shared", now + 120).unwrap());
    }

    /// Bead jleechan-g1ib: replace_peer_claims wipes stale rows on every
    /// call so a peer that drops a claim doesn't keep haunting us.
    #[test]
    fn replace_peer_claims_wipes_previous_set() {
        let s = store();
        let now = 1000;
        // First sync: two claims.
        s.replace_peer_claims(
            &[
                ("jeff-ubuntu".to_string(), "bead-a".to_string(), now, now + 60),
                ("jeff-ubuntu".to_string(), "bead-b".to_string(), now, now + 60),
            ],
            now,
        )
        .unwrap();
        assert!(s.peer_claim_taken("bead-a", now + 1).unwrap());
        assert!(s.peer_claim_taken("bead-b", now + 1).unwrap());
        // Second sync: peer dropped bead-b. Replace wipes both rows and
        // re-inserts only bead-a.
        s.replace_peer_claims(
            &[("jeff-ubuntu".to_string(), "bead-a".to_string(), now, now + 60)],
            now + 10,
        )
        .unwrap();
        assert!(s.peer_claim_taken("bead-a", now + 11).unwrap());
        assert!(!s.peer_claim_taken("bead-b", now + 11).unwrap());
    }

    /// Bead jleechan-g1ib: list_live_local_claims respects the TTL window.
    #[test]
    fn list_live_local_claims_filters_by_ttl() {
        let s = store();
        s.try_claim("fresh", "jeff-ubuntu", 100, 60).unwrap();
        s.try_claim("stale", "jeff-ubuntu", 50, 60).unwrap();
        let claims = s.list_live_local_claims(120, 60).unwrap();
        let beads: Vec<String> = claims.iter().map(|(b, _, _)| b.clone()).collect();
        // 120 - 60 = 60 cutoff: "stale" (at=50) is excluded; "fresh" (at=100) is included.
        assert_eq!(beads, vec!["fresh".to_string()]);
    }

    /// Bead jleechan-g1ib: schema migration is idempotent (legacy DB
    /// without claim columns gets them on open; a re-open is a no-op).
    #[test]
    fn claimed_by_columns_present_after_open() {
        let s = store();
        let has_claimed_by: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'claimed_by'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_claimed_by, 1);
    }

    #[test]
    fn peer_claims_table_present_after_open() {
        let s = store();
        let has_table: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'peer_claims'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_table, 1);
    }

    /// Bead bze8.3: a dispatch reservation must stamp `attempt_started_at`
    /// atomically, clear any prior `autonomy_secs`, and persist all of
    /// that in the same row.
    #[test]
    fn dispatch_reservation_stamps_attempt_started_at_and_zeros_autonomy_secs() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let old_started_at = now_epoch - 11_000;
        store
            .save(&BeadOverlay {
                bead_id: "old-dispatch".into(),
                state: OverlayState::Dispatched,
                attempt: 5,
                reroll_count: 0,
                autonomy_secs: 11_000,
                spend_usd: 0.0,
                pr_number: Some(42),
                branch: Some("factory/old-dispatch-r5".into()),
                session_id: Some("old-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: Some(old_started_at),
                session_ao_project: None,
            })
            .unwrap();

        store
            .save(&BeadOverlay {
                bead_id: "old-dispatch".into(),
                state: OverlayState::Dispatched,
                attempt: 6,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(42),
                branch: Some("factory/old-dispatch-r6".into()),
                session_id: Some("fresh-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
                attempt_started_at: Some(now_epoch),
                session_ao_project: None,
            })
            .unwrap();

        let fresh = store.load("old-dispatch").unwrap().unwrap();
        assert_eq!(fresh.attempt_started_at, Some(now_epoch));
        assert_eq!(fresh.autonomy_secs, 0);
        assert_eq!(fresh.state, OverlayState::Dispatched);
        assert_eq!(fresh.attempt, 6);
    }

    fn adopted_overlay(bead_id: &str, state: OverlayState, pr: u64) -> BeadOverlay {
        BeadOverlay {
            bead_id: bead_id.into(),
            state,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some("factory/shared-pr".into()),
            session_id: (state == OverlayState::Dispatched).then(|| "owner-session".into()),
            session_ao_project: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: (state == OverlayState::HumanHeld).then(|| "coder_silent".into()),
            target_repo: Some("owner/repo".into()),
            attempt_started_at: None,
        }
    }

    fn adopted_identity(pr: u64, head: &str) -> AdoptedPrIdentity {
        AdoptedPrIdentity {
            repo: "owner/repo".into(),
            default_repo: "owner/repo".into(),
            pr_number: pr,
            branch: "factory/shared-pr".into(),
            head_sha: head.into(),
        }
    }

    #[test]
    fn adopted_pr_claim_coalesces_exact_active_owner_without_reassignment() {
        let s = store();
        let owner = adopted_overlay("owner-bead", OverlayState::Dispatched, 607);
        s.save(&owner).unwrap();
        s.register_branch("owner-bead", "factory/shared-pr").unwrap();
        let duplicate = adopted_overlay("duplicate-bead", OverlayState::Attested, 607);
        assert_eq!(
            s.claim_adopted_pr(&adopted_identity(607, "head-a"), &duplicate)
                .unwrap(),
            AdoptedPrClaim::CoalescedActive {
                owner_bead_id: "owner-bead".into()
            }
        );
        assert_eq!(
            s.bead_id_for_branch("factory/shared-pr").unwrap(),
            Some("owner-bead".into())
        );
        assert!(s.load("duplicate-bead").unwrap().is_none());
        assert_eq!(
            s.claim_adopted_pr(&adopted_identity(607, "head-b"), &duplicate)
                .unwrap(),
            AdoptedPrClaim::CoalescedActive {
                owner_bead_id: "owner-bead".into()
            },
            "same repo/PR/branch/owner must advance to the newly observed exact head"
        );
        let advanced_head: String = s
            .conn
            .query_row(
                "SELECT head_sha FROM adopted_pr_binding WHERE branch = ?1",
                params!["factory/shared-pr"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(advanced_head, "head-b");
    }

    #[test]
    fn adopted_pr_claim_replaces_exact_human_held_owner_and_refuses_pr_mismatch() {
        let s = store();
        let owner = adopted_overlay("owner-bead", OverlayState::HumanHeld, 607);
        s.save(&owner).unwrap();
        s.register_branch("owner-bead", "factory/shared-pr").unwrap();
        let mismatch = adopted_overlay("wrong-pr-bead", OverlayState::Attested, 642);
        assert!(matches!(
            s.claim_adopted_pr(&adopted_identity(642, "head-a"), &mismatch)
                .unwrap(),
            AdoptedPrClaim::RefusedMismatch { .. }
        ));
        assert_eq!(
            s.bead_id_for_branch("factory/shared-pr").unwrap(),
            Some("owner-bead".into())
        );
        let replacement = adopted_overlay("replacement-bead", OverlayState::Attested, 607);
        assert_eq!(
            s.claim_adopted_pr(&adopted_identity(607, "head-a"), &replacement)
                .unwrap(),
            AdoptedPrClaim::ReplacedHumanHeld {
                owner_bead_id: "owner-bead".into()
            }
        );
        assert_eq!(
            s.bead_id_for_branch("factory/shared-pr").unwrap(),
            Some("replacement-bead".into())
        );
        assert_eq!(
            s.load("replacement-bead").unwrap().unwrap().state,
            OverlayState::Attested
        );
    }

    #[test]
    fn adopted_pr_claim_rolls_back_registry_and_binding_when_candidate_save_fails() {
        for replacement in [false, true] {
            let s = store();
            if replacement {
                let owner = adopted_overlay("owner-bead", OverlayState::HumanHeld, 607);
                s.save(&owner).unwrap();
                s.register_branch("owner-bead", "factory/shared-pr").unwrap();
            }
            s.conn
                .execute_batch(
                    "CREATE TRIGGER fail_candidate_save BEFORE INSERT ON bead_overlay \
                     WHEN NEW.bead_id = 'invalid-candidate' BEGIN \
                     SELECT RAISE(FAIL, 'scripted candidate save failure'); END;",
                )
                .unwrap();
            let invalid = adopted_overlay("invalid-candidate", OverlayState::Attested, 607);
            assert!(s
                .claim_adopted_pr(&adopted_identity(607, "head-a"), &invalid)
                .is_err());
            assert_eq!(
                s.bead_id_for_branch("factory/shared-pr").unwrap(),
                replacement.then(|| "owner-bead".to_string())
            );
            let binding_count: i64 = s
                .conn
                .query_row("SELECT COUNT(*) FROM adopted_pr_binding", [], |row| row.get(0))
                .unwrap();
            assert_eq!(binding_count, 0);
            assert!(s.load("invalid-candidate").unwrap().is_none());
        }
    }

    #[test]
    fn adopted_pr_claim_rolls_back_when_commit_fails() {
        let s = store();
        s.conn
            .execute_batch(
                "PRAGMA foreign_keys = ON; \
                 CREATE TABLE adopted_pr_commit_parent (id TEXT PRIMARY KEY); \
                 DROP TABLE adopted_pr_binding; \
                 CREATE TABLE adopted_pr_binding (\
                   branch TEXT PRIMARY KEY, repo TEXT NOT NULL, pr_number INTEGER NOT NULL,\
                   head_sha TEXT NOT NULL, bead_id TEXT NOT NULL, updated_at TEXT NOT NULL,\
                   FOREIGN KEY (bead_id) REFERENCES adopted_pr_commit_parent(id)\
                     DEFERRABLE INITIALLY DEFERRED\
                 );",
            )
            .unwrap();

        let candidate = adopted_overlay("commit-failure", OverlayState::Attested, 607);
        let error = s
            .claim_adopted_pr(&adopted_identity(607, "head-a"), &candidate)
            .expect_err("deferred foreign key must make COMMIT fail");
        match error {
            DaemonError::Tool { stderr, .. } => {
                assert!(stderr.contains("claim_adopted_pr commit"), "{stderr}");
                assert!(stderr.to_ascii_lowercase().contains("foreign key"), "{stderr}");
            }
            other => panic!("expected the original COMMIT error, got {other:?}"),
        }
        assert_eq!(
            s.bead_id_for_branch("factory/shared-pr").unwrap(),
            None,
            "failed COMMIT must roll back the registry write"
        );
        let binding_count: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM adopted_pr_binding", [], |row| row.get(0))
            .unwrap();
        assert_eq!(binding_count, 0, "failed COMMIT must roll back the binding write");
        assert!(
            s.load("commit-failure").unwrap().is_none(),
            "failed COMMIT must roll back the overlay write"
        );
    }

    #[test]
    fn adopted_pr_claim_two_sqlite_connections_elect_one_owner_without_split_state() {
        let path = std::env::temp_dir().join(format!(
            "dark-factory-adopted-claim-race-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        drop(SqliteStateStore::open(&path).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for bead_id in ["racer-a", "racer-b"] {
            let thread_path = path.clone();
            let thread_barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let store = SqliteStateStore::open(&thread_path).unwrap();
                let candidate = adopted_overlay(bead_id, OverlayState::Attested, 607);
                thread_barrier.wait();
                store
                    .claim_adopted_pr(&adopted_identity(607, "race-head"), &candidate)
                    .unwrap()
            }));
        }
        let claims: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, AdoptedPrClaim::Owned))
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, AdoptedPrClaim::CoalescedActive { .. }))
                .count(),
            1
        );
        let store = SqliteStateStore::open(&path).unwrap();
        let registry_owner = store
            .bead_id_for_branch("factory/shared-pr")
            .unwrap()
            .unwrap();
        let binding_owner: String = store
            .conn
            .query_row(
                "SELECT bead_id FROM adopted_pr_binding WHERE branch = ?1",
                params!["factory/shared-pr"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(binding_owner, registry_owner);
        assert!(store.load(&registry_owner).unwrap().is_some());
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn adopted_pr_claim_orphan_refusal_preserves_prior_binding_head() {
        let s = store();
        let owner = adopted_overlay("owner-bead", OverlayState::Dispatched, 607);
        s.save(&owner).unwrap();
        s.register_branch("owner-bead", "factory/shared-pr").unwrap();
        let duplicate = adopted_overlay("duplicate-bead", OverlayState::Attested, 607);
        assert!(matches!(
            s.claim_adopted_pr(&adopted_identity(607, "head-a"), &duplicate)
                .unwrap(),
            AdoptedPrClaim::CoalescedActive { .. }
        ));
        s.conn
            .execute("DELETE FROM bead_overlay WHERE bead_id = ?1", params!["owner-bead"])
            .unwrap();
        assert!(matches!(
            s.claim_adopted_pr(&adopted_identity(607, "head-b"), &duplicate)
                .unwrap(),
            AdoptedPrClaim::RefusedMismatch { .. }
        ));
        let preserved_head: String = s
            .conn
            .query_row(
                "SELECT head_sha FROM adopted_pr_binding WHERE branch = ?1",
                params!["factory/shared-pr"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_head, "head-a");
        assert_eq!(
            s.bead_id_for_branch("factory/shared-pr").unwrap(),
            Some("owner-bead".into())
        );
    }
}
