// Task 10: tick loop wiring (design doc §5, spec §4.2.2/§4.2.9). This is the
// only module that calls every other module's public entry point in one
// place: `intake::normalize`, `router::route`, `dispatch::dispatch_ready`,
// `verifier::assess`, plus `telemetry::emit` + `StateStore`. Fast tier runs
// every tick (ATTESTED beads -> verifier); slow tier (intake + route +
// dispatch) runs every `slow_tick_secs / fast_tick_secs` ticks, tracked via
// `TickCounter::slow_tier_due`. Startup reconciliation is implicit: overlays
// live in SQLite (or the caller's `StateStore` impl) and are read fresh via
// `StateStore::load` on every tick, so there is no separate in-memory cache to
// rehydrate on process restart.
//
// Stage gate (spec §4.2.9, `docs/auto-factory-daemon-spec.md` §4.2.9
// "Stage-1 substitution rule"): whenever a re-roll-worthy verdict would fire
// (an ATTESTED bead's gate assessment is not all-green), Stage 1 NEVER enters
// `RE_ROLL` or executes the Re-Roll Engine. It only emits
// `REROLL_VERDICT_RECORDED` and parks the bead `HUMAN_HELD`. `cfg.stage` is
// asserted to be `1` at the top of the gate path; any other value is a
// `DaemonError::Config` since Stage 2 execution is out of scope for this
// binary entirely (design doc says the Rust daemon IS the Stage 2 owner
// eventually, but this task only implements the Stage-1 substitution rule).
use crate::config::Config;
use crate::dispatch::{self, MAX_TRANSIENT_SPAWN_RETRY};
use crate::errors::DaemonError;
use crate::intake::{self, IntakeOutcome, IntakeVerdict};
use crate::router::{self, RoutingVerdict};
use crate::state::{
    set_human_hold_reason, BeadOverlay, HumanHoldReason, OverlayState, StateStore,
};
use crate::telemetry::{self, TelemetryEvent};
use crate::tools::{Bead, Llm, PrHeadBranch, Scm, SessionId, Sessions, Tracker, Vcs};
use crate::verifier::{self, PrEvidence};
use std::collections::HashSet;
use std::path::Path;

/// Everything one `run_tick` call needs: the five tool-boundary trait objects,
/// config, state store, and the telemetry log path. Bundled into one struct so
/// `run_tick`'s signature stays readable and every call site (the binary's
/// poll loop, `--once`, and the integration test) constructs it identically.
pub struct TickDeps<'a> {
    pub scm: &'a dyn Scm,
    pub tracker: &'a dyn Tracker,
    pub sessions: &'a dyn Sessions,
    pub llm: &'a dyn Llm,
    pub store: &'a dyn StateStore,
    pub vcs: &'a dyn Vcs,
    pub cfg: &'a Config,
    pub telemetry_log: &'a Path,
}

/// Summary counters returned by `run_tick`, mirrored into the `TICK` telemetry
/// event's `metrics` field (spec §4.2.9's "once per invocation, summarizing
/// counts").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickSummary {
    pub beads_created: usize,
    pub beads_routed: usize,
    pub beads_dispatched: usize,
    pub gates_assessed: usize,
    pub beads_ready: usize,
    pub beads_parked_human_held: usize,
    /// Beads that the automated HUMAN_HELD exit requeued back to QUEUED
    /// this tick (jleechan-gib: Rust port of shell `recover-held`).
    pub beads_recovered_from_held: usize,
    /// Beads that reached an automation cap and now require explicit
    /// operator/escalation attention instead of silent indefinite retry.
    pub beads_escalated: usize,
    /// Beads that reached an automation cap AND had no SCM comment target
    /// at all (no `pr_number`, not found in `fetch_candidates()`), so the
    /// escalation was recorded only as a local durable marker
    /// (`bead_overlay.park_reason` + `ESCALATED_LOCALLY` telemetry) instead
    /// of a GitHub comment. Counted separately from `beads_escalated` so
    /// operators can tell "posted a comment" apart from "local-only, go
    /// query `bead_overlay` yourself" (2026-07-09 live incident: 45 beads
    /// silently lost with no durable trace anywhere).
    pub beads_escalated_locally: usize,
    /// jleechan-zaga / issue #348: beads held at `DISPOSITION_REQUIRED`
    /// because every red gate is structural (re-rolling would be no-op
    /// churn). The fast tier keeps assessing on each tick; this counter
    /// just records the holds placed this tick.
    pub beads_held_disposition_required: usize,
    /// Bead jleechan-rouf (worldarchitect.ai #8428 / #8420 / #8421): an
    /// escalation event that the dedup ledger suppressed within the
    /// `escalation_refire_secs` window because the same `(branch, reason)`
    /// had already emitted. Distinct from `beads_escalated` so operators
    /// can see the dedup work without it being mistaken for new alerts.
    pub escalations_suppressed: usize,
}

/// Bounded retry cap for the automated HUMAN_HELD exit. Matches the shell
/// overlay's `recover-held` (daemon/factory-overlay.sh:319-333) and the
/// ad-hoc attempt counter cap cited in the gap-review verdict
/// (`docs/factory-goal-gap-review-2026-07-06.md` Blocker #3). Beads at or
/// above this cap are deliberately left in HUMAN_HELD for a human to
/// review — the daemon stops blindly retrying past this point.
const MAX_HUMAN_HELD_RECOVERY_ATTEMPT: u32 = 10;
const ESCALATION_REVIEWER: &str = "dark-factory-escalation";
const ESCALATION_SENTINEL_ATTEMPT: u32 = u32::MAX;

/// Task 1 (reviewer-outage-resilience): N consecutive pending assessments
/// before a provider is marked in-outage in the `vendor_health` ledger.
const VENDOR_OUTAGE_CONSECUTIVE_PENDING_THRESHOLD: u32 = 3;

/// Task 2 (reviewer-outage-resilience): grace period (seconds) measured from
/// the PR head commit's committer epoch after which an in-outage provider's
/// stale pending check-run status is waived so the verification step can
/// proceed and report the true CI result. 15 minutes.
const OUTAGE_GRACE_PERIOD_SECS: u64 = 900;

/// Bead jleechan-msmq: an ATTESTED bead with `reroll_count > 0` is in the
/// reroll pipeline. The OLD PR's gate verdict cannot advance the bead
/// (the reroll branch IS the advancement); re-assessing it on every
/// subsequent tick is pure churn that races with two breakers:
///
///   * the autonomy timebox — `autonomy_secs` keeps bumping, and once it
///     crosses `autonomy_timebox_secs`, the timebox-park branch calls
///     `kill_session_and_clear_handle`, killing the fresh coder lane
///     that was just fabricated by the reroll before it has a chance to
///     push a fix. Symptom: bead goes HumanHeld → recover-held → QUEUED,
///     dispatched session dead, bead ping-pongs forever.
///   * the circuit-breaker — same reviewer citing the same red gate on
///     identical evidence trips on attempt 2 and parks HUMAN_HELD, even
///     though the fresh lane was about to land a fix.
///
/// The fix: skip the per-tick gate assessment for ATTESTED beads whose
/// `reroll_count > 0`. The reroll branch (which runs below this guard in
/// `run_fast_tier`) is the ONLY consumer of the fresh lane's progress;
/// the OLD PR's gates are not relevant once reroll has been initiated.
/// Emitting `VERIFIER_SKIPPED_REROLL_IN_PROGRESS` lets an operator tell
/// the skip from a transient snapshot error or a circuit-breaker trip.
/// Result of looking up CI-pending state for an active overlay (used by the
/// autonomy/timebox bookkeeping in `run_tick`'s active-overlay loop).
///
/// `NotApplicable` covers non-`Attested` overlays and `Attested` overlays
/// without a PR yet — the caller proceeds with normal timebox + wedge work
/// for those (the autonomy clock must keep ticking regardless of which
/// state the bead is in, except for `Attested + PR exists + ci_pending=true`
/// where CI wall-clock would falsely count against the coder's budget).
///
/// `Known(ci_pending)` is the normal path: snapshot fetched, value known.
///
/// `SnapshotUnavailable` (jleechan-qdw) is the new third state — a
/// transient `gh`/GraphQL/network hiccup on `pr_snapshot`. The caller MUST
/// skip this overlay's timebox bump, timebox-park, and wedge-check for
/// this tick (continue to the next overlay) and emit
/// `BEAD_SNAPSHOT_TRANSIENT_ERROR` with `phase: "ci_pending"` /
/// `"active_overlay"`. Leaving the bead `Attested` is deliberate: the
/// next tick retries the snapshot fetch. The previous implementation
/// collapsed `Err` to `false` here, which let the timebox-park branch
/// false-park an `Attested` bead on a single transient snapshot error
/// (bead 901 regression the audit caught).
enum CiPendingStatus {
    NotApplicable,
    Known(bool),
    SnapshotUnavailable,
}

/// `repo` (bead jleechan-9xrs, Stage D) is the bead's OWN resolved repo
/// (`overlay.repo(cfg)`) — was `scm.pr_snapshot(pr)` bound to the daemon's
/// global `cfg.target_repo`, silently wrong for a bead dispatched into a
/// different `[repos.*]` entry.
fn ci_pending_for_attested(
    overlay: &BeadOverlay,
    scm: &dyn crate::tools::Scm,
    repo: &str,
) -> CiPendingStatus {
    if overlay.state != OverlayState::Attested {
        return CiPendingStatus::NotApplicable;
    }
    match overlay.pr_number {
        None => CiPendingStatus::NotApplicable,
        Some(pr) => match scm.pr_snapshot_for_repo(repo, pr) {
            Ok(snap) => CiPendingStatus::Known(snap.ci_pending),
            Err(_) => CiPendingStatus::SnapshotUnavailable,
        },
    }
}

/// Dependency-free ISO-8601 UTC timestamp, matching `state.rs::now_iso8601`'s
/// discipline (design doc §2's five-crate budget excludes chrono). Duplicated
/// here rather than made `pub` in `state.rs` to keep that already-merged
/// Task-4 module's public surface unchanged by this task.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's civil_from_days algorithm (public domain), days since
/// epoch -> (y, m, d). Mirrors `state.rs`'s private copy exactly.
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

fn emit(
    telemetry_log: &Path,
    bead_id: &str,
    attempt_id: u32,
    lifecycle_state: &str,
    event_type: &str,
    metrics: serde_json::Value,
    context: serde_json::Value,
) -> Result<(), DaemonError> {
    telemetry::emit(
        telemetry_log,
        &TelemetryEvent {
            timestamp: now_iso8601(),
            bead_id: bead_id.to_string(),
            attempt_id,
            lifecycle_state: lifecycle_state.to_string(),
            event_type: event_type.to_string(),
            metrics,
            context,
        },
    )
}

/// jleechan-eazj: emit exactly one structured verdict event for a
/// non-adopted `intake::IntakeOutcome`. No bead exists yet for these
/// candidates, so `bead_id` is the `external_ref` (e.g.
/// `"owner/repo#8171"`) — the same identifier the operator greps for, so
/// `grep 8171 daemon.jsonl` finds this line even though no bead was ever
/// created. `ADOPTED` verdicts are NOT handled here: they already carry a
/// real bead id and are reported by the caller via INTAKE_BEAD_CREATED /
/// EXISTING_PR_ADOPTED, which predate this helper.
fn emit_intake_outcome(telemetry_log: &Path, outcome: &IntakeOutcome) -> Result<(), DaemonError> {
    let (event_type, context) = match &outcome.verdict {
        IntakeVerdict::SkippedDuplicate => (
            "SKIPPED_DUPLICATE",
            serde_json::json!({"external_ref": outcome.external_ref}),
        ),
        IntakeVerdict::SkippedFork => (
            "SKIPPED_FORK",
            serde_json::json!({"external_ref": outcome.external_ref}),
        ),
        IntakeVerdict::SkippedIneligible { precondition } => (
            "SKIPPED_INELIGIBLE",
            serde_json::json!({
                "external_ref": outcome.external_ref,
                "precondition": precondition,
            }),
        ),
        IntakeVerdict::Errored { reason } => (
            "ERRORED",
            serde_json::json!({
                "external_ref": outcome.external_ref,
                "reason": reason,
            }),
        ),
    };
    emit(
        telemetry_log,
        &outcome.external_ref,
        1,
        "INTAKE",
        event_type,
        serde_json::json!({}),
        context,
    )
}

/// Run one full tick: slow tier (intake -> route -> dispatch) then fast tier
/// (verify every ATTESTED bead), then emit exactly one summarizing `TICK`
/// event. `tick_index` selects whether the slow tier is due this call
/// (`tick_index % (slow_tick_secs / fast_tick_secs).max(1) == 0`); pass `0` to
/// always run the slow tier (used by `--once`).
///
/// Stage gate: `deps.cfg.stage` must be `1` — this function only implements
/// the Stage-1 substitution rule (re-roll verdicts recorded, never executed).
///
/// jleechan-park-leaves-zombie-session-mh9o: best-effort terminate the live
/// AO session bound to `overlay` and clear the durable session handle, so
/// every PARKED_* transition (a) does not leave a zombie session listed as
/// `[spawning]` in AO's state — which the AO dedup guard rejects as a
/// "Duplicate session detected" on the next `ao spawn` for the same bead —
/// and (b) lets the automated HUMAN_HELD exit (`recover_human_held`'s
/// `session_id IS NULL` predicate) requeue the bead if its
/// `park_reason` is in the recoverable set.
///
/// Fail-soft by design, with an important asymmetry: the durable handle is
/// cleared ONLY on a successful `stop()` (the session is provably dead) or
/// when there is no handle to begin with. On a `stop()` failure the
/// session may still be live, so the handle is RETAINED on disk — this
/// (i) prevents `recover_human_held` from requeueing a bead whose live
/// worker could overlap a freshly-spawned replacement, and (ii) gives
/// operators the durable evidence they need to retry cleanup or kill the
/// session manually. The `BEAD_SESSION_KILL_FAILED` telemetry event
/// preserves visibility into the still-leaked session.
fn kill_session_and_clear_handle(deps: &TickDeps, overlay: &mut BeadOverlay) {
    let Some(session_id_str) = overlay.session_id.clone() else {
        return;
    };
    let session_id = SessionId(session_id_str.clone());
    match deps.sessions.stop(&session_id) {
        Ok(()) => {
            let _ = emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "BEAD_SESSION_KILLED",
                serde_json::json!({}),
                serde_json::json!({
                    "session_id": session_id_str,
                    "phase": "park_transition",
                }),
            );
            // Proven dead — safe to clear the handle. Unblocks both
            // recover_human_held and any operator-driven requeue without
            // risking a duplicate worker or AO dedup collision.
            overlay.session_id = None;
        }
        Err(stop_err) => {
            // Stop failed: the session may still be live. RETAIN the handle
            // so (a) recover_human_held cannot requeue and dispatch a second
            // worker that would overlap the existing live one and (b) the
            // operator retains the durable session_id needed to retry
            // `ao session kill <id>` once AO recovers. Failure is logged
            // but never escalated — the park itself stands.
            let _ = emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "BEAD_SESSION_KILL_FAILED",
                serde_json::json!({}),
                serde_json::json!({
                    "session_id": session_id_str,
                    "error": format!("{stop_err:?}"),
                    "phase": "park_transition",
                }),
            );
        }
    }
}

pub fn run_tick(
    deps: &TickDeps,
    tick_index: u64,
    elapsed_secs: u64,
) -> Result<TickSummary, DaemonError> {
    if deps.cfg.stage != 1 && deps.cfg.stage != 2 {
        return Err(DaemonError::Config(format!(
            "run_tick only implements stage 1 or 2; got stage={}",
            deps.cfg.stage
        )));
    }

    let mut summary = TickSummary::default();

    let slow_tier_due = {
        let ratio = (deps.cfg.slow_tick_secs / deps.cfg.fast_tick_secs.max(1)).max(1);
        tick_index.is_multiple_of(ratio)
    };

    // jleechan-gib: automated HUMAN_HELD exit (Rust port of shell
    // `recover-held`). Runs at the TOP of the tick, BEFORE the active-overlay
    // wedge-detection loop, so that a bead recovered this tick cannot also be
    // parked this same tick (otherwise the wedge check would re-park a
    // freshly-QUEUED bead before dispatch can make progress). Recovery only
    // fires when the slow tier is due (matches the shell overlay's cadence
    // — `recover-held` was never per-fast-tick).
    if slow_tier_due {
        run_recovery_step(deps, &mut summary)?;
    }

    // jleechan-54ky / sub-fix for jleechan-gib: split the SQL-level "increment
    // every active row" into list + per-row bump so we can pause the autonomy
    // clock for ATTESTED rows whose PR has ci_pending=true (CI wait time is
    // operator/CI wall-clock, not coder session time we are budgeting against).
    let active_overlays = deps.store.list_active_overlays()?;
    for mut overlay in active_overlays {
        // jleechan-qdw: per-overlay isolation. A snapshot fetch error in the
        // ci_pending lookup must not bump the autonomy clock AND must not
        // run the timebox-park / wedge-check branches for this overlay —
        // any of those would risk false-parking a near-timebox bead on a
        // single transient `gh`/GraphQL/network hiccup. Skip this overlay
        // for the rest of the active-overlay loop and continue to the next.
        let active_overlay_repo = overlay.repo(deps.cfg).to_string();
        match ci_pending_for_attested(&overlay, deps.scm, &active_overlay_repo) {
            CiPendingStatus::SnapshotUnavailable => {
                let _ = emit(
                    deps.telemetry_log,
                    &overlay.bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "BEAD_SNAPSHOT_TRANSIENT_ERROR",
                    serde_json::json!({}),
                    serde_json::json!({"phase": "ci_pending", "skip": "active_overlay"}),
                );
                continue;
            }
            CiPendingStatus::NotApplicable => {
                if elapsed_secs > 0 {
                    overlay.autonomy_secs += elapsed_secs;
                    deps.store
                        .bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
                }
            }
            CiPendingStatus::Known(ci_pending) => {
                if !ci_pending && elapsed_secs > 0 {
                    overlay.autonomy_secs += elapsed_secs;
                    deps.store
                        .bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
                }
            }
        }
        // 1. Time-box envelope check
        if overlay.autonomy_secs >= deps.cfg.autonomy_timebox_secs {
            overlay.state = OverlayState::HumanHeld;
            // jleechan-park-leaves-zombie-session-mh9o: kill the live AO
            // session and clear the durable handle BEFORE save, so the bead
            // is not stranded with a live session_id that (a) the AO dedup
            // guard still reports as [spawning] and (b) recover_human_held's
            // `session_id IS NULL` predicate cannot requeue through. Without
            // this, every autonomy_timebox_exceeded park leaks its session
            // and poisons the next redispatch of the same bead.
            kill_session_and_clear_handle(deps, &mut overlay);
            set_human_hold_reason(&mut overlay, HumanHoldReason::AutonomyTimeboxExceeded);
            deps.store.save(&overlay)?;
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "PARKED_HUMAN_HELD",
                serde_json::json!({}),
                serde_json::json!({"reason": "autonomy_timebox_exceeded"}),
            )?;
            let comment_body = "🤖 **[dark-factory]** Coder session parked (human held): autonomy time-box limit exceeded.".to_string();
            let _ = post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body);
            summary.beads_parked_human_held += 1;
            continue;
        }

        // 2. Budget warning (when autonomy time crosses 80% of the time-box)
        let warning_threshold = (deps.cfg.autonomy_timebox_secs * 80) / 100;
        if overlay.autonomy_secs >= warning_threshold
            && (overlay.autonomy_secs.saturating_sub(elapsed_secs)) < warning_threshold
        {
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                overlay.state.as_str(),
                "BUDGET_WARNING",
                serde_json::json!({
                    "autonomySecs": overlay.autonomy_secs,
                    "thresholdSecs": warning_threshold,
                }),
                serde_json::json!({"message": "Autonomy time has crossed 80% of the time-box limit"}),
            )?;
        }

        // 3. Wedge detection
        match overlay.state {
            OverlayState::Dispatched => {
                // bead jleechan-tfs1 amendment: post-hoc append-only
                // verification for adopted-branch remediation. The coder
                // session dispatched by `reroll::execute_adopted` is only
                // constrained not to force-push at the PROMPT level (it's
                // an independent subprocess the daemon doesn't control at
                // the git layer) — this is the code-level backstop. Runs
                // every tick the bead sits DISPATCHED on an adopted branch
                // (not gated on the 30-minute autonomy threshold, same as
                // the session_branch_mismatch sweep below it), so a
                // history-rewrite is caught on the very next tick rather
                // than silently surviving until promotion.
                //
                // Fail-closed by design (opposite bias from the
                // session_branch_mismatch check below, which intentionally
                // treats "cannot verify" as NOT a violation): both a
                // confirmed non-ancestor (`Ok(false)`) and an inconclusive
                // check (`Err`, e.g. a fetch failure) escalate. A missed
                // stall retries next tick for free; a missed force-push is
                // silent, permanent history loss on a branch the daemon
                // does not own.
                if overlay.is_adopted {
                    if let (Some(branch), Some(pre_sha)) =
                        (overlay.branch.clone(), overlay.pre_session_head_sha.clone())
                    {
                        let verdict = deps.vcs.remote_head_sha(&branch).and_then(|post_sha| {
                            deps.vcs
                                .is_ancestor(&pre_sha, &post_sha)
                                .map(|ok| (ok, post_sha))
                        });
                        match verdict {
                            Ok((true, _)) => {}
                            Ok((false, post_sha)) => {
                                overlay.state = OverlayState::HumanHeld;
                                // jleechan-park-leaves-zombie-session-mh9o:
                                // adopted-branch remediation parks also leak
                                // their session if we don't terminate it.
                                // Wire the same cleanup helper as the other
                                // PARKED_* sites.
                                kill_session_and_clear_handle(deps, &mut overlay);
                                set_human_hold_reason(
                                    &mut overlay,
                                    HumanHoldReason::AdoptedBranchHistoryRewriteDetected,
                                );
                                deps.store.save(&overlay)?;
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    OverlayState::HumanHeld.as_str(),
                                    "PARKED_HUMAN_HELD",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "reason": "adopted_branch_history_rewrite_detected",
                                        "branch": branch,
                                        "pre_session_sha": pre_sha,
                                        "post_session_sha": post_sha,
                                    }),
                                )?;
                                let comment_body = format!(
                                    "🤖 **[dark-factory]** Escalation required: history-rewrite \
                                     detected on adopted branch `{}`. The pre-remediation \
                                     HEAD `{}` is no longer an ancestor of the branch's \
                                     current tip (`{}`) — this means the branch was \
                                     force-pushed or rebased, which violates the append-only \
                                     guarantee for adopted PRs (bead jleechan-tfs1). Parked \
                                     HUMAN_HELD for manual review; the daemon will not touch \
                                     this branch further.",
                                    branch, pre_sha, post_sha
                                );
                                let _ = post_scm_comment_by_bead_id(
                                    deps,
                                    &overlay.bead_id,
                                    &comment_body,
                                );
                                summary.beads_parked_human_held += 1;
                                continue;
                            }
                            Err(e) => {
                                overlay.state = OverlayState::HumanHeld;
                                // jleechan-park-leaves-zombie-session-mh9o:
                                // adopted-branch append-only check failure
                                // also leaks its session. Wire the same
                                // cleanup helper as the other PARKED_*
                                // sites.
                                kill_session_and_clear_handle(deps, &mut overlay);
                                set_human_hold_reason(
                                    &mut overlay,
                                    HumanHoldReason::AdoptedBranchAppendOnlyCheckFailed,
                                );
                                deps.store.save(&overlay)?;
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    OverlayState::HumanHeld.as_str(),
                                    "PARKED_HUMAN_HELD",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "reason": "adopted_branch_append_only_check_failed",
                                        "branch": branch,
                                        "pre_session_sha": pre_sha,
                                        "error": e.to_string(),
                                    }),
                                )?;
                                let comment_body = format!(
                                    "🤖 **[dark-factory]** Escalation required: could not verify \
                                     the append-only guarantee on adopted branch `{}` \
                                     (pre-remediation HEAD `{}`): {}. Parked HUMAN_HELD \
                                     for manual review rather than assuming the branch is safe.",
                                    branch, pre_sha, e
                                );
                                let _ = post_scm_comment_by_bead_id(
                                    deps,
                                    &overlay.bead_id,
                                    &comment_body,
                                );
                                summary.beads_parked_human_held += 1;
                                continue;
                            }
                        }
                    }
                }

                // jleechan-5ia2: dispatch-integrity sweep. Runs every tick
                // (independent of the 30-minute autonomy threshold below) so
                // a corrupted DISPATCHED row can never sit trusted
                // indefinitely, however it was written. Live-reproduced:
                // bead `jleechan-vj89`'s overlay had `state=DISPATCHED` and
                // a real, alive `session_id` — but that session belonged to
                // a completely unrelated task on a different branch
                // (`wa-3004` / `feat/wa-3004-hook-refactor`, not
                // `factory/jleechan-vj89-r1`), and zero `TASK_DISPATCHED`
                // telemetry exists for the bead — no code path in this
                // crate's dispatch/reroll modules can produce that pairing
                // through a genuine `Sessions::spawn`/`attach` return value,
                // so the row was almost certainly written out-of-band
                // (bypassing both this daemon and `factory-overlay.sh`'s own
                // "no direct sqlite3 mutations" contract). `dispatch_ready`
                // now refuses to *create* such a row going forward; this
                // sweep additionally refuses to keep *trusting* one if it
                // somehow appears — parking it HUMAN_HELD rather than
                // silently treating it as a legitimate in-flight worker.
                // `Ok(None)` ("cannot verify") is intentionally NOT
                // treated as a violation — only a positively confirmed
                // mismatch (or a confirmed-dead session) parks the bead.
                if let Some(session_id_str) = overlay.session_id.clone() {
                    let session_id = SessionId(session_id_str.clone());
                    if let Ok(Some(actual_branch)) = deps.sessions.session_branch(&session_id) {
                        let expected_branch = overlay.branch.clone().unwrap_or_default();
                        if actual_branch != expected_branch {
                            overlay.state = OverlayState::HumanHeld;
                            // jleechan-park-leaves-zombie-session-mh9o:
                            // `session_branch` just proved the live session
                            // belongs to a DIFFERENT bead/branch (the
                            // `jleechan-5ia2` corruption case), so we MUST
                            // NOT call `sessions.stop()` here — that would
                            // terminate another bead's legitimate worker.
                            // The right fix is to drop OUR overlay's bad
                            // handle (the durable record pointing at a
                            // session that was never ours to own) without
                            // touching AO. The leaked overlay can then
                            // never poison a future redispatch of THIS
                            // bead via the AO dedup guard.
                            overlay.session_id = None;
                            set_human_hold_reason(
                                &mut overlay,
                                HumanHoldReason::SessionBranchMismatch,
                            );
                            deps.store.save(&overlay)?;
                            emit(
                                deps.telemetry_log,
                                &overlay.bead_id,
                                overlay.attempt,
                                OverlayState::HumanHeld.as_str(),
                                "PARKED_HUMAN_HELD",
                                serde_json::json!({}),
                                serde_json::json!({
                                    "reason": "session_branch_mismatch",
                                    "session_id": session_id_str,
                                    "expected_branch": expected_branch,
                                    "actual_branch": actual_branch,
                                }),
                            )?;
                            let comment_body = format!(
                                "🤖 **[dark-factory]** Escalation required: bead `{}` was recorded DISPATCHED with session `{}`, but that session's live branch (`{}`) does not match the bead's registered branch (`{}`). This record cannot be trusted and has been parked HUMAN_HELD for manual review (see jleechan-5ia2).",
                                overlay.bead_id, session_id_str, actual_branch, expected_branch
                            );
                            let _ =
                                post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body);
                            summary.beads_parked_human_held += 1;
                            continue;
                        }
                    }
                }

                // Adopted remediation reuses cumulative autonomy_secs from
                // adoption, which can false-positive-park a fresh session;
                // a dedicated staleness check for that path is follow-up.
                if !overlay.is_adopted {
                    if let Some(ref branch) = overlay.branch {
                        if overlay.autonomy_secs >= 1800 {
                            // jleechan-bqdv Stage C: poll the bead's OWN
                            // resolved repo (`overlay.repo(cfg)`), not
                            // `cfg.target_repo` directly. Before this fix, a
                            // bead dispatched into a non-default `[repos.*]`
                            // repo was watched against the wrong repo's
                            // branch history — the coder-silence watcher
                            // could never observe real progress and would
                            // eventually park a perfectly healthy, actively
                            // pushing coder as `coder_silent`.
                            let last_commit_epoch = deps
                                .scm
                                .remote_branch_last_commit_for_repo(overlay.repo(deps.cfg), branch)?;
                            let now_epoch = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let branch_is_silent = match last_commit_epoch {
                                None => true,
                                Some(commit_time) => now_epoch.saturating_sub(commit_time) >= 1800,
                            };

                            // Bead jleechan-coder-silent-false-parks-h92r:
                            // 2026-07-17 all 6 active lanes were parked
                            // `coder_silent` while their coders were
                            // demonstrably working — a coder can iterate
                            // locally (edit/test/edit) for well over 30
                            // minutes before its next push, so "no remote
                            // commit in 30 minutes" alone is not evidence of
                            // silence. Consult the coder's own transcript
                            // mtime as a second, independent liveness
                            // signal before parking; only park when NEITHER
                            // signal shows recent activity (fail-closed
                            // preserved: missing/unresolvable transcript
                            // evidence does not by itself save a bead from
                            // parking).
                            let transcript_epoch = deps
                                .cfg
                                .resolve_repo(overlay.repo(deps.cfg))
                                .and_then(|routing| {
                                    deps.sessions
                                        .worktree_transcript_last_activity_epoch(
                                            &routing.ao_project,
                                            branch,
                                        )
                                        .ok()
                                        .flatten()
                                });
                            let transcript_is_active = transcript_epoch
                                .is_some_and(|t| now_epoch.saturating_sub(t) < 1800);

                            if branch_is_silent && transcript_is_active {
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    overlay.state.as_str(),
                                    "CODER_ACTIVE_GRACE",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "reason": "coder_active_grace",
                                        "branch": branch,
                                        "last_commit_epoch": last_commit_epoch,
                                        "transcript_epoch": transcript_epoch,
                                    }),
                                )?;
                            } else if branch_is_silent {
                                overlay.state = OverlayState::HumanHeld;
                                // jleechan-park-leaves-zombie-session-mh9o:
                                // mirror the autonomy_timebox fix above —
                                // the wedge-detection sweep also leaks its
                                // session if we save() without first calling
                                // `ao session kill` and clearing the handle.
                                kill_session_and_clear_handle(deps, &mut overlay);
                                set_human_hold_reason(&mut overlay, HumanHoldReason::CoderSilent);
                                deps.store.save(&overlay)?;
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    OverlayState::HumanHeld.as_str(),
                                    "PARKED_HUMAN_HELD",
                                    serde_json::json!({}),
                                    serde_json::json!({"reason": "coder_silent"}),
                                )?;
                                let comment_body = "🤖 **[dark-factory]** Coder session parked (human held): coder silent/inactive on branch for 30 minutes.".to_string();
                                let _ = post_scm_comment_by_bead_id(
                                    deps,
                                    &overlay.bead_id,
                                    &comment_body,
                                );
                                summary.beads_parked_human_held += 1;
                            }
                        }
                    }
                }
            }
            OverlayState::Attested => {
                if let Some(pr_number) = overlay.pr_number {
                    // jleechan-qdw: per-bead isolation. A transient
                    // `gh`/GraphQL/network hiccup fetching THIS overlay's
                    // PR snapshot must not abort the entire tick — one
                    // bead's tool failure cannot wedge the wedge-detection
                    // loop and freeze every other active overlay. Log the
                    // error and skip to the next overlay; the wedge check
                    // re-runs on the next tick when the snapshot succeeds.
                    // jleechan-9xrs Stage D: was `deps.scm.pr_snapshot(pr_number)`
                    // — this is the SAME active-overlay loop iteration whose
                    // sibling `ci_pending_for_attested` call above already
                    // resolves `active_overlay_repo`; missing this one meant
                    // the wedge-detection / timebox-park path still read a
                    // cross-repo bead's PR from `cfg.target_repo`.
                    let pr_snapshot = match deps
                        .scm
                        .pr_snapshot_for_repo(&active_overlay_repo, pr_number)
                    {
                        Ok(snap) => snap,
                        Err(e) => {
                            let _ = emit(
                                deps.telemetry_log,
                                &overlay.bead_id,
                                overlay.attempt,
                                OverlayState::Attested.as_str(),
                                "BEAD_SNAPSHOT_TRANSIENT_ERROR",
                                serde_json::json!({}),
                                serde_json::json!({"error": format!("{e:?}"), "phase": "wedge_detection"}),
                            );
                            continue;
                        }
                    };
                    let now_epoch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    if now_epoch.saturating_sub(pr_snapshot.updated_at_epoch) >= 1800
                        && !pr_snapshot.ci_pending
                    {
                        let is_stalled_or_dead =
                            if let Some(ref session_id_str) = overlay.session_id {
                                let session_id = SessionId(session_id_str.clone());
                                deps.sessions.is_quiescent(&session_id)?
                            } else {
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "EXISTING_PR_WAITING",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "reason": "no_worker_session",
                                        "pr_number": pr_number,
                                    }),
                                )?;
                                false
                            };

                        if is_stalled_or_dead {
                            // Bead `jleechan-ubas`: do NOT park a stalled
                            // session if the remote PR head is genuinely
                            // ahead of the local branch — that's evidence
                            // the worker is still landing commits even
                            // though `is_quiescent` says otherwise (e.g. an
                            // AO session was forked, externally terminated,
                            // or the AO state lost sync with the actual PR
                            // progress). The check is `is_remote_ahead`,
                            // not raw SHA inequality: a divergent or
                            // local-only-ahead branch (the worker has
                            // unpushed commits, or local has commits the
                            // remote has never seen) would also satisfy
                            // `remote_sha != local_head` and would
                            // silently mask a real stall behind a green
                            // PR. We deliberately do NOT run `git fetch`
                            // / `git branch -f` here: a daemon tick is
                            // the wrong place to mutate the local branch.
                            // The next tick's fast tier re-runs gate
                            // assessment against the live PR state.
                            let mut commits_observed_after_exit = false;
                            if let Some(ref branch) = overlay.branch {
                                if let Ok(ahead) =
                                    deps.vcs.is_remote_ahead(branch, &pr_snapshot.head_sha)
                                {
                                    if ahead {
                                        commits_observed_after_exit = true;
                                    }
                                }
                            }

                            if commits_observed_after_exit {
                                emit(
                                    deps.telemetry_log,
                                    &overlay.bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "COMMITS_OBSERVED_AFTER_STALL",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "reason": "commits observed after session_exit",
                                        "remote_head_sha": pr_snapshot.head_sha,
                                    }),
                                )?;
                                // Stay in ATTESTED; the next tick's fast
                                // tier re-runs gate assessment against
                                // the live PR state. No state mutation,
                                // no destructive git ops.
                                continue;
                            }

                            overlay.state = OverlayState::HumanHeld;
                            // `is_quiescent` positively reported a canonical
                            // AO terminal state above. Persist the cleared
                            // handle with the recoverable hold so recovery
                            // cannot overlap a live worker.
                            overlay.session_id = None;
                            set_human_hold_reason(&mut overlay, HumanHoldReason::SessionStalled);
                            deps.store.save(&overlay)?;
                            emit(
                                deps.telemetry_log,
                                &overlay.bead_id,
                                overlay.attempt,
                                OverlayState::HumanHeld.as_str(),
                                "PARKED_HUMAN_HELD",
                                serde_json::json!({}),
                                serde_json::json!({"reason": "session_stalled"}),
                            )?;
                            let comment_body = "🤖 **[dark-factory]** Coder session parked (human held): session stalled or quiescent on open PR.".to_string();
                            let _ =
                                post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body);
                            summary.beads_parked_human_held += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if slow_tier_due {
        run_slow_tier(deps, &mut summary)?;
    }

    run_fast_tier(deps, &mut summary)?;

    emit(
        deps.telemetry_log,
        "_tick",
        0,
        "N/A",
        "TICK",
        serde_json::json!({
            "beadsCreated": summary.beads_created,
            "beadsRouted": summary.beads_routed,
            "beadsDispatched": summary.beads_dispatched,
            "gatesAssessed": summary.gates_assessed,
            "beadsReady": summary.beads_ready,
            "beadsParkedHumanHeld": summary.beads_parked_human_held,
            "beadsRecoveredFromHeld": summary.beads_recovered_from_held,
            "beadsEscalated": summary.beads_escalated,
            "beadsEscalatedLocally": summary.beads_escalated_locally,
            "beadsHeldDispositionRequired": summary.beads_held_disposition_required,
        }),
        serde_json::json!({"tick_index": tick_index, "slow_tier_due": slow_tier_due}),
    )?;

    Ok(summary)
}

/// jleechan-gib: automated HUMAN_HELD exit (Rust port of shell
/// `recover-held`). Requeues only allow-listed retry-safe `HUMAN_HELD`
/// beads below `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` whose durable overlay has
/// no session handle, increments `attempt`, and zeros `autonomy_secs`.
/// Unknown/possibly-live holds fail closed. Must run BEFORE the
/// active-overlay wedge-detection loop in `run_tick` so a freshly-QUEUED
/// bead is not immediately re-parked by the timebox/wedge checks in the
/// same tick.
fn run_recovery_step(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    let recovered = deps
        .store
        .recover_human_held(MAX_HUMAN_HELD_RECOVERY_ATTEMPT)?;
    for overlay in recovered {
        summary.beads_recovered_from_held += 1;
        emit(
            deps.telemetry_log,
            &overlay.bead_id,
            overlay.attempt,
            OverlayState::Queued.as_str(),
            "RECOVERED_FROM_HELD",
            serde_json::json!({}),
            serde_json::json!({
                "prior_state": OverlayState::HumanHeld.as_str(),
                "pr_number": overlay.pr_number,
                "branch": overlay.branch,
            }),
        )?;
    }
    for overlay in deps
        .store
        .human_held_at_or_above_attempt(MAX_HUMAN_HELD_RECOVERY_ATTEMPT)?
    {
        if escalation_already_recorded(deps, &overlay.bead_id)? {
            continue;
        }
        let comment_body = format!(
            "🤖 **[dark-factory]** Escalation required: bead `{}` is HUMAN_HELD at attempt {} (max automated recovery attempts: {}). Automation will not silently requeue it again.",
            overlay.bead_id, overlay.attempt, MAX_HUMAN_HELD_RECOVERY_ATTEMPT
        );
        if let Err(err) = post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body) {
            if is_missing_scm_target_error(&err) {
                record_local_escalation_fallback(
                    deps,
                    &overlay.bead_id,
                    "human_held_recovery_attempt_cap_reached",
                )?;
                summary.beads_escalated_locally += 1;
                emit(
                    deps.telemetry_log,
                    &overlay.bead_id,
                    overlay.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATED_LOCALLY",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "human_held_recovery_attempt_cap_reached",
                        "max_attempt": MAX_HUMAN_HELD_RECOVERY_ATTEMPT,
                        "pr_number": overlay.pr_number,
                        "branch": overlay.branch,
                        "scm_error": err.to_string(),
                    }),
                )?;
                continue;
            }
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "ESCALATION_NOTIFICATION_FAILED",
                serde_json::json!({}),
                serde_json::json!({
                    "reason": "human_held_recovery_attempt_cap_reached",
                    "error": err.to_string(),
                }),
            )?;
            continue;
        }
        record_escalation(
            deps,
            &overlay.bead_id,
            "human_held_recovery_attempt_cap_reached",
        )?;
        summary.beads_escalated += 1;
        emit(
            deps.telemetry_log,
            &overlay.bead_id,
            overlay.attempt,
            OverlayState::HumanHeld.as_str(),
            "ESCALATION_REQUIRED",
            serde_json::json!({}),
            serde_json::json!({
                "reason": "human_held_recovery_attempt_cap_reached",
                "max_attempt": MAX_HUMAN_HELD_RECOVERY_ATTEMPT,
                "pr_number": overlay.pr_number,
                "branch": overlay.branch,
            }),
        )?;
    }
    Ok(())
}

/// Slow tier: intake new beads, route each freshly-queued bead, dispatch as
/// many QUEUED beads as the safety envelope (30/15) allows.
fn run_slow_tier(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    // jleechan-gib: recovery has already run at the top of this tick via
    // `run_recovery_step` (it must run BEFORE the active-overlay wedge loop
    // so freshly-QUEUED beads aren't immediately re-parked by wedge
    // detection). The freshly-recovered QUEUED beads are picked up by the
    // routing_candidates loop below, so they get dispatched this same tick.
    let mut pr_intake_bead_ids = HashSet::new();
    let (pr_adoptions, pr_skip_outcomes) =
        intake::normalize_labeled_prs(deps.scm, deps.tracker, deps.cfg)?;
    // jleechan-eazj: every factory-labeled PR that did NOT result in an
    // adoption still gets exactly one verdict event here, unconditionally —
    // before the adoption loop below runs any I/O of its own.
    for outcome in &pr_skip_outcomes {
        emit_intake_outcome(deps.telemetry_log, outcome)?;
    }
    for adopted in pr_adoptions {
        pr_intake_bead_ids.insert(adopted.bead_id.clone());
        if adopted.newly_created {
            summary.beads_created += 1;
        }
        if let Some(owner) = deps.store.bead_id_for_branch(&adopted.head_ref_name)? {
            if owner != adopted.bead_id {
                let owner_live = deps.store.load(&owner)?.is_some();
                let comment_body = format!(
                    "🤖 **[dark-factory]** Escalation required: refusing factory PR adoption for branch `{}` because it is already registered to bead `{}`. Branch-key stealing is not allowed; please use a unique same-repo branch.",
                    adopted.head_ref_name, owner
                );
                let _ = deps
                    .tracker
                    .comment_external(&adopted.external_ref, &comment_body);
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &adopted.bead_id,
                    1,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "adoption_branch_collision",
                        "branch": adopted.head_ref_name,
                        "registered_bead": owner,
                        "registered_bead_live": owner_live,
                        "external_ref": adopted.external_ref,
                    }),
                )?;
                continue;
            }
        }
        deps.store
            .register_branch(&adopted.bead_id, &adopted.head_ref_name)?;

        let existing = deps.store.load(&adopted.bead_id)?;
        let attempt = existing.as_ref().map(|o| o.attempt).unwrap_or(1);
        let should_adopt = !matches!(
            existing.as_ref().map(|o| o.state),
            Some(OverlayState::Ready) | Some(OverlayState::HumanHeld)
        );
        if should_adopt {
            // jleechan-35y4 Stage A: adopted PRs are always same-repo
            // (fork/cross-repo PRs are rejected earlier by `same_repo_pr`
            // in intake.rs), so this always resolves to `cfg.target_repo`'s
            // owner/repo today. Still resolved from `external_ref` (not
            // left `None`) so it stays correct once Stage C/D lift the
            // same-repo-only restriction for adopted PRs.
            let target_repo =
                intake::resolve_target_repo("", Some(adopted.external_ref.as_str()));
            let mut overlay = existing.unwrap_or(BeadOverlay {
                bead_id: adopted.bead_id.clone(),
                state: OverlayState::Attested,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(adopted.pr_number),
                branch: Some(adopted.head_ref_name.clone()),
                session_id: None,
                is_adopted: true,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo,
            });
            overlay.state = OverlayState::Attested;
            overlay.pr_number = Some(adopted.pr_number);
            overlay.branch = Some(adopted.head_ref_name.clone());
            // Explicit stored provenance flag (bead jleechan-tfs1), NOT a
            // branch-name pattern match: every bead that reaches this block
            // arrived via `intake::normalize_labeled_prs` adopting an
            // external contributor's own head_ref_name, so it is always
            // adopted — including on a re-adopt of a pre-migration row that
            // predates this field. `reroll()` reads this flag to choose
            // append-only remediation instead of fabricating a replacement
            // branch and closing the contributor's PR.
            overlay.is_adopted = true;
            deps.store.save(&overlay)?;
        }
        emit(
            deps.telemetry_log,
            &adopted.bead_id,
            attempt,
            OverlayState::Attested.as_str(),
            "EXISTING_PR_ADOPTED",
            serde_json::json!({}),
            serde_json::json!({
                "pr_number": adopted.pr_number,
                "branch": adopted.head_ref_name,
                "external_ref": adopted.external_ref,
                "newly_created": adopted.newly_created,
            }),
        )?;
    }

    let (created, issue_skip_outcomes) = intake::normalize(deps.scm, deps.tracker, deps.cfg)?;
    // jleechan-eazj: same unconditional per-candidate guarantee as the PR
    // path above — every factory-labeled issue that did NOT result in a
    // newly-created bead still gets exactly one verdict event.
    for outcome in &issue_skip_outcomes {
        emit_intake_outcome(deps.telemetry_log, outcome)?;
    }
    let tracker_candidates = deps.tracker.fetch_candidates()?;
    let mut routing_candidates: Vec<Bead> = Vec::new();
    for bead_id in &created {
        let mut pr_number = None;
        let tracker_bead = tracker_candidates
            .iter()
            .find(|bead| bead.id == *bead_id)
            .cloned();
        // jleechan-35y4 Stage A: resolve per-bead repo identity at intake
        // time — explicit `target_repo:` body field wins, else the
        // `owner/repo` prefix of external_ref, else None (legacy/global,
        // resolved later via `BeadOverlay::repo`). Computed BEFORE the
        // PR-existence probe below (jleechan-x8tf) so that probe can target
        // the bead's OWN resolved repo instead of unconditionally
        // `cfg.target_repo`.
        let target_repo = intake::resolve_target_repo(
            tracker_bead.as_ref().map(|b| b.description.as_str()).unwrap_or(""),
            tracker_bead.as_ref().and_then(|b| b.external_ref.as_deref()),
        );

        if deps.llm.is_real() {
            if let Some(bead) = tracker_bead.as_ref() {
                if let Some(ref ext_ref) = bead.external_ref {
                    if let Some((_, num_str)) = parse_external_ref(ext_ref) {
                        if let Ok(num) = num_str.parse::<u64>() {
                            // jleechan-x8tf: probe the bead's OWN resolved
                            // repo (`target_repo`, computed above), not
                            // unconditionally `deps.cfg.target_repo` — this
                            // used to parse a repo out of `ext_ref` via
                            // `parse_external_ref` and then discard it
                            // (`_`), silently falling back to the global
                            // config repo. For any bead whose external_ref
                            // or `target_repo:` body field names a repo
                            // OTHER than `cfg.target_repo` (e.g. a
                            // dark-factory fixture bead while the daemon's
                            // global default is worldarchitect.ai), this
                            // probe silently checked the WRONG repo's PR
                            // list — corrupting any multi-repo E2E proof
                            // that depends on this check landing on the
                            // bead's own repo.
                            let probe_repo =
                                target_repo.as_deref().unwrap_or(&deps.cfg.target_repo);
                            if crate::tools::run_tool(
                                "gh",
                                &[
                                    "pr",
                                    "view",
                                    &num.to_string(),
                                    "--repo",
                                    probe_repo,
                                    "--json",
                                    "number",
                                ],
                                10,
                            )
                            .is_ok()
                            {
                                pr_number = Some(num);
                            }
                        }
                    }
                }
            }
        }
        let overlay = BeadOverlay {
            bead_id: bead_id.clone(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number,
            branch: None,
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo,
        };
        deps.store.save(&overlay)?;
        summary.beads_created += 1;
        emit(
            deps.telemetry_log,
            bead_id,
            1,
            OverlayState::Queued.as_str(),
            "INTAKE_BEAD_CREATED",
            serde_json::json!({}),
            // jleechan-eazj: carry external_ref so `grep <issue-number>
            // daemon.jsonl` finds the ADOPTED event too, not just the four
            // SKIPPED_*/ERRORED verdicts (which are keyed on external_ref
            // via bead_id since no bead exists yet for those).
            serde_json::json!({
                "external_ref": tracker_bead.as_ref().and_then(|b| b.external_ref.clone()),
            }),
        )?;
        // `Tracker::fetch_candidates` == `br list ...`; a real `br` shows this
        // bead on the very next call since `br` is durable. Prefer that real
        // bead payload so the worker prompt carries the tracker title rather than
        // an empty just-created stub; keep a non-empty fallback for static fakes.
        routing_candidates.push(tracker_bead.unwrap_or(Bead {
            id: bead_id.clone(),
            title: bead_id.clone(),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        }));
    }

    // Also pick up any bead left over from a prior tick that reached QUEUED
    // but was never routed/dispatched (process restart resilience) — real
    // `Tracker::fetch_candidates` reflects prior `create_bead` calls, so this
    // covers that path in production even though the static test fake can't.
    for bead in tracker_candidates {
        if pr_intake_bead_ids.contains(&bead.id) {
            continue;
        }
        if !routing_candidates.iter().any(|b| b.id == bead.id) {
            routing_candidates.push(bead);
        }
    }

    let mut ready: Vec<(Bead, RoutingVerdict, dispatch::DriveBranchDecision)> = Vec::new();
    for bead in &routing_candidates {
        let overlay = match deps.store.load(&bead.id)? {
            Some(o) => {
                if o.state == OverlayState::Queued || o.state == OverlayState::Redispatched {
                    o
                } else {
                    continue;
                }
            }
            None => {
                // jleechan-35y4 Stage A: same intake resolution precedence
                // as the GH-issue path above, applied identically to
                // manual `br`-created beads (spec requirement: "Manual `br`
                // beads: same body-field parse").
                let target_repo = intake::resolve_target_repo(
                    bead.description.as_str(),
                    bead.external_ref.as_deref(),
                );
                let o = BeadOverlay {
                    bead_id: bead.id.clone(),
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
            target_repo,
                };
                deps.store.save(&o)?;
                summary.beads_created += 1;
                emit(
                    deps.telemetry_log,
                    &bead.id,
                    1,
                    OverlayState::Queued.as_str(),
                    "INTAKE_BEAD_CREATED",
                    serde_json::json!({}),
                    serde_json::json!({"manual": true, "external_ref": bead.external_ref}),
                )?;
                o
            }
        };

        match router::route(deps.llm, bead) {
            Ok(verdict) => {
                summary.beads_routed += 1;
                let verdict_str = match verdict {
                    RoutingVerdict::SmallPath => "SMALL_PATH",
                    RoutingVerdict::StandardPath => "STANDARD_PATH",
                    RoutingVerdict::ResearchPath => "RESEARCH_PATH",
                    RoutingVerdict::GenericPath => "GENERIC_PATH",
                };
                emit(
                    deps.telemetry_log,
                    &bead.id,
                    overlay.attempt,
                    OverlayState::Queued.as_str(),
                    "TASK_ROUTED",
                    serde_json::json!({}),
                    // jleechan-35y4: target_repo now visible in daemon.jsonl
                    // (null == legacy/global cfg.target_repo).
                    serde_json::json!({"routingVerdict": verdict_str, "target_repo": overlay.target_repo}),
                )?;
                // jleechan-drive-pr-branch-binding-pcpr: resolved here (not
                // in `dispatch.rs`, which intentionally has no `Scm`
                // access) so a bead whose `external_ref` names a currently
                // OPEN PR in its OWN resolved repo dispatches onto that
                // PR's head branch instead of a freshly fabricated one.
                // Recomputed on every tick this bead is `ready` (fresh
                // dispatch AND every redispatch/park-recovery cycle) — the
                // live 2026-07-17 incident this closes happened on a
                // redispatch, not just the first attempt.
                let resolved_repo = overlay.repo(deps.cfg).to_string();
                let drive_branch =
                    resolve_drive_pr_head_branch(deps.scm, deps.cfg, bead, &resolved_repo);
                ready.push((bead.clone(), verdict, drive_branch));
            }
            Err(DaemonError::Parse(reason)) => {
                // ZFC: an unparseable routing verdict is never guessed at —
                // park the bead HUMAN_HELD per the same "unknown is not a
                // silent default" discipline router.rs already enforces.
                let mut held = overlay;
                held.state = OverlayState::HumanHeld;
                set_human_hold_reason(
                    &mut held,
                    HumanHoldReason::RouterParse(reason.clone()),
                );
                deps.store.save(&held)?;
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &bead.id,
                    held.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({"reason": reason}),
                )?;
                let comment_body = format!(
                    "🤖 **[dark-factory]** Router parse error (human held): {}",
                    reason
                );
                if let Some(ref ext_ref) = bead.external_ref {
                    let _ = deps.tracker.comment_external(ext_ref, &comment_body);
                }
            }
            Err(other) => return Err(other),
        }
    }

    if !ready.is_empty() {
        let dispatch_report =
            dispatch::dispatch_ready(deps.sessions, deps.store, deps.cfg, &ready)?;
        summary.beads_dispatched += dispatch_report.success_count();

        for failure in &dispatch_report.failures {
            if failure.phase == "spawn_retry_cap_exceeded" {
                // `dispatch::dispatch_ready` already parked this bead
                // HUMAN_HELD on disk (it has no `Tracker`/`Scm` access to
                // post a comment itself — see its module doc comment).
                // Record the state-transition fact unconditionally, then
                // best-effort escalate exactly once (mirrors
                // `run_recovery_step`'s cap-escalation idiom below).
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "transient_spawn_retry_cap_exceeded",
                        "branch": failure.branch.as_deref(),
                        "error": failure.error.as_str(),
                        "max_transient_spawn_retry": MAX_TRANSIENT_SPAWN_RETRY,
                    }),
                )?;
                if escalation_already_recorded(deps, &failure.bead_id)? {
                    continue;
                }
                let comment_body = format!(
                    "🤖 **[dark-factory]** Escalation required: bead `{}` failed to spawn a worker session more than {} consecutive times (transient errors only — e.g. AO session-cap pressure). Automation parked it HUMAN_HELD instead of retrying indefinitely; please check target-repo session capacity before requeuing.",
                    failure.bead_id, MAX_TRANSIENT_SPAWN_RETRY
                );
                if let Err(err) = post_scm_comment_by_bead_id(deps, &failure.bead_id, &comment_body)
                {
                    if is_missing_scm_target_error(&err) {
                        record_local_escalation_fallback(
                            deps,
                            &failure.bead_id,
                            "transient_spawn_retry_cap_exceeded",
                        )?;
                        summary.beads_escalated_locally += 1;
                        emit(
                            deps.telemetry_log,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "ESCALATED_LOCALLY",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "transient_spawn_retry_cap_exceeded",
                                "max_transient_spawn_retry": MAX_TRANSIENT_SPAWN_RETRY,
                                "branch": failure.branch.as_deref(),
                                "scm_error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "transient_spawn_retry_cap_exceeded",
                            "error": err.to_string(),
                        }),
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "transient_spawn_retry_cap_exceeded")?;
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "transient_spawn_retry_cap_exceeded",
                        "max_transient_spawn_retry": MAX_TRANSIENT_SPAWN_RETRY,
                        "branch": failure.branch.as_deref(),
                    }),
                )?;
                continue;
            }

            if failure.phase == "unmapped_repo" {
                // jleechan-8jxr r3 (review follow-up, chatgpt-codex-connector
                // P2 @ daemon/src/dispatch.rs:287): mirror the
                // `unmapped_target_repo` idiom below. `dispatch_ready`
                // already parked the bead HUMAN_HELD with reason
                // `unmapped_repo` (jleechan-8jxr r2) — distinct from
                // `unmapped_target_repo` ("I resolved a repo and it's not
                // in [repos]") so operators can tell which remediation
                // applies: add a `[repos.*]` entry vs. add a
                // `target_repo:`/`external_ref` field on the bead body or
                // label the source issue `factory` so intake can resolve
                // it. Without this branch, the fall-through at the bottom
                // of this loop labels a genuinely permanent,
                // operator-action-required park as retryable, emits
                // `BEAD_DISPATCH_TRANSIENT_ERROR` (not `PARKED_HUMAN_HELD`),
                // never increments `summary.beads_parked_human_held`, and
                // posts no escalation comment — exactly the anti-pattern
                // the `unmapped_target_repo` and `worktree_remote_mismatch`
                // branches were added to fix.
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "unmapped_repo",
                        "error": failure.error.as_str(),
                    }),
                )?;
                if escalation_already_recorded(deps, &failure.bead_id)? {
                    continue;
                }
                let comment_body = format!(
                    "🤖 **[dark-factory]** Escalation required: bead `{}` had no resolvable repo identity at dispatch time (no `target_repo:` body field, no `external_ref` with a parseable `owner/repo#N` prefix, no adopted-PR context, and no other intake-side repo signal). Automation parked it HUMAN_HELD rather than silently defaulting to the daemon's global `target_repo` (which would have routed it to a wrong repo — confirmed 5x on 2026-07-18: yvfe/vmy2/46dk/s9ba/txtd). Operator action: supply an explicit `target_repo: <owner>/<repo>` line in the bead body, set `external_ref = \"<owner>/<repo>#NNN\"`, or file under an issue/PR labeled `factory` so intake can resolve the repo from the GitHub external_ref.",
                    failure.bead_id
                );
                if let Err(err) = post_scm_comment_by_bead_id(deps, &failure.bead_id, &comment_body)
                {
                    if is_missing_scm_target_error(&err) {
                        record_local_escalation_fallback(
                            deps,
                            &failure.bead_id,
                            "unmapped_repo",
                        )?;
                        summary.beads_escalated_locally += 1;
                        emit(
                            deps.telemetry_log,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "ESCALATED_LOCALLY",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "unmapped_repo",
                                "scm_error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "unmapped_repo",
                            "error": err.to_string(),
                        }),
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "unmapped_repo")?;
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "unmapped_repo"}),
                )?;
                continue;
            }

            if failure.phase == "unmapped_target_repo" {
                // jleechan-35y4 (adversarial review of PR #245): this phase
                // (from `dispatch::dispatch_ready`'s fail-loud park) was
                // previously falling through to the generic
                // `BEAD_DISPATCH_TRANSIENT_ERROR` branch below, which
                // labeled a genuinely HUMAN_HELD, non-transient park as
                // `lifecycle_state = QUEUED` / `"transient": false`-but-
                // treated-as-retryable telemetry, incremented no
                // HUMAN_HELD counter, and posted no escalation comment —
                // the exact opposite of the "fail loud" intent. Mirror the
                // `spawn_retry_cap_exceeded` idiom: record the park, then
                // best-effort escalate exactly once.
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "unmapped_target_repo",
                        "error": failure.error.as_str(),
                    }),
                )?;
                if escalation_already_recorded(deps, &failure.bead_id)? {
                    continue;
                }
                let comment_body = format!(
                    "🤖 **[dark-factory]** Escalation required: bead `{}` claims a `target_repo` with no matching `[repos.*]` config entry (and it is not the daemon's global `target_repo`). Automation parked it HUMAN_HELD rather than guessing which repo/AO-project to dispatch into; please add a `[repos.\"<repo>\"]` entry to `config/daemon.toml` (or correct the bead's `target_repo`) before requeuing.",
                    failure.bead_id
                );
                if let Err(err) = post_scm_comment_by_bead_id(deps, &failure.bead_id, &comment_body)
                {
                    if is_missing_scm_target_error(&err) {
                        record_local_escalation_fallback(
                            deps,
                            &failure.bead_id,
                            "unmapped_target_repo",
                        )?;
                        summary.beads_escalated_locally += 1;
                        emit(
                            deps.telemetry_log,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "ESCALATED_LOCALLY",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "unmapped_target_repo",
                                "scm_error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "unmapped_target_repo",
                            "error": err.to_string(),
                        }),
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "unmapped_target_repo")?;
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "unmapped_target_repo"}),
                )?;
                continue;
            }

            if failure.phase == "worktree_remote_mismatch" {
                // jleechan-bqdv Stage C: mirrors the `unmapped_target_repo`
                // idiom immediately above. `dispatch::dispatch_ready` already
                // parked this bead HUMAN_HELD (and killed the mismatched
                // session) on disk before returning this failure — it has no
                // `Tracker`/`Scm` access to post a comment itself (same
                // module-boundary reason as every other dispatch.rs park).
                // Record the state-transition fact unconditionally, then
                // best-effort escalate exactly once so this NEVER falls
                // through to the generic `BEAD_DISPATCH_TRANSIENT_ERROR`
                // branch below (which would misreport a genuinely
                // HUMAN_HELD, non-transient park as retryable and post no
                // escalation comment — the exact anti-pattern jleechan-35y4's
                // review fixed for `unmapped_target_repo`).
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "worktree_remote_mismatch",
                        "branch": failure.branch.as_deref(),
                        "error": failure.error.as_str(),
                    }),
                )?;
                if escalation_already_recorded(deps, &failure.bead_id)? {
                    continue;
                }
                let comment_body = format!(
                    "🤖 **[dark-factory]** Escalation required: bead `{}`'s spawned worktree had a git remote that does NOT match the bead's resolved repo. Automation killed the session and parked it HUMAN_HELD rather than risk the coder pushing to the wrong repo (jleechan-9sh5 / jleechan-bqdv); please verify the target AO project's local checkout/remotes before requeuing. Details: {}",
                    failure.bead_id,
                    failure.error.as_str()
                );
                if let Err(err) = post_scm_comment_by_bead_id(deps, &failure.bead_id, &comment_body)
                {
                    if is_missing_scm_target_error(&err) {
                        record_local_escalation_fallback(
                            deps,
                            &failure.bead_id,
                            "worktree_remote_mismatch",
                        )?;
                        summary.beads_escalated_locally += 1;
                        emit(
                            deps.telemetry_log,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "ESCALATED_LOCALLY",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "worktree_remote_mismatch",
                                "scm_error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "worktree_remote_mismatch",
                            "error": err.to_string(),
                        }),
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "worktree_remote_mismatch")?;
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &failure.bead_id,
                    failure.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "worktree_remote_mismatch"}),
                )?;
                continue;
            }

            let lifecycle_state = if failure.branch.is_some() {
                OverlayState::Dispatching.as_str()
            } else {
                OverlayState::Queued.as_str()
            };
            emit(
                deps.telemetry_log,
                &failure.bead_id,
                failure.attempt,
                lifecycle_state,
                "BEAD_DISPATCH_TRANSIENT_ERROR",
                serde_json::json!({}),
                serde_json::json!({
                    "phase": failure.phase,
                    "branch": failure.branch.as_deref(),
                    "error": failure.error.as_str(),
                    "transient": failure.transient,
                }),
            )?;
        }

        for success in &dispatch_report.successes {
            emit(
                deps.telemetry_log,
                &success.bead_id,
                success.attempt,
                OverlayState::Dispatched.as_str(),
                "TASK_DISPATCHED",
                serde_json::json!({}),
                serde_json::json!({
                    "branch": success.branch.as_str(),
                    "sessionId": success.session_id.as_str(),
                    // jleechan-35y4: resolved repo now visible in daemon.jsonl.
                    "target_repo": success.target_repo.as_str(),
                    // jleechan-drive-pr-branch-binding-pcpr: "pr_head" vs
                    // "generated" — which branch-binding mode this dispatch used.
                    "branch_mode": success.branch_mode,
                }),
            )?;
            let comment_body = format!(
                "🤖 **[dark-factory]** Spawned worker session in slot for bead `{}` (attempt {}). Branch: `{}`.",
                success.bead_id, success.attempt, success.branch
            );
            if let Some(ext_ref) = ready
                .iter()
                .find(|(bead, _, _)| bead.id == success.bead_id)
                .and_then(|(bead, _, _)| bead.external_ref.as_ref())
            {
                let _ = deps.tracker.comment_external(ext_ref, &comment_body);
            }
        }
    }

    Ok(())
}

/// Build gate 6/7's `PrEvidence` for one PR. Gate 7 (Skeptic) is Stage 1's
/// only LLM call in the fast tier (spec §4.2.5 item 7, verifier.rs module doc:
/// "Stage 1 Skeptic consumes ... an `Llm` adversarial call"): render a minimal
/// review prompt, call `Llm::judge`, and strictly parse the reply through
/// `verifier::parse_skeptic_verdict`'s fixed `pass|warn|fail` grammar — an
/// unparseable reply becomes `None` (gate 7 -> `Unknown`, never a guessed
/// `Green`/`Red`), matching the ZFC discipline `router.rs` and `verifier.rs`
/// already enforce elsewhere in this crate.
///
/// The evidence floor (gate 6, non-test changed LOC) has no wired data source
/// yet in Stage 1 (no `Vcs`/`Scm` method returns a diff LOC count in the
/// traits this task is scoped to) — `non_test_changed_loc` defaults to `0`,
/// which is honestly "floor not exceeded" rather than a guessed pass; wiring a
/// real LOC count is a follow-up, not silently faked here. Likewise Stage 1
/// has no wired `/er` runner yet, so `er_verdict` is honestly `Absent` (gate 6
/// -> `Unknown`, never a guessed `Pass`) until a real `/er` invocation is
/// wired in (bead jleechan-3rf, verifier.rs `evidence_floor_gate`).
/// Wall-clock timeout for one gate-7 reviewer subprocess, every vendor.
///
/// jleechan-hhmb: 120s (codex/claude/agy) killed every GENUINE review — a
/// real end-to-end skeptic pass over a 50-file PR does `gh`-backed
/// investigation (diff, checks, comments) before answering. Live-measured
/// 2026-07-10: the exact claude invocation took 2m27s on
/// worldarchitect.ai#7888 and was SIGTERM'd by the old limit every cycle,
/// which — with codex quota-exhausted, agy returning empty stdout, and
/// gemini UNSUPPORTED_CLIENT (jleechan-yige) — made "both reviewers failed
/// to produce a parseable verdict" a permanent state. 300s ≈ 2× the
/// measured duration, same headroom philosophy as gemini's earlier
/// 110s→150s sizing (PR#216).
const REVIEWER_TIMEOUT_SECS: u64 = 300;

/// Dispatch one independent reviewer subprocess by vendor name. Extracted
/// from `skeptic_evidence` so two vendors can be dispatched in parallel
/// threads (PR#163 finding 2) without duplicating the per-vendor argv
/// construction.
fn dispatch_reviewer(vendor: &str, prompt: &str) -> Result<String, DaemonError> {
    use crate::tools::run_tool;
    match vendor {
        "codex" => run_tool(
            "codex",
            &["exec", "--yolo", "--skip-git-repo-check", prompt],
            REVIEWER_TIMEOUT_SECS,
        ),
        "claude" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let nvm_claude = format!("{}/.nvm/versions/node/v22.22.0/bin/claude", home);
            let claude_bin = if std::path::Path::new(&nvm_claude).exists() {
                nvm_claude
            } else {
                "claude".to_string()
            };
            run_tool(
                &claude_bin,
                &[
                    "--print",
                    "--dangerously-skip-permissions",
                    "--setting-sources",
                    "",
                    prompt,
                ],
                REVIEWER_TIMEOUT_SECS,
            )
        }
        "agy" => run_tool(
            "agy",
            &["--print", "--dangerously-skip-permissions", prompt],
            REVIEWER_TIMEOUT_SECS,
        ),
        // jleechan-bkru: 4th reviewer vendor, added after a live 2026-07-09
        // incident where codex+claude+agy were ALL simultaneously
        // non-functional (codex quota-exhausted multi-day, claude weekly
        // limit hit multi-day, agy quota-exhausted + a separate
        // session-continuity bug). `gemini` is Google's Gemini CLI
        // (`@google/gemini-cli`), a distinct product/account/quota from
        // `agy` (Antigravity). `--yolo` auto-approves tool calls (gemini's
        // equivalent of `--dangerously-skip-permissions`); `--skip-trust`
        // is required in headless/non-interactive contexts or the CLI
        // refuses to run with a "not a trusted directory" error.
        "gemini" => run_tool(
            "gemini",
            &["-p", prompt, "--yolo", "--skip-trust"],
            REVIEWER_TIMEOUT_SECS,
        ),
        other => Err(DaemonError::Tool {
            tool: other.to_string(),
            rc: -1,
            stderr: "unknown reviewer vendor".to_string(),
        }),
    }
}

/// Render a `SkepticVerdict` back into the bare `pass|warn <note>|fail
/// <reason>` grammar `parse_skeptic_verdict` accepts, so the dual-reviewer
/// combined verdict can be folded into the `subsystem: gate-7` line
/// alongside the (optional) `gha`/`sign-off` subsystems.
fn skeptic_verdict_to_line(v: &verifier::SkepticVerdict) -> String {
    match v {
        verifier::SkepticVerdict::Pass => "pass".to_string(),
        verifier::SkepticVerdict::Warn(note) => format!("warn {note}"),
        verifier::SkepticVerdict::Fail(reason) => format!("fail {reason}"),
    }
}

fn skeptic_evidence(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    repo: &str,
    snapshot: &crate::tools::PrSnapshot,
) -> Result<PrEvidence, DaemonError> {
    // jleechan-9xrs Stage D: the reviewer subprocess (`dispatch_reviewer`)
    // runs `codex exec` / `claude --print` with no cwd override, so `gh`
    // commands the reviewer issues without an explicit `--repo` default to
    // whatever repo the daemon process's own cwd happens to be checked out
    // as. Embedding `repo` (the bead's OWN resolved repo, `overlay.repo(cfg)`
    // at the call site) plus explicit `--repo` flags — mirroring
    // `er_runner::build_er_prompt` — makes the reviewer query the RIGHT repo
    // regardless of daemon cwd, instead of silently reviewing PR #{pr} in
    // whatever repo happened to be checked out.
    //
    // Task 3 (reviewer-outage-resilience): tell the skeptic reviewer which
    // review providers are currently in outage so its completeness check can
    // account for the missing signals (their gates have been waived). Only
    // append the note when at least one vendor is in outage; the healthy path
    // keeps the prompt byte-identical to before.
    let outage_note = {
        let mut waived: Vec<&str> = Vec::new();
        if deps
            .store
            .vendor_health("coderabbit")
            .ok()
            .flatten()
            .is_some_and(|h| h.in_outage)
        {
            waived.push("coderabbit");
        }
        if deps
            .store
            .vendor_health("bugbot")
            .ok()
            .flatten()
            .is_some_and(|h| h.in_outage)
        {
            waived.push("bugbot");
        }
        if waived.is_empty() {
            String::new()
        } else {
            format!(
                "\nNote: the following review providers are currently in \
                 outage and their gates have been waived: {}. Account for \
                 this in your completeness check.",
                waived.join(", ")
            )
        }
    };
    let prompt = format!(
        "You are the Stage-1 Skeptic gate for an autonomous coding factory.\n\
         Review bead {bead_id}'s PR #{pr} in repo {repo} end-to-end (diff, \
         evidence, tests) and judge whether it is ready to merge:\n\
           gh pr diff {pr} --repo {repo}\n\
           gh pr view {pr} --repo {repo} --json body,comments\n\
           gh pr checks {pr} --repo {repo}\n\
         Respond with exactly one line of the form:\n\
         pass|warn <note>|fail <reason>{outage_note}",
    );

    let coder_agent = std::env::var("DARK_FACTORY_CODER_DEFAULT")
        .or_else(|_| std::env::var("DARK_FACTORY_REVIEWER_DEFAULT"))
        .unwrap_or_else(|_| "minimax".to_string());

    let coder_vendor = match coder_agent.to_ascii_lowercase().as_str() {
        a if a.contains("claude") => "claude",
        a if a.contains("minimax") => "minimax",
        a if a.contains("agy") => "agy",
        a if a.contains("codex") => "codex",
        a if a.contains("gemini") => "gemini",
        _ => "",
    };

    // jleechan-bkru: 4th reviewer vendor (see `dispatch_reviewer`'s
    // "gemini" arm). Appended at the end so the existing codex/claude/agy
    // ordering is unchanged; it is reached only via the fallback loop
    // below when earlier vendors fail to produce a parseable verdict, not
    // reordered ahead of them — codex/claude/agy's current outage is a
    // point-in-time incident (quotas reset), not a permanent property of
    // those vendors.
    let mut priority = vec!["codex", "claude", "agy", "gemini"];
    if !coder_vendor.is_empty() {
        priority.retain(|&v| v != coder_vendor);
    }

    // jleechan-9xrs Stage D: was `deps.cfg.target_repo` — must be the
    // bead's OWN resolved repo so a test-repo bead dispatched under a
    // non-test global `cfg.target_repo` (or vice versa) is classified
    // correctly instead of by the daemon-global repo.
    let is_test_repo =
        repo.contains("fake-") || repo.contains("test-") || repo == "owner/repo";

    let mut gha_verdict = "verdict: absent";
    let mut signoff_verdict = "verdict: absent";

    for comment in &snapshot.comments {
        let body_lower = comment.body.to_ascii_lowercase();
        let author_lower = comment.author.to_ascii_lowercase();

        if (author_lower.contains("github-actions") || author_lower.contains("gha"))
            && body_lower.contains("skeptic")
        {
            if body_lower.contains("verdict: pass") || body_lower.contains("verdict: success") {
                gha_verdict = "verdict: pass";
            } else if body_lower.contains("verdict: fail")
                || body_lower.contains("verdict: failure")
            {
                gha_verdict = "verdict: fail";
            }
        }

        if !author_lower.contains("github-actions")
            && !author_lower.contains("coderabbit")
            && !author_lower.contains("bugbot")
            && !author_lower.contains("cursor")
        {
            if body_lower.contains("sign-off")
                || body_lower.contains("signoff")
                || body_lower.contains("verdict: pass")
                || body_lower.contains("/skeptic pass")
            {
                signoff_verdict = "verdict: pass";
            } else if body_lower.contains("verdict: fail") || body_lower.contains("/skeptic fail") {
                signoff_verdict = "verdict: fail";
            }
        }
    }

    // jleechan-wzgl: track which reviewer vendor(s) actually contributed a
    // parseable verdict to `skeptic_verdict`, so GATE_ASSESSMENT telemetry
    // can report gate-7 provenance (confirming the reviewer was non-self
    // and genuinely ran, not self-certified) instead of leaving it
    // unrecoverable. `"mock_llm"` marks the `is_test_repo` path explicitly
    // as a mock, not a real independent vendor.
    let mut used_vendors: Vec<String> = Vec::new();

    let skeptic_verdict = if is_test_repo {
        let reply = deps.llm.judge(&prompt)?;
        let verdict = verifier::parse_skeptic_verdict(&reply);
        if verdict.is_some() {
            used_vendors.push("mock_llm".to_string());
        }
        verdict
    } else {
        // PR#163 finding 2: dispatch the first TWO vendors in the
        // coder-exclusion-filtered priority list as INDEPENDENT parallel
        // reviewers — never the vendor that authored the code under review
        // (self-review would defeat the adversarial guarantee) — and
        // combine via `combine_dual_verdict`. This restores main's
        // pre-rebase dual-reviewer safety net: a single reviewer-tool
        // outage can never false-park a bead.
        //
        // jleechan-baaf: a TOTAL outage of vendor1+vendor2 (both
        // unparseable/errored) no longer immediately propagates `Err`. If
        // `priority` has a third, still-untried member (`priority[2]`,
        // guaranteed distinct from vendor1/vendor2 and from the coder's own
        // vendor), it is dispatched as a fallback BEFORE giving up — live
        // incident 2026-07-09: `agy` silently returns empty stdout and
        // `codex` is quota-exhausted, so with `priority = [codex, claude,
        // agy]` the pre-fix code never reached the healthy `claude` vendor
        // at `priority[2]`, permanently false-failing gate 7. Only when the
        // fallback ALSO fails to produce a parseable verdict (or no third
        // vendor exists) does `Err` propagate out of this function to
        // `run_fast_tier`'s per-bead catch-and-continue (phase =
        // "skeptic_evidence"); the bead stays ATTESTED and the next tick
        // retries, instead of guessing a verdict.
        let vendor1 = priority.first().copied().unwrap_or("codex").to_string();
        let vendor2 = priority.get(1).copied().unwrap_or("claude").to_string();
        let vendor1_label = vendor1.clone();
        let vendor2_label = vendor2.clone();

        let prompt1 = prompt.clone();
        let handle1 = std::thread::spawn(move || dispatch_reviewer(&vendor1, &prompt1));
        let prompt2 = prompt.clone();
        let handle2 = std::thread::spawn(move || dispatch_reviewer(&vendor2, &prompt2));

        let res1 = handle1.join().unwrap_or(Err(DaemonError::Tool {
            tool: "thread".into(),
            rc: -1,
            stderr: "join failed".into(),
        }));
        let res2 = handle2.join().unwrap_or(Err(DaemonError::Tool {
            tool: "thread".into(),
            rc: -1,
            stderr: "join failed".into(),
        }));

        let v1 = res1.ok().and_then(|r| verifier::parse_skeptic_verdict(&r));
        let v2 = res2.ok().and_then(|r| verifier::parse_skeptic_verdict(&r));
        let v1_present = v1.is_some();
        let v2_present = v2.is_some();

        let dual_verdict = match combine_dual_verdict(v1, v2, bead_id, pr) {
            Ok(v) => {
                // jleechan-wzgl: both dual-dispatch primaries can
                // contribute (e.g. two Fails combine into one `Fail`
                // reason) — record whichever of the two actually produced
                // a parseable verdict, in dispatch order, so telemetry
                // never over- or under-reports which vendor(s) ran.
                if v1_present {
                    used_vendors.push(vendor1_label.clone());
                }
                if v2_present {
                    used_vendors.push(vendor2_label.clone());
                }
                v.expect("combine_dual_verdict returns Some(..) whenever it returns Ok(..)")
            }
            Err(total_outage_err) => {
                // vendor1 AND vendor2 both failed to parse. Try each
                // remaining `priority` member (index 2, 3, ...) in turn
                // before propagating the outage. jleechan-bkru generalizes
                // the original single priority[2] fallback (jleechan-qdw)
                // into a loop over ALL remaining vendors, so a 4th (or
                // later) configured vendor — e.g. `gemini` at priority[3]
                // — is reachable too. Live incident 2026-07-09: codex
                // (quota-exhausted), claude (weekly limit hit), AND agy
                // (quota-exhausted + a session-continuity bug) were ALL
                // simultaneously non-functional, so a fallback that only
                // ever tried a single 3rd vendor was no longer sufficient.
                let mut fallback_verdict = None;
                let mut fallback_vendor: Option<String> = None;
                for vendor_n in priority.iter().skip(2) {
                    let v_n = dispatch_reviewer(vendor_n, &prompt)
                        .ok()
                        .and_then(|r| verifier::parse_skeptic_verdict(&r));
                    let v_n_present = v_n.is_some();
                    // Re-use combine_dual_verdict as a single-verdict
                    // wrapper (its (Some, None) arms already treat a lone
                    // verdict as a full success); if vendor_n ALSO fails to
                    // parse this returns the same kind of total-outage
                    // `Err` as above and the loop tries the next vendor.
                    if let Ok(v) = combine_dual_verdict(v_n, None, bead_id, pr) {
                        fallback_verdict = v;
                        if v_n_present {
                            fallback_vendor = Some((*vendor_n).to_string());
                        }
                        break;
                    }
                }
                match fallback_verdict {
                    Some(v) => {
                        // jleechan-wzgl: v1_present/v2_present were both
                        // false in this arm (that is what made
                        // combine_dual_verdict return `Err` above), so only
                        // the fallback vendor that actually produced this
                        // verdict is recorded — never codex/claude, which
                        // were dispatched but never produced usable output.
                        if let Some(fv) = fallback_vendor {
                            used_vendors.push(fv);
                        }
                        v
                    }
                    // Every remaining vendor (if any) also failed to parse
                    // — or coder vendor exclusion left fewer than 3
                    // candidates in the first place — propagate the
                    // original total-outage error so `run_fast_tier`'s
                    // catch-and-continue retries next tick instead of
                    // guessing a verdict.
                    None => return Err(total_outage_err),
                }
            }
        };

        // PR#163 finding 1 (round 1) + finding (round 3, residual): `gha`
        // (a target-repo CI workflow posting a skeptic verdict comment) and
        // `sign-off` (a human reviewer comment) are both OPTIONAL
        // enrichment signals, never hard requirements — a human sign-off
        // will never exist in this Level-5 autonomous system, and not
        // every target repo runs an equivalent GHA skeptic workflow.
        // Requiring either would permanently deadlock gate 7 exactly like
        // the pre-fix bug did. `verifier::parse_skeptic_verdict` now treats
        // BOTH `gha` and `sign-off` as optional (only `gate-7`, the
        // dual-LLM verdict, is a hard requirement there), so this function
        // no longer needs to special-case "both absent" — but it still
        // avoids synthesizing a `subsystem: gha` / `subsystem: sign-off`
        // block with the literal placeholder `"verdict: absent"` for
        // whichever subsystem has no real evidence: only a subsystem block
        // with REAL evidence is emitted. (Round 3 root cause: emitting a
        // placeholder block for an evidence-less subsystem was harmless by
        // itself once verifier.rs's `gha`/`sign-off` are optional, but
        // omitting it entirely here is simpler, avoids ever depending on
        // `parse_skeptic_verdict` correctly ignoring "verdict: absent" as
        // defense in depth, and keeps the combined string minimal.) When
        // real evidence exists it is folded in through
        // `parse_skeptic_verdict`'s subsystem grammar so it can still
        // escalate (Fail beats Warn beats Pass); when neither has real
        // evidence, gate 7 is satisfied by the dual-LLM verdict alone.
        let has_gha_evidence = gha_verdict != "verdict: absent";
        let has_signoff_evidence = signoff_verdict != "verdict: absent";

        if !has_gha_evidence && !has_signoff_evidence {
            Some(dual_verdict)
        } else {
            let mut combined = String::new();
            combined.push_str("subsystem: gate-7\n");
            combined.push_str(&skeptic_verdict_to_line(&dual_verdict));
            combined.push('\n');

            if has_gha_evidence {
                combined.push_str("subsystem: gha\n");
                combined.push_str(gha_verdict);
                combined.push('\n');
            }

            if has_signoff_evidence {
                combined.push_str("subsystem: sign-off\n");
                combined.push_str(signoff_verdict);
                combined.push('\n');
            }

            verifier::parse_skeptic_verdict(&combined)
        }
    };

    Ok(PrEvidence {
        is_production: false,
        non_test_changed_loc: 0,
        has_integration_evidence_marker: false,
        er_verdict: verifier::ErVerdict::Absent,
        skeptic_verdict,
        skeptic_reviewers: used_vendors,
    })
}

/// Combine two reviewer verdicts (one per subprocess) into a single
/// `SkepticVerdict` for gate 7.
///
/// Single-pass success is preserved: if EITHER reviewer returns a usable
/// verdict (`Fail`/`Warn`/`Pass`), that verdict wins (Fail beats Warn
/// beats Pass in priority, and either side overrides `None`).
///
/// jleechan-qdw: when BOTH reviewers failed (tool errored OR reply was
/// unparseable, i.e. `v1 == None && v2 == None`) the previous code
/// returned `Ok(None)` — gate 7 became `Unknown`, `all_green` became
/// `false`, and Stage-1's substitution rule parked the bead
/// `HUMAN_HELD`. That is a false-park on a reviewer-tool outage (the
/// bead is not known-bad; it is unjudged). Return `Err` instead so
/// `run_fast_tier`'s per-bead catch-and-continue fires (phase =
/// `skeptic_evidence`), the bead stays ATTESTED, and the next tick
/// retries the reviewer call.
pub fn combine_dual_verdict(
    v1: Option<verifier::SkepticVerdict>,
    v2: Option<verifier::SkepticVerdict>,
    bead_id: &str,
    pr: u64,
) -> Result<Option<verifier::SkepticVerdict>, DaemonError> {
    let combined = match (v1, v2) {
        (Some(verifier::SkepticVerdict::Fail(r1)), Some(verifier::SkepticVerdict::Fail(r2))) => {
            Some(verifier::SkepticVerdict::Fail(format!("{r1} && {r2}")))
        }
        (Some(verifier::SkepticVerdict::Fail(r)), _) => Some(verifier::SkepticVerdict::Fail(r)),
        (_, Some(verifier::SkepticVerdict::Fail(r))) => Some(verifier::SkepticVerdict::Fail(r)),
        (Some(verifier::SkepticVerdict::Warn(w1)), Some(verifier::SkepticVerdict::Warn(w2))) => {
            Some(verifier::SkepticVerdict::Warn(format!("{w1} && {w2}")))
        }
        (Some(verifier::SkepticVerdict::Warn(w)), _) => Some(verifier::SkepticVerdict::Warn(w)),
        (_, Some(verifier::SkepticVerdict::Warn(w))) => Some(verifier::SkepticVerdict::Warn(w)),
        (Some(verifier::SkepticVerdict::Pass), Some(verifier::SkepticVerdict::Pass)) => {
            Some(verifier::SkepticVerdict::Pass)
        }
        (Some(verifier::SkepticVerdict::Pass), None) => Some(verifier::SkepticVerdict::Pass),
        (None, Some(verifier::SkepticVerdict::Pass)) => Some(verifier::SkepticVerdict::Pass),
        _ => None,
    };
    if combined.is_none() {
        return Err(DaemonError::Tool {
            tool: "skeptic_evidence".into(),
            rc: 1,
            stderr: format!(
                "both reviewers failed to produce a parseable verdict for bead {bead_id} PR #{pr}"
            ),
        });
    }
    Ok(combined)
}

/// Fast tier: for every bead whose overlay is `ATTESTED` (or freshly promoted
/// from `DISPATCHED` because its PR is now open), assess all 7 gates. All
/// green -> `READY` (terminal) + `READY_FOR_MERGE`. Not all green -> Stage-1
/// substitution rule: emit `REROLL_VERDICT_RECORDED` and park `HUMAN_HELD`,
/// never enter `RE_ROLL` or execute the Re-Roll Engine.
fn run_fast_tier(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    // In-flight beads are discovered via the branch registry (populated by
    // `dispatch::dispatch_ready`'s `register_branch` call), not
    // `Tracker::fetch_candidates` — a bead can be DISPATCHED/ATTESTED long
    // after it drops out of `br list --status open --label factory` filters,
    // and `branch_registry` is this store's authoritative "beads we're
    // actively tracking" set (deletion-guard doc comment on
    // `StateStore::owned_branches`).
    let branches = deps.store.owned_branches()?;
    let mut bead_ids: Vec<String> = Vec::new();
    for branch in &branches {
        if let Ok(Some(bead_id)) = deps.store.bead_id_for_branch(branch) {
            bead_ids.push(bead_id);
        }
    }
    bead_ids.sort();
    bead_ids.dedup();

    for bead_id in &bead_ids {
        let mut overlay = match deps.store.load(bead_id)? {
            Some(o) => o,
            None => continue,
        };
        // jleechan-9xrs Stage D: resolve THIS bead's own repo once per
        // iteration (`overlay.repo(cfg)` — `None` falls back to
        // `cfg.target_repo`, so legacy beads are unaffected) and thread it
        // through every verification-loop call below instead of reading
        // `deps.cfg.target_repo` directly. See
        // docs/multirepo-dispatch-investigation-2026-07-11.md Stage D.
        let repo = overlay.repo(deps.cfg).to_string();

        if overlay.state == OverlayState::Dispatched && overlay.pr_number.is_none() {
            let is_test_repo =
                repo.contains("fake-") || repo.contains("test-") || repo == "owner/repo";

            if !is_test_repo {
                if let Some(ref session_id) = overlay.session_id {
                    let mut project = repo.split('/').next_back().unwrap_or(&repo).to_string();
                    if project == "worldarchitect.ai" {
                        project = "worldarchitect".to_string();
                    }

                    let r = crate::tools::run_tool("ao", &["status", "-p", &project, "--json"], 30);
                    if let Ok(out) = r {
                        let json_start = out.find('[').unwrap_or(0);
                        if let Ok(val) =
                            serde_json::from_str::<serde_json::Value>(&out[json_start..])
                        {
                            if let Some(arr) = val.as_array() {
                                if let Some(entry) = arr.iter().find(|e| {
                                    e.get("name").and_then(|v| v.as_str())
                                        == Some(session_id.as_str())
                                }) {
                                    if let Some(pr_num) =
                                        entry.get("prNumber").and_then(|v| v.as_u64())
                                    {
                                        overlay.pr_number = Some(pr_num);
                                        deps.store.save(&overlay)?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Promote DISPATCHED -> ATTESTED once a PR is open (spec §4.2.7).
        if overlay.state == OverlayState::Dispatched {
            if overlay.pr_number.is_none() {
                if let Some(ref branch) = overlay.branch {
                    if let Ok(out) = crate::tools::run_tool(
                        "gh",
                        &[
                            "pr",
                            "list",
                            "--head",
                            branch,
                            "--repo",
                            &repo,
                            "--json",
                            "number",
                            "--jq",
                            ".[0].number",
                        ],
                        30,
                    ) {
                        if let Ok(pr) = out.trim().parse::<u64>() {
                            overlay.pr_number = Some(pr);
                        }
                    }
                }
            }

            if let Some(pr) = overlay.pr_number {
                // Adopted-branch remediation reuses DISPATCHED with
                // `pr_number` already set from adoption time. Gate promotion
                // on the remediation coder session having quiesced so the
                // verifier checks real landed work, not the stale pre-fix
                // commit.
                let ready_to_promote = if overlay.is_adopted {
                    match &overlay.session_id {
                        Some(session_id_str) => deps
                            .sessions
                            .is_quiescent(&SessionId(session_id_str.clone()))
                            .unwrap_or(false),
                        None => false,
                    }
                } else {
                    true
                };

                if ready_to_promote {
                    overlay.state = OverlayState::Attested;
                    deps.store.save(&overlay)?;
                    let event_type = if overlay.is_adopted {
                        "REROLL_ADOPTED_SESSION_QUIESCED"
                    } else {
                        "PR_OPENED"
                    };
                    emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        event_type,
                        serde_json::json!({}),
                        serde_json::json!({"pr_number": pr}),
                    )?;
                    let comment_body = if overlay.is_adopted {
                        format!(
                            "🤖 **[dark-factory]** Remediation coder session finished working on `{}` (attempt {}). Re-running gate verification against the latest commits...",
                            overlay.branch.clone().unwrap_or_default(),
                            overlay.attempt
                        )
                    } else {
                        format!(
                            "🤖 **[dark-factory]** Worker session opened this pull request for bead `{}`. Beginning gate-by-gate safety verification...",
                            bead_id
                        )
                    };
                    let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                }
            }
        }

        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // jleechan-zaga / issue #348: a `DISPOSITION_REQUIRED` bead must
        // KEEP being re-assessed — otherwise it is a terminal hold (the fast
        // tier's ATTESTED-only filter would never look at it again, so the
        // chain could never resume when the structural condition clears).
        //
        // r3 residual 2 (cooldown): a structural condition can persist for
        // hours; re-fetching the PR snapshot every fast tick would hammer the
        // SCM API. Skip re-assessment until the durable per-bead
        // `held_recheck_after` cooldown elapses, and stamp the NEXT recheck now
        // (before any SCM fetch) so the cooldown holds regardless of how this
        // re-assessment exits (resume / reroll / re-hold / transient error).
        //
        // r3 residual 3 (provenance): the promotion back to ATTESTED is
        // IN-MEMORY only and is NOT persisted here. The stored state stays
        // DISPOSITION_REQUIRED until assessment reaches a terminal decision
        // (READY / reroll / re-hold), so an early-exit (snapshot fetch failure,
        // ci_pending, transient) leaves hold provenance intact and does not
        // let the next re-hold double-emit the counter/telemetry/comment. The
        // reroll branch persists the ATTESTED promotion just before calling
        // `reroll::execute` (whose freshness guard requires ATTESTED/RE_ROLL).
        let entered_as_disposition = overlay.state == OverlayState::DispositionRequired;
        if entered_as_disposition {
            if let Some(recheck_after) = deps.store.held_recheck_after(bead_id)? {
                if now_epoch < recheck_after {
                    continue; // still in cooldown — do not touch the SCM API.
                }
            }
            deps.store.set_held_recheck_after(
                bead_id,
                now_epoch.saturating_add(deps.cfg.held_recheck_cooldown_secs),
            )?;
            overlay.state = OverlayState::Attested; // in-memory only (NOT saved)
        }
        if overlay.state != OverlayState::Attested {
            continue;
        }
        let pr = match overlay.pr_number {
            Some(pr) => pr,
            None => continue,
        };

        // jleechan-qdw: per-bead isolation. A transient `gh`/GraphQL/network
        // hiccup fetching THIS bead's PR snapshot must not abort the fast
        // tier for the rest of the in-flight beads — one bead's failure
        // cannot stop another bead in the same tick from advancing. Log the
        // failure via telemetry and skip to the next bead; the bead stays
        // ATTESTED so the next tick retries the snapshot fetch (no false-
        // green, no false-park on a single transient error).
        let mut snapshot = match deps.scm.pr_snapshot_for_repo(&repo, pr) {
            Ok(snap) => snap,
            Err(e) => {
                let _ = emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "BEAD_SNAPSHOT_TRANSIENT_ERROR",
                    serde_json::json!({}),
                    serde_json::json!({"phase": "fast_tier", "error": format!("{e:?}")}),
                );
                continue;
            }
        };
        // Task 2 (reviewer-outage-resilience): outage-aware CI-pending
        // override. When `ci_pending` is true ONLY because of stale pending
        // check-runs from in-outage review-bot providers (and every real CI
        // check has completed), waive those pending statuses after a 15-minute
        // grace period (measured from the head commit's committer epoch) so
        // the assessment can proceed and report the true CI result. All real
        // CI failures still fail the assessment exactly as before — the
        // override never waives a FAILED check, only a stale PENDING status.
        if snapshot.ci_pending {
            // Build the set of in-outage provider name-match patterns from
            // the vendor_health ledger. "coderabbit" -> ["coderabbit"];
            // "bugbot" -> ["bugbot", "cursor"] (matching the bugbot_status
            // derivation in adapters.rs).
            let mut outage_patterns: Vec<&str> = Vec::new();
            let mut outage_vendors: Vec<&str> = Vec::new();
            if deps
                .store
                .vendor_health("coderabbit")
                .ok()
                .flatten()
                .is_some_and(|h| h.in_outage)
            {
                outage_patterns.push("coderabbit");
                outage_vendors.push("coderabbit");
            }
            if deps
                .store
                .vendor_health("bugbot")
                .ok()
                .flatten()
                .is_some_and(|h| h.in_outage)
            {
                outage_patterns.push("bugbot");
                outage_patterns.push("cursor");
                outage_vendors.push("bugbot");
            }

            // Partition pending check names: those matching an in-outage
            // provider pattern (case-insensitive substring) vs. real CI.
            let matches_outage = |name: &str| {
                let lower = name.to_lowercase();
                outage_patterns.iter().any(|p| lower.contains(p))
            };
            let waived: Vec<String> = snapshot
                .pending_check_names
                .iter()
                .filter(|n| matches_outage(n))
                .cloned()
                .collect();
            let real_pending: Vec<String> = snapshot
                .pending_check_names
                .iter()
                .filter(|n| !matches_outage(n))
                .cloned()
                .collect();

            // Only override when ALL pending check-runs belong to in-outage
            // providers. Any real CI still pending => keep waiting.
            if real_pending.is_empty() && !outage_patterns.is_empty() {
                let now_epoch_ovr = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let elapsed = now_epoch_ovr.saturating_sub(snapshot.head_committed_epoch);
                if elapsed >= OUTAGE_GRACE_PERIOD_SECS {
                    // Recompute ci_success/ci_status from the full check-run
                    // list, EXCLUDING in-outage provider checks. Real CI
                    // failures remain failures; an all-outage-provider PR
                    // (no remaining checks) is treated as green (waived).
                    let remaining: Vec<&(String, String)> = snapshot
                        .check_names_and_buckets
                        .iter()
                        .filter(|(name, _)| !matches_outage(name))
                        .collect();
                    let any_real_fail = remaining
                        .iter()
                        .any(|(_, b)| b == "fail" || b == "cancel");
                    let any_real_pending = remaining
                        .iter()
                        .any(|(_, b)| b == "pending");
                    let (ci_success, ci_status) = if remaining.is_empty() {
                        (true, "green".to_string())
                    } else if any_real_fail {
                        (false, "red".to_string())
                    } else if any_real_pending {
                        // Should not happen (real_pending was empty), but
                        // fail-closed: keep pending rather than false-green.
                        (snapshot.ci_success, snapshot.ci_status.clone())
                    } else {
                        (true, "green".to_string())
                    };
                    snapshot.ci_pending = false;
                    snapshot.ci_success = ci_success;
                    snapshot.ci_status = ci_status;
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "VERIFICATION_OUTAGE_GRACE_PERIOD",
                        serde_json::json!({}),
                        serde_json::json!({
                            "vendor": outage_vendors,
                            "head_sha": snapshot.head_sha,
                            "head_committed_epoch": snapshot.head_committed_epoch,
                            "grace_period_secs": OUTAGE_GRACE_PERIOD_SECS,
                            "pending_checks_waived": waived,
                            "real_ci_result": if snapshot.ci_success { "green" } else { "red" },
                        }),
                    );
                } else {
                    // Grace period not yet elapsed — keep waiting.
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "VERIFICATION_OUTAGE_GRACE_WAIT",
                        serde_json::json!({}),
                        serde_json::json!({
                            "vendor": outage_vendors,
                            "head_sha": snapshot.head_sha,
                            "head_committed_epoch": snapshot.head_committed_epoch,
                            "elapsed_secs": elapsed,
                            "grace_period_secs": OUTAGE_GRACE_PERIOD_SECS,
                            "pending_checks_waived": waived,
                        }),
                    );
                }
            }
        }
        if snapshot.ci_pending {
            emit(
                deps.telemetry_log,
                bead_id,
                overlay.attempt,
                OverlayState::Attested.as_str(),
                "VERIFICATION_PENDING",
                serde_json::json!({}),
                serde_json::json!({"message": "CI checks are still running (in progress), waiting for completion"}),
            )?;
            continue;
        }

        let mut evidence = match skeptic_evidence(deps, bead_id, pr, &repo, &snapshot) {
            Ok(e) => e,
            Err(e) => {
                let _ = emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "BEAD_PROCESSING_TRANSIENT_ERROR",
                    serde_json::json!({}),
                    serde_json::json!({"phase": "skeptic_evidence", "error": format!("{e:?}")}),
                );
                continue;
            }
        };

        // jleechan-qqq: if no `/er` verdict is recorded yet, dispatch an
        // independent reviewer (claude/codex subprocess) and post the
        // verdict as a PR comment. Re-fetch the snapshot so the just-
        // posted comment is visible to `parse_er_verdict` below.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Task 1 (reviewer-outage-resilience): record vendor health
        // observations for BOTH external review-bot providers from the
        // fetched PrSnapshot, BEFORE verifier::assess. Outage marker if the
        // provider's status is "unknown" (pending/unavailable); success if
        // the provider approved / is clean. Emit VENDOR_WAIVED / VENDOR_RECOVERED
        // telemetry ONLY on state transitions (0→1 for waived, 1→0 for recovered).
        {
            let vendors: [(&str, bool, bool); 2] = [
                (
                    "coderabbit",
                    snapshot.coderabbit_status == "unknown",
                    snapshot.coderabbit_approved,
                ),
                (
                    "bugbot",
                    snapshot.bugbot_status == "unknown",
                    snapshot.bugbot_status == "green",
                ),
            ];
            for (vendor, is_outage_marker, is_success) in vendors {
                let prior = deps.store.vendor_health(vendor).ok().flatten();
                let row = match deps.store.record_vendor_observation(
                    vendor,
                    is_outage_marker,
                    is_success,
                    &snapshot.head_sha,
                    now_epoch,
                    VENDOR_OUTAGE_CONSECUTIVE_PENDING_THRESHOLD,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "VENDOR_HEALTH_RECORD_ERROR",
                            serde_json::json!({}),
                            serde_json::json!({
                                "vendor": vendor,
                                "error": format!("{e:?}"),
                            }),
                        );
                        continue;
                    }
                };
                let was_in_outage = prior.as_ref().is_some_and(|p| p.in_outage);
                if row.in_outage && !was_in_outage {
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "VENDOR_WAIVED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "vendor": vendor,
                            "consecutive_pending": row.consecutive_pending,
                            "head_sha": snapshot.head_sha,
                            "reason": format!(
                                "provider marked in-outage after {} consecutive pending assessments",
                                row.consecutive_pending
                            ),
                        }),
                    );
                } else if !row.in_outage && was_in_outage {
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "VENDOR_RECOVERED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "vendor": vendor,
                            "last_success_head": row.last_success_head.clone().unwrap_or_default(),
                            "outage_observations": row.outage_observations,
                            "success_observations": row.success_observations,
                        }),
                    );
                }
            }
        }
        let runner_outcome = match crate::er_runner::maybe_run(deps, bead_id, pr, now_epoch) {
            Ok(out) => out,
            Err(e) => {
                let _ = emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "BEAD_PROCESSING_TRANSIENT_ERROR",
                    serde_json::json!({}),
                    serde_json::json!({"phase": "er_runner", "error": format!("{e:?}")}),
                );
                continue;
            }
        };
        // When the runner just posted a verdict this tick, prefer the
        // returned verdict over a re-parse of the refreshed snapshot —
        // `parse_er_verdict` would otherwise pick up any "/er" token
        // anywhere in the comment body, including in the runner's own
        // formatted prefix, and could disagree with the verdict the
        // runner just emitted. Only fall back to the snapshot when the
        // runner DIDN'T post (cooldown/capped/already-present/no-op).
        let mut posted_verdict: Option<verifier::ErVerdict> = None;
        let mut er_runner_capped_count: Option<u32> = None;
        match runner_outcome {
            crate::er_runner::Outcome::Posted { verdict, count } => {
                posted_verdict = Some(verdict);
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    crate::er_runner::EVT_POSTED,
                    serde_json::json!({}),
                    serde_json::json!({
                        "verdict": format!("{verdict:?}"),
                        "attempt": count,
                    }),
                )?;
                // jleechan-qdw: per-bead isolation for the post-/er refetch.
                // If the refresh fails after posting, this bead's SCM view
                // is transiently unavailable. Emit the outage and retry on a
                // later tick rather than falling through into `assess()`,
                // which performs another `pr_snapshot` and would turn the
                // outage into Unknown/all_green=false -> HUMAN_HELD.
                match deps.scm.pr_snapshot_for_repo(&repo, pr) {
                    Ok(snap) => snapshot = snap,
                    Err(e) => {
                        let _ = emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "BEAD_SNAPSHOT_TRANSIENT_ERROR",
                            serde_json::json!({}),
                            serde_json::json!({"phase": "post_er_refetch", "error": format!("{e:?}")}),
                        );
                        continue;
                    }
                }
            }
            crate::er_runner::Outcome::AlreadyPosted(v) => {
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    crate::er_runner::EVT_NOOP,
                    serde_json::json!({}),
                    serde_json::json!({"reason": "already_posted", "verdict": format!("{v:?}")}),
                )?;
            }
            crate::er_runner::Outcome::Capped { count } => {
                er_runner_capped_count = Some(count);
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    crate::er_runner::EVT_CAPPED,
                    serde_json::json!({}),
                    serde_json::json!({"count": count}),
                )?;
            }
            crate::er_runner::Outcome::Cooldown {
                elapsed_secs,
                count,
            } => {
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    crate::er_runner::EVT_NOOP,
                    serde_json::json!({}),
                    serde_json::json!({"reason": "cooldown", "elapsed_secs": elapsed_secs, "count": count}),
                )?;
            }
            crate::er_runner::Outcome::NotApplicable => {}
        }

        evidence.er_verdict = match posted_verdict {
            Some(v) => v,
            // jleechan-nplh: a verdict comment older than the current head
            // commit is stale evidence — gate 6 must not self-certify from
            // it (same staleness rule as `er_runner::maybe_run` step 2).
            None => verifier::parse_er_verdict_since(
                &snapshot.comments,
                snapshot.head_committed_epoch,
            ),
        };
        evidence.is_production = verifier::classify_production(&snapshot.files);
        evidence.non_test_changed_loc = verifier::calculate_non_test_loc(&snapshot.files);
        evidence.has_integration_evidence_marker =
            verifier::check_integration_marker(&snapshot.body, &snapshot.comments);
        let report = verifier::assess(deps.scm, pr, &repo, deps.cfg, &evidence)?;
        summary.gates_assessed += 1;
        // jleechan-wzgl: log the full per-gate breakdown (all 7 gates,
        // verdict + reason) plus the gate-7 reviewer vendor identity, not
        // just the aggregate `all_green` boolean — `report.to_json()` is
        // `verifier::assess`'s own serialization, so this can't drift from
        // what was actually computed, and `evidence.skeptic_reviewers`
        // names the vendor(s) that produced this tick's skeptic verdict.
        //
        // Task 3 (reviewer-outage-resilience): annotate in-outage provider
        // gates with the canonical "waived_vendor_unavailable" token. Build
        // the in-outage vendor list from the `vendor_health` ledger for both
        // review providers, then pass it to `to_json_with_outage` so the
        // provider's OWN gate key (coderabbit / bugbot) carries the waiver
        // token instead of its real verdict. All other gates keep their real
        // verdict and remain fully enforced/blocking.
        let mut in_outage_vendors: Vec<&str> = Vec::new();
        if deps
            .store
            .vendor_health("coderabbit")
            .ok()
            .flatten()
            .is_some_and(|h| h.in_outage)
        {
            in_outage_vendors.push("coderabbit");
        }
        if deps
            .store
            .vendor_health("bugbot")
            .ok()
            .flatten()
            .is_some_and(|h| h.in_outage)
        {
            in_outage_vendors.push("bugbot");
        }
        let mut gate_assessment_context = report.to_json_with_outage(&in_outage_vendors);
        if let Some(obj) = gate_assessment_context.as_object_mut() {
            // Task 3: surface the list of vendors whose gates were waived so
            // the merge authority and operators can see which gates carried
            // "waived_vendor_unavailable" without re-deriving it.
            obj.insert(
                "waived_vendors".to_string(),
                serde_json::json!(in_outage_vendors),
            );
            obj.insert(
                "skeptic_reviewers".to_string(),
                serde_json::json!(evidence.skeptic_reviewers),
            );
            // jleechan-wzgl (PR #239 review round 1): `auto-merge-guard.sh`'s
            // `latest_assessment_no_red` greps GATE_ASSESSMENT lines by
            // `context.pr_number` before parsing `context.gates` — without
            // this key the guard's match path is permanently dormant no
            // matter how correct the `gates` shape is.
            obj.insert("pr_number".to_string(), serde_json::json!(pr));
        }
        emit(
            deps.telemetry_log,
            bead_id,
            overlay.attempt,
            OverlayState::Attested.as_str(),
            "GATE_ASSESSMENT",
            serde_json::json!({}),
            gate_assessment_context,
        )?;

        if report.all_green {
            overlay.state = OverlayState::Ready;
            deps.store.save(&overlay)?;
            summary.beads_ready += 1;
            emit(
                deps.telemetry_log,
                bead_id,
                overlay.attempt,
                OverlayState::Ready.as_str(),
                "READY_FOR_MERGE",
                serde_json::json!({}),
                serde_json::json!({}),
            )?;
            let comment_body = format!(
                "🤖 **[dark-factory]** All safety gates are now GREEN for bead `{}`. PR is merge-ready!",
                bead_id
            );
            let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
        } else {
            let red_reasons: Vec<String> = report
                .results
                .iter()
                .filter_map(|(gate_name, result)| match result {
                    verifier::GateResult::Red(reason) => Some(format!("{gate_name:?}: {reason}")),
                    _ => None,
                })
                .collect();
            if red_reasons.is_empty() {
                // jleechan-zaga / issue #348: no coder-fixable RED gate, but
                // is the chain blocked by a STRUCTURAL-pending gate (an
                // external verifier a coder cannot drive — CodeRabbit
                // unavailable, Bugbot absent)? If so, hold
                // DISPOSITION_REQUIRED and keep re-assessing, rather than
                // silently churning the ATTESTED transient path forever (which
                // eventually cap-parks HUMAN_HELD — the exact regression #348
                // documents). A purely TRANSIENT report (CI still running,
                // unverifiable thread count) falls through to the existing
                // transient handling below.
                if deps.cfg.stage == 2
                    && matches!(
                        verifier::classify_chain(&report),
                        verifier::ChainDisposition::HoldDisposition
                    )
                {
                    let structural_gates: Vec<serde_json::Value> =
                        verifier::structural_pending_gates(&report)
                            .into_iter()
                            .map(|(gate_name, reason)| {
                                serde_json::json!({
                                    "gate": gate_name.as_str(),
                                    "reason": reason,
                                    "disposition": "structural",
                                })
                            })
                            .collect();
                    overlay.state = OverlayState::DispositionRequired;
                    deps.store.save(&overlay)?;
                    // Only a NEW hold (not a re-hold of an already-held bead)
                    // increments the operator counter, emits the telemetry
                    // event, and posts the comment — a bead re-assessed and
                    // still structural must not spam the PR every tick.
                    if !entered_as_disposition {
                        // A first hold starts the re-assessment cooldown (a
                        // re-hold already stamped it at re-assessment start).
                        deps.store.set_held_recheck_after(
                            bead_id,
                            now_epoch.saturating_add(deps.cfg.held_recheck_cooldown_secs),
                        )?;
                        summary.beads_held_disposition_required += 1;
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::DispositionRequired.as_str(),
                            "DISPOSITION_REQUIRED",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "chain blocked only by structural-pending gate(s) (external verifier the coder cannot drive); reroll would be no-op churn",
                                "structural_gates": structural_gates,
                                "pr_number": pr,
                            }),
                        )?;
                        let gate_lines: Vec<String> = structural_gates
                            .iter()
                            .filter_map(|g| {
                                let gate = g.get("gate")?.as_str()?;
                                let reason = g.get("reason")?.as_str()?;
                                Some(format!("- `{gate}`: {reason}"))
                            })
                            .collect();
                        let comment_body = format!(
                            "🤖 **[dark-factory]** Disposition required for bead `{bead_id}`: the only remaining blockers are structural (an external verifier the coder cannot drive — re-rolling cannot clear them). Daemon held at `DISPOSITION_REQUIRED` rather than superseding. Per-gate disposition needs:\n\n{}\n\nThe fast tier will continue to assess on each tick; the bead resumes the moment any gate becomes coder-fixable or green.",
                            gate_lines.join("\n")
                        );
                        let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                    }
                    continue;
                }
                if let Some(count) = er_runner_capped_count {
                    if escalation_already_recorded(deps, bead_id)? {
                        continue;
                    }
                    let comment_body = format!(
                        "🤖 **[dark-factory]** Escalation required: gate assessment is still Unknown-only after {} automated /er attempts. Automation parked bead `{}` HUMAN_HELD at the recovery cap for inspection rather than silently churning.",
                        count, bead_id
                    );
                    if let Err(err) = post_scm_comment_by_bead_id(deps, bead_id, &comment_body) {
                        if is_missing_scm_target_error(&err) {
                            // Unlike the other two cap-escalation sites, this
                            // bead has NOT been parked HUMAN_HELD yet (that
                            // only happens after a successful SCM comment
                            // below) — mirror that state transition here so
                            // the local-fallback path is equally terminal
                            // instead of leaving the bead ATTESTED to churn
                            // through the same Unknown-only gate report
                            // forever.
                            overlay.state = OverlayState::HumanHeld;
                            overlay.attempt = MAX_HUMAN_HELD_RECOVERY_ATTEMPT;
                            set_human_hold_reason(
                                &mut overlay,
                                HumanHoldReason::UnknownOnlyGateCapped,
                            );
                            deps.store.save(&overlay)?;
                            record_local_escalation_fallback(
                                deps,
                                bead_id,
                                "unknown_only_gate_report_with_er_runner_capped",
                            )?;
                            summary.beads_escalated_locally += 1;
                            summary.beads_parked_human_held += 1;
                            emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::HumanHeld.as_str(),
                                "ESCALATED_LOCALLY",
                                serde_json::json!({}),
                                serde_json::json!({
                                    "reason": "unknown_only_gate_report_with_er_runner_capped",
                                    "er_runner_attempts": count,
                                    "pr_number": pr,
                                    "scm_error": err.to_string(),
                                }),
                            )?;
                            continue;
                        }
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "ESCALATION_NOTIFICATION_FAILED",
                            serde_json::json!({}),
                            serde_json::json!({
                                "reason": "unknown_only_gate_report_with_er_runner_capped",
                                "er_runner_attempts": count,
                                "pr_number": pr,
                                "error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    overlay.state = OverlayState::HumanHeld;
                    overlay.attempt = MAX_HUMAN_HELD_RECOVERY_ATTEMPT;
                    set_human_hold_reason(
                        &mut overlay,
                        HumanHoldReason::UnknownOnlyGateCapped,
                    );
                    deps.store.save(&overlay)?;
                    record_escalation(
                        deps,
                        bead_id,
                        "unknown_only_gate_report_with_er_runner_capped",
                    )?;
                    summary.beads_escalated += 1;
                    summary.beads_parked_human_held += 1;
                    emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_REQUIRED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "unknown_only_gate_report_with_er_runner_capped",
                            "er_runner_attempts": count,
                            "pr_number": pr,
                        }),
                    )?;
                    continue;
                }
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "GATE_ASSESSMENT_TRANSIENT_UNKNOWN",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "gate assessment had unknown gates but no red gates"}),
                )?;
                continue;
            }

            if deps.cfg.stage == 1 {
                // Stage-1 substitution rule (CONTRACT.md §1): record the re-roll
                // verdict, never execute it. Park HUMAN_HELD instead of RE_ROLL.
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "REROLL_VERDICT_RECORDED",
                    serde_json::json!({}),
                    serde_json::json!({"stage": deps.cfg.stage}),
                )?;
                overlay.state = OverlayState::HumanHeld;
                // ATTESTED is reached only after positive worker quiescence;
                // make that no-live-session proof durable in this same save.
                overlay.session_id = None;
                set_human_hold_reason(&mut overlay, HumanHoldReason::Stage1GateNotGreen);
                deps.store.save(&overlay)?;
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "PARKED_HUMAN_HELD",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "gate assessment not all-green (stage 1: recorded, not executed)"}),
                )?;
                // bead jleechan-tfs1: use the explicit stored provenance
                // flag, not the `session_id.is_none()` proxy this used to
                // rely on (every adopted bead happens to have no session_id
                // since the factory never spawns one for an externally
                // authored branch, but that was an inference, not a fact —
                // `is_adopted` is the real fact).
                let comment_body = if overlay.is_adopted {
                    "🤖 **[dark-factory]** Escalation required: this adopted PR is not green, so automation parked it HUMAN_HELD. Remediation for adopted PRs lands with bead `jleechan-tfs1`; no replacement branch was fabricated.".to_string()
                } else {
                    "🤖 **[dark-factory]** Coder session parked (human held): gate assessment failed. Stage 1 configuration prevents re-roll."
                        .to_string()
                };
                let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
            } else {
                // Stage 2: execute re-roll engine (there is at least one
                // coder-fixable RED gate; the structural-only hold is handled
                // in the no-red branch above, before the transient path).
                //
                // r3 residual 3: if this bead was held DISPOSITION_REQUIRED,
                // its stored state is still DISPOSITION_REQUIRED (the promotion
                // above was in-memory only). Persist the ATTESTED promotion now
                // — assessment has completed and produced a coder-fixable red —
                // so `reroll::execute`'s ATTESTED/RE_ROLL freshness guard
                // accepts it instead of aborting.
                if entered_as_disposition {
                    deps.store.save(&overlay)?;
                }
                let mut reviewer = "verifier".to_string();
                for (gate_name, result) in &report.results {
                    if let verifier::GateResult::Red(_) = result {
                        if *gate_name == verifier::GateName::Skeptic {
                            reviewer = "skeptic".to_string();
                        } else if *gate_name == verifier::GateName::CodeRabbitApproved {
                            reviewer = "coderabbit".to_string();
                        }
                    }
                }
                let review_text = red_reasons.join("\n");

                let reroll_deps = crate::reroll::RerollDeps {
                    scm: deps.scm,
                    sessions: deps.sessions,
                    vcs: deps.vcs,
                    store: deps.store,
                    llm: deps.llm,
                    cfg: deps.cfg,
                    telemetry_log: deps.telemetry_log,
                    reviewer,
                    review_text,
                };

                match crate::reroll::execute(&reroll_deps, &mut overlay) {
                    Ok(crate::reroll::RerollOutcome::Rerolled { new_branch }) => {
                        if overlay.is_adopted {
                            // `reroll::execute` already spawned a real coder
                            // session on the EXISTING contributor branch
                            // (bead.state is now DISPATCHED, not ATTESTED).
                            // The fast-tier loop's quiescence-gated
                            // DISPATCHED -> ATTESTED promotion picks this
                            // back up once the session finishes.
                            emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                overlay.state.as_str(),
                                "REROLL_ADOPTED_REDISPATCH_SKIPPED",
                                serde_json::json!({}),
                                serde_json::json!({"branch": new_branch}),
                            )?;
                            let comment_body = format!(
                                "🤖 **[dark-factory]** Spawned a remediation coder session on `{}` (attempt {}) to address review feedback. This pull request remains open under your ownership; no replacement branch was created.",
                                new_branch, overlay.attempt
                            );
                            let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                            continue;
                        }
                        // Perform recovery validation: check if spec is valid TOML
                        let spec_path = std::path::Path::new(&deps.cfg.spec_dir)
                            .join(format!("{}.toml", overlay.bead_id));
                        let validation_pass = if spec_path.exists() {
                            if let Ok(c) = std::fs::read_to_string(&spec_path) {
                                toml::from_str::<serde_json::Value>(&c).is_ok()
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        if validation_pass {
                            overlay.state = OverlayState::Redispatched;
                            deps.store.save(&overlay)?;
                            emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::Redispatched.as_str(),
                                "REDISPATCHED",
                                serde_json::json!({}),
                                serde_json::json!({}),
                            )?;
                            let comment_body = format!(
                                "🤖 **[dark-factory]** Re-roll validation passed. Redispatched worker session (attempt {}). Branch: `factory/{}-r{}`.",
                                overlay.attempt, bead_id, overlay.attempt
                            );
                            let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                        } else {
                            overlay.state = OverlayState::HumanHeld;
                            set_human_hold_reason(
                                &mut overlay,
                                HumanHoldReason::SpecValidationFailed,
                            );
                            deps.store.save(&overlay)?;
                            summary.beads_parked_human_held += 1;
                            emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::HumanHeld.as_str(),
                                "PARKED_HUMAN_HELD",
                                serde_json::json!({}),
                                serde_json::json!({"reason": "spec file validation failed in recovery"}),
                            )?;
                            let comment_body =
                                "🤖 **[dark-factory]** Coder session parked (human held): spec file validation failed in recovery."
                                    .to_string();
                            let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                        }
                    }
                    Ok(crate::reroll::RerollOutcome::Held(reason)) => {
                        summary.beads_parked_human_held += 1;
                        // already saved to HumanHeld inside execute
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "PARKED_HUMAN_HELD",
                            serde_json::json!({}),
                            serde_json::json!({"reason": reason.clone()}),
                        )?;
                        let comment_body = format!("🤖 **[dark-factory]** Coder session parked (human held): re-roll held. Reason: {}", reason);
                        let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                    }
                    Ok(crate::reroll::RerollOutcome::Deferred(reason)) => {
                        // Bead jleechan-zeij / issue #322 r2: the fail-closed
                        // proceed predicate could not confirm the previous
                        // worker was safe to supersede this tick (active
                        // session, moving HEAD, or failed stop()). `execute`
                        // left the bead ATTESTED (no fresh branch, PR
                        // untouched, session_id preserved) so this loop
                        // re-selects and re-evaluates it next tick. This is
                        // NOT a park — do not count it toward
                        // beads_parked_human_held and do not post an
                        // escalation comment.
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "REROLL_DEFERRED",
                            serde_json::json!({}),
                            serde_json::json!({"reason": reason}),
                        )?;
                    }
                    Ok(crate::reroll::RerollOutcome::Aborted(_)) => {}
                    Err(e) if e.is_transient() => {
                        // jleechan-cq8r: per-bead isolation, matching the
                        // jleechan-qdw pattern used elsewhere in this same
                        // loop (BEAD_SNAPSHOT_TRANSIENT_ERROR /
                        // BEAD_PROCESSING_TRANSIENT_ERROR above). A single
                        // bead's TRANSIENT re-roll engine failure -- e.g. the
                        // circuit-breaker comparator's LLM call hitting a
                        // rate limit or returning a malformed reply -- must
                        // not abort processing for every OTHER in-flight
                        // bead in this tick. `reroll::execute` already
                        // persisted this bead as `ReRoll` before the
                        // failure; emit telemetry and move on to the next
                        // bead rather than propagating with `return Err`,
                        // which used to abort the entire fast tier. The bead is
                        // re-selected next tick once it returns to ATTESTED (or
                        // via the transient's own retry path).
                        let _ = emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            overlay.state.as_str(),
                            "BEAD_PROCESSING_TRANSIENT_ERROR",
                            serde_json::json!({}),
                            serde_json::json!({"phase": "reroll_execute", "error": format!("{e:?}")}),
                        );
                        continue;
                    }
                    Err(e) => {
                        // Bead jleechan-zeij / issue #322 r4 P1: a PERMANENT
                        // re-roll error must NOT be swallowed as transient.
                        // `reroll::execute` persisted this bead as RE_ROLL
                        // before returning, and the fast tier only re-selects
                        // ATTESTED overlays (see the `overlay.state != Attested`
                        // guard at the top of run_fast_tier), so logging-and-
                        // continuing would strand it in RE_ROLL forever,
                        // invisible to recovery. Park it HUMAN_HELD with a
                        // distinct, operator-visible reason instead.
                        overlay.state = OverlayState::HumanHeld;
                        set_human_hold_reason(
                            &mut overlay,
                            HumanHoldReason::RerollPermanentError,
                        );
                        deps.store.save(&overlay)?;
                        summary.beads_parked_human_held += 1;
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "PARKED_HUMAN_HELD",
                            serde_json::json!({}),
                            serde_json::json!({"reason": "reroll_permanent_error", "error": format!("{e:?}")}),
                        )?;
                        let comment_body = format!(
                            "🤖 **[dark-factory]** Coder session parked (human held): the re-roll engine hit a permanent (non-transient) error and cannot self-recover. Error: {e}"
                        );
                        let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                        continue;
                    }
                }
            }
        }
    }

    Ok(())
}

fn escalation_already_recorded(deps: &TickDeps, bead_id: &str) -> Result<bool, DaemonError> {
    Ok(matches!(
        deps.store
            .load_rejection(bead_id, ESCALATION_SENTINEL_ATTEMPT)?,
        Some((reviewer, _)) if reviewer == ESCALATION_REVIEWER
    ))
}

fn record_escalation(deps: &TickDeps, bead_id: &str, reason: &str) -> Result<(), DaemonError> {
    deps.store.save_rejection(
        bead_id,
        ESCALATION_SENTINEL_ATTEMPT,
        ESCALATION_REVIEWER,
        reason,
        reason,
    )
}

/// Substring unique to the `Err` `post_scm_comment_by_bead_id` returns when
/// it could not find ANY SCM comment target — i.e. the bead has no
/// `overlay.pr_number` and is absent (or has no `external_ref`) from
/// `fetch_candidates()`. Kept in sync with the message literal in
/// `post_scm_comment_by_bead_id` below.
const MISSING_SCM_TARGET_ERROR_MARKER: &str = "no SCM comment target found";

/// Distinguishes a permanently-missing SCM comment target from a transient
/// tracker/API failure (network error, rate limit, etc). Transient failures
/// come back as whatever error variant the underlying `Tracker` call
/// produces (typically `DaemonError::Tool`) and MUST keep retrying every
/// tick — see `capped_human_held_comment_failure_retries_before_recording_escalation`
/// and `capped_human_held_candidate_lookup_failure_retries_before_recording_escalation`
/// in `tests/tick_integration.rs`. A missing target is deterministic: no
/// `pr_number` and no matching `fetch_candidates()` row will NEVER resolve
/// on its own, so retrying it forever is pure waste (2026-07-09 live
/// incident: 45 beads stuck in exactly this state with zero durable trace).
fn is_missing_scm_target_error(err: &DaemonError) -> bool {
    matches!(err, DaemonError::Config(msg) if msg.contains(MISSING_SCM_TARGET_ERROR_MARKER))
}

/// Local park_reason marker prefix written on `bead_overlay` when
/// `post_scm_comment_by_bead_id` permanently has no target (see
/// `is_missing_scm_target_error`). Distinguishable from the circuit-breaker
/// prefix (`park_reason LIKE 'circuit-breaker%'`, which `recover_human_held`
/// excludes from requeue) so this marker is never mistaken for that guard —
/// beads reaching this path are already at/above
/// `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` and therefore already excluded from
/// requeue by attempt count regardless of `park_reason`.
/// Fallback for the HUMAN_HELD recovery-cap escalation idiom (used by
/// `run_recovery_step`, the dispatch spawn-retry-cap path, and the
/// unknown-only-gate-report cap path) when `post_scm_comment_by_bead_id`
/// fails with `is_missing_scm_target_error`. Because no SCM comment will
/// ever be postable for this bead, this persists a durable, human-visible
/// escalation marker directly on the bead's own `bead_overlay` row
/// (`park_reason`) AND records the same `review_rejection` escalation
/// sentinel `record_escalation` writes on the success path, so
/// `escalation_already_recorded` suppresses further attempts on later
/// ticks — turning "silently lost forever" into "durably visible, once."
/// Does not touch `overlay.state`/`attempt` (only common-case caller
/// behavior does that); if the overlay row is somehow gone by the time this
/// runs, this degrades to just recording the sentinel.
fn record_local_escalation_fallback(
    deps: &TickDeps,
    bead_id: &str,
    reason: &str,
) -> Result<(), DaemonError> {
    if let Some(mut overlay) = deps.store.load(bead_id)? {
        set_human_hold_reason(
            &mut overlay,
            HumanHoldReason::EscalationLocalFallback(reason.to_string()),
        );
        deps.store.save(&overlay)?;
    }
    record_escalation(deps, bead_id, reason)
}

fn parse_external_ref(external_ref: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// jleechan-drive-pr-branch-binding-pcpr: resolve whether `bead` should
/// dispatch onto an existing PR's own head branch instead of a freshly
/// generated `factory/<bead>-r<attempt>` one. Fires only when ALL of:
/// * `bead.external_ref` parses to `owner/repo#N`,
/// * that `owner/repo` matches the bead's OWN already-resolved repo
///   (`resolved_repo`, `overlay.repo(cfg)`) — a bead whose `external_ref`
///   happens to name a DIFFERENT repo than its resolved `target_repo` (a
///   stray/contradictory `target_repo:` body field) must never bind to
///   that other repo's PR,
/// * `owner/repo` is a configured repo (`cfg.resolve_repo`), and
/// * `Scm::open_pr_head_ref_for_repo` positively confirms PR `N` is OPEN
///   AND same-repo (`PrHeadBranch::SameRepo`) -> `DriveBranchDecision::PrHead`.
///
/// An OPEN PR whose head lives on a fork (`PrHeadBranch::Fork`) resolves to
/// `DriveBranchDecision::ForkFallback` — the fail-closed guard mirroring
/// `intake::same_repo_pr`; dispatch still falls back to the generated
/// branch, but telemetry records WHY (`branch_mode:
/// "generated_fork_fallback"`) instead of conflating it with "no drive-PR
/// signal at all".
///
/// Every other case — closed/merged/missing PR, malformed `external_ref`,
/// unconfigured repo, repo mismatch, or a transient lookup failure — is
/// `DriveBranchDecision::Generated` (fail-safe: dispatch falls back to the
/// generated-branch path exactly as before this bead).
fn resolve_drive_pr_head_branch(
    scm: &dyn Scm,
    cfg: &Config,
    bead: &Bead,
    resolved_repo: &str,
) -> dispatch::DriveBranchDecision {
    use dispatch::DriveBranchDecision;
    let Some(ext_ref) = bead.external_ref.as_deref() else {
        return DriveBranchDecision::Generated;
    };
    let Some((owner_repo, num_str)) = parse_external_ref(ext_ref) else {
        return DriveBranchDecision::Generated;
    };
    if owner_repo != resolved_repo {
        return DriveBranchDecision::Generated;
    }
    if cfg.resolve_repo(&owner_repo).is_none() {
        return DriveBranchDecision::Generated;
    }
    let Ok(pr_num) = num_str.parse::<u64>() else {
        return DriveBranchDecision::Generated;
    };
    match scm.open_pr_head_ref_for_repo(&owner_repo, pr_num) {
        Ok(PrHeadBranch::SameRepo(head_ref)) => DriveBranchDecision::PrHead(head_ref),
        Ok(PrHeadBranch::Fork) => DriveBranchDecision::ForkFallback,
        Ok(PrHeadBranch::NotFound) | Err(_) => DriveBranchDecision::Generated,
    }
}

fn post_scm_comment_by_bead_id(
    deps: &TickDeps,
    bead_id: &str,
    body: &str,
) -> Result<(), DaemonError> {
    if let Some(overlay) = deps.store.load(bead_id)? {
        if let Some(pr) = overlay.pr_number {
            // jleechan-9xrs Stage D: was `deps.cfg.target_repo` — escalation
            // (and every other caller of this function, including gate
            // failure / HUMAN_HELD comments) must target the bead's OWN
            // resolved repo or the comment lands on the wrong repo's PR
            // entirely (the twa0/mdgr cross-repo escalation class this
            // stage closes). `overlay` is already loaded here, so
            // `overlay.repo(cfg)` is free.
            let ext_ref = format!("{}#{}", overlay.repo(deps.cfg), pr);
            return deps.tracker.comment_external(&ext_ref, body);
        }
    }
    let candidates = deps.tracker.fetch_candidates()?;
    if let Some(bead) = candidates.iter().find(|b| b.id == bead_id) {
        if let Some(ref ext_ref) = bead.external_ref {
            return deps.tracker.comment_external(ext_ref, body);
        }
    }
    Err(DaemonError::Config(format!(
        "no SCM comment target found for bead {bead_id}"
    )))
}
// === jleechan-rouf dedup helpers (added by clean replay of PR #470 / cb2136ffe) ===
// PR #472 (origin/main) removed the FNV-1a hash helpers and the
// `escalation_dedup_should_emit` / `record_escalation_emit_dedup` wrappers
// during the vendor-outage refactor. The clean replay re-introduces them
// in their `bc6ef0c36` shape so the `adoption_branch_collision` call-site
// can re-key dedup on the branch (cb2136ffe's fix).
//
// Pinned by `mod escalation_context_hash_tests` at the bottom of this file
// (FNV-1a reference-charter constants + cross-process determinism contract).

/// FNV-1a 64-bit hash per the FNV reference charter.
/// OFFSET_BASIS = 0xcbf29ce484222325, PRIME = 0x100000001b3.
/// Non-cryptographic, intentionally cheap; needs to be deterministic
/// across processes and Rust versions, which `std::hash::DefaultHasher`
/// is NOT (per-process random `RandomState` seed).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Canonical-JSON byte form per RFC 8789 (JCS-lite): keys at every
/// nesting level sorted lexicographically. `serde_json::to_string`
/// preserves `serde_json::json!` macro insertion order, so without this
/// sort, two structurally-identical JSON values built with different
/// field declaration orders produce different hashes.
pub fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    use serde_json::Value;
    match value {
        Value::Null => b"null".to_vec(),
        Value::Bool(b) => b.to_string().into_bytes(),
        Value::Number(n) => n.to_string().into_bytes(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default().into_bytes(),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len() * 8);
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(&canonical_json_bytes(item));
            }
            out.push(b']');
            out
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = Vec::with_capacity(entries.len() * 16);
            out.push(b'{');
            for (i, (key, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(&serde_json::to_string(key).unwrap_or_default().into_bytes());
                out.push(b':');
                out.extend_from_slice(&canonical_json_bytes(val));
            }
            out.push(b'}');
            out
        }
    }
}

/// Stable 16-char lowercase-hex context hash for the escalation dedup
/// ledger. `format!("{:016x}", fnv1a_64(&canonical_json_bytes(value)))`.
/// Same format as the legacy `DefaultHasher`-based impl (also 16 hex
/// chars) so callers comparing against pre-fix ledger rows keep working.
pub fn escalation_context_hash(context: &serde_json::Value) -> String {
    let canonical = canonical_json_bytes(context);
    format!("{:016x}", fnv1a_64(&canonical))
}

/// Wall-clock epoch in seconds. Duplicates the helper in `crate::reroll`
/// (where it is private) so `tick.rs` can compute `now_epoch` without
/// widening the cross-module surface. If the two ever drift, the
/// `escalations_suppressed` field's docstring + the test
/// `escalation_refire_secs_default_is_3600` will surface the gap.
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Wrap `StateStore::escalation_should_emit` with the context-hash
/// computation the call-site needs. Returns `(should_emit, ctx_hash)` so
/// the caller can pass `ctx_hash` straight into
/// `record_escalation_emit_dedup` without re-computing it (and risking
/// drift between the dedup check and the ledger write).
///
/// Bead jleechan-rouf: the production caller passes the BRANCH as
/// `bead_id` so all colliding beads for the same branch collapse to one
/// ledger row. Pass the actual bead_id only when you genuinely want
/// per-bead dedup.
fn escalation_dedup_should_emit(
    deps: &TickDeps,
    bead_id: &str,
    reason: &str,
    context: &serde_json::Value,
    now_epoch: u64,
) -> Result<(bool, String), DaemonError> {
    let context_hash = escalation_context_hash(context);
    let should = deps.store.escalation_should_emit(
        bead_id,
        reason,
        &context_hash,
        now_epoch,
        deps.cfg.escalation_refire_secs,
    )?;
    Ok((should, context_hash))
}

/// Wrap `StateStore::record_escalation_emit` with the same `(bead_id,
/// reason, context_hash, now_epoch)` shape so the call-site stays
/// symmetric with `escalation_dedup_should_emit` above. Returns the
/// `DaemonError` directly so the caller can `?`-propagate without
/// `map_err`.
fn record_escalation_emit_dedup(
    deps: &TickDeps,
    bead_id: &str,
    reason: &str,
    context_hash: &str,
    now_epoch: u64,
) -> Result<(), DaemonError> {
    deps.store
        .record_escalation_emit(bead_id, reason, context_hash, now_epoch)
}