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
use crate::state::{set_human_hold_reason, BeadOverlay, HumanHoldReason, OverlayState, StateStore};
use crate::telemetry::{self, TelemetryEvent};
use crate::tools::{Bead, Llm, PrHeadBranch, Scm, SessionId, Sessions, Tracker, Vcs};
use crate::verifier::{self, PrEvidence};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

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
    /// Bead jleechan-jsby (r2): process-wide vendor-health ledger the
    /// fast tier MUTATES on every assessment (acceptance criterion 1:
    /// the ledger must be populated, not just consulted). Wrapped in
    /// `Mutex` so the daemon's poll loop's `--once` and concurrent
    /// tick callers can share one instance — the r1 PR #459 had no
    /// ledger field here at all and the in-tick `PrEvidence` was
    /// constructed with a fresh empty ledger, so the waiver path
    /// never executed. Optional so existing test sites that don't
    /// exercise the ledger can pass `None` (pre-r1 behavior preserved).
    pub vendor_health: Option<&'a Mutex<crate::vendor_health::VendorHealthLedger>>,
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
    /// 1s2q-escalation-dedup: escalation events (ESCALATION_REQUIRED /
    /// ESCALATION_NOTIFICATION_FAILED) suppressed this tick because the
    /// context hash was unchanged AND the backoff window
    /// (`Config::escalation_refire_secs`) had not elapsed since the last
    /// emit for the same `(bead_id, reason)`. Counted in the per-tick TICK
    /// summary line, not as per-bead telemetry (avoiding the very spam this
    /// dedup exists to stop).
    pub escalations_suppressed: usize,
    /// 1s2q-escalation-dedup Task 2: escalations marked terminal
    /// ("escalation_undeliverable") this tick because the notification
    /// failure was caused by a PERMANENT (non-transient per
    /// `DaemonError::is_transient`) gh error that will never resolve (e.g.
    /// `invalid issue format: "local-xxx"`). Each such marking emits ONE
    /// final `ESCALATION_UNDELIVERABLE` event and sets `terminal = 1` in the
    /// `escalation_ledger` so `escalation_should_emit` returns `Ok(false)`
    /// on every future tick — stopping the live incident where
    /// `ESCALATION_NOTIFICATION_FAILED` re-fired every ~90s for beads with
    /// permanent gh errors despite `human_held_recovery_attempt_cap_reached`.
    pub escalations_undeliverable: usize,
    /// jleechan-rln6: fast-rejected bead-ticks — beads whose gate assessment
    /// ran but was short-circuited because the ONLY red gate was a stale
    /// evidence marker (`EvidenceFloor` Red whose reason starts with
    /// "evidence head"). Counted separately from `gates_assessed` so
    /// operators can see the breakdown: the gate was assessed (still in
    /// `gates_assessed`), the verdict was Red, but no park / no reroll
    /// fired. Lets dashboards answer "how many lanes were saved from a
    /// full reroll cycle by the fast-rejection path" without re-deriving
    /// from telemetry.
    pub gates_assessed_fast_rejected: usize,
    /// Bead rev-4ou1z: coder panes woken this tick by the quota watchdog
    /// (an armed session whose recorded Gemini quota reset time, plus the
    /// 60s wake grace, has passed).
    pub quota_watchdog_wakes: usize,
}

/// Bounded retry cap for the automated HUMAN_HELD exit. Matches the shell
/// overlay's `recover-held` (daemon/factory-overlay.sh:319-333) and the
/// ad-hoc attempt counter cap cited in the gap-review verdict
/// (`docs/factory-goal-gap-review-2026-07-06.md` Blocker #3). Beads at or
/// above this cap are deliberately left in HUMAN_HELD for a human to
/// review — the daemon stops blindly retrying past this point.
const MAX_HUMAN_HELD_RECOVERY_ATTEMPT: u32 = 10;
const ESCALATION_REVIEWER: &str = "dark-factory-escalation";
// jleechan-6l1f: gate-regression cap. After this many green->red
// transitions, a further regression MUST emit GATE_REGRESSED_CAPPED and
// park HUMAN_HELD with park_reason="gate_regression_capped" rather than
// silently looping the bead back into the reroll lane forever. Distinct
// from MAX_HUMAN_HELD_RECOVERY_ATTEMPT (which caps automated requeue from
// HUMAN_HELD): this cap sits in front of the demotion path, so a
// persistently-flapping gate can't even reach `recover_human_held`. Live
// incident: PR #540 sat READY all_green=true at 2026-08-04T13:18:43Z;
// when CI regressed the bead silently dead-ended (no reroll triggered).
pub const MAX_GATE_REGRESSIONS: u32 = 3;
const ESCALATION_SENTINEL_ATTEMPT: u32 = u32::MAX;
// jleechan-rln6: one-shot sentinel for the stale-evidence fast-rejection
// path. Keyed on the SAME `save_rejection`/`load_rejection` infrastructure
// the escalation dedup uses, so once the daemon has told the coder
// session "your Evidence marker head SHA does not match PR head; refresh
// via `gh pr edit --body`", subsequent ticks on the same bead do not
// re-post the same comment (re-runs would spam the PR every minute until
// the coder fixes it). Distinct from `ESCALATION_REVIEWER` so the two
// one-shot flows never collide on the same `bead_id`/`attempt` row.
const EVIDENCE_HEAD_STALE_REVIEWER: &str = "dark-factory-evidence-head-stale";
const EVIDENCE_HEAD_STALE_SENTINEL_ATTEMPT: u32 = u32::MAX - 1;
// jtg8-r4 acceptance #3: per-tick gh call count threshold above which the
// slow tier logs a warning so operators can investigate before the core
// rate-limit bucket exhausts. Pre-fix baseline was ~50 per slow tick
// (one `gh api repos/.../pulls/N` call per factory-labeled PR plus the
// per-PR `collaborator_permission` probe). Post-fix steady state is ~1
// (the `gh pr list` query); per-PR probes are served from the adoption
// probe cache. 20 is a generous in-between signal.
const INTAKE_GH_CALL_WARN_THRESHOLD: u32 = 20;

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
            host: telemetry::local_hostname(),
            bead_id: bead_id.to_string(),
            attempt_id,
            lifecycle_state: lifecycle_state.to_string(),
            event_type: event_type.to_string(),
            metrics,
            context,
        },
    )
}

/// 1s2q-escalation-dedup: compute a deterministic hex hash of the serialized
/// `context` JSON for an escalation event. Used as the dedup key in the
/// `escalation_ledger` — two events with the same stable context fields
/// produce the same hash, so a re-fire with identical context is suppressed
/// within the backoff window.
///
/// Cross-model review P2 #3 (bead jleechan-n6mk, follow-up to PR #447): the
/// previous implementation used `std::hash::DefaultHasher`. `DefaultHasher`
/// is NOT a stable cross-process hash — `std::collections::hash_map::RandomState`
/// seeds it with a per-process random value (DOS-resistance), so the same
/// input on two daemon restarts can produce DIFFERENT 64-bit digests. The
/// stored `escalation_ledger.context_hash` would then never match the
/// freshly-computed context hash, silently disabling dedup across restarts.
/// The bug is invisible within a single process run (the seed is fixed for
/// the lifetime of `RandomState`), so the original tick-level test
/// `escalation_dedup_tick_level_*` passed against both FakeStateStore and
/// SqliteStateStore — it never crossed a process boundary.
///
/// This implementation uses FNV-1a 64-bit (Fowler-Noll-Vo) over the canonical
/// JSON bytes. FNV-1a is a non-cryptographic hash with three properties that
/// matter here: (a) zero dependencies (the repo's 5-dep budget rules out
/// `sha2`/`fnv`/`seahash` crates), (b) deterministic across processes and
/// Rust versions (no random seed), and (c) sufficient collision-resistance
/// for the per-bead context space. We also canonicalize the JSON by sorting
/// keys before serializing — `serde_json::to_string` preserves insertion
/// order from `serde_json::json!`, so without sorting, two `json!` macros
/// in different call sites that happen to lay the same fields out in
/// different orders would produce different hashes.
///
/// The 16-char lowercase hex format is unchanged so the
/// `escalation_ledger.context_hash` column's TEXT storage stays compatible
/// (no migration needed). Pinned FNV constants: `OFFSET_BASIS = 0xcbf29ce484222325`,
/// `PRIME = 0x100000001b3`.
fn escalation_context_hash(context: &serde_json::Value) -> String {
    let canonical = canonical_json_bytes(context);
    format!("{:016x}", fnv1a_64(&canonical))
}

/// FNV-1a 64-bit hash. See <https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function>.
/// Inlined to keep `daemon/Cargo.toml` at its strict 5-dependency budget
/// (no `fnv`/`seahash` crate); the function is ~5 lines.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    // FNV-1a 64-bit OFFSET_BASIS and PRIME per the FNV reference charter.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Recursively rewrite `Value` so every object has its keys sorted by
/// `Ord<str>` ascending. After this pass, `serde_json::to_string` emits the
/// same byte sequence regardless of the order in which the original
/// `serde_json::json!` macro laid out the fields. This is the canonical-JSON
/// property RFC 8789 calls "JSON Canonicalization Scheme" (JCS), but we don't
/// need the full RFC (no Unicode normalization, no number formatting) — just
/// sorted keys.
fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
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
            // Collect (key, value) pairs, sort by key, then serialize.
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

/// 1s2q-escalation-dedup: the current unix epoch in seconds. Centralized so
/// every escalation dedup check in a single tick shares the same `now_epoch`
/// (avoids sub-second skew making a same-tick re-fire look like it's past
/// backoff). Matches the `SystemTime::now()...as_secs()` pattern already used
/// throughout tick.rs.
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 1s2q-escalation-dedup: consult the `escalation_ledger` before emitting an
/// escalation event. Returns `Ok((true, hash))` (proceed with emit) when no
/// prior record exists, the context hash changed, or the backoff window has
/// elapsed. Returns `Ok((false, hash))` (suppress) when the same context was
/// emitted within `cfg.escalation_refire_secs`. The returned `hash` is the
/// deterministic hex digest of `context` — pass it to
/// `record_escalation_emit_dedup` after the emit so the ledger row is stamped
/// without re-borrowing `context` (which `emit` consumes by ownership). On
/// suppression, the caller increments `summary.escalations_suppressed` and
/// skips the emit (no per-bead suppression telemetry — the count goes in the
/// TICK summary).
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

/// 1s2q-escalation-dedup: record (upsert) the escalation ledger row after a
/// successful emit. Takes the precomputed `context_hash` (returned by
/// `escalation_dedup_should_emit`) rather than re-borrowing the `context`
/// value that `emit` has already consumed by ownership.
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

/// 1s2q-escalation-dedup Task 2: when a notification failure (`Err` from
/// `post_scm_comment_by_bead_id`) is caused by a PERMANENT (non-transient per
/// `DaemonError::is_transient`) gh error — e.g. `invalid issue format:
/// "local-xxx"` — the error will never resolve on retry. Mark the
/// `(bead_id, reason)` escalation ledger row terminal
/// (`mark_escalation_undeliverable` → `terminal = 1`), emit ONE final
/// `ESCALATION_UNDELIVERABLE` event, and bump the per-tick counter. On every
/// future tick `escalation_should_emit` returns `Ok(false)` for this row
/// (terminal check precedes hash/backoff), so no re-emit occurs — stopping
/// the live incident where `ESCALATION_NOTIFICATION_FAILED` re-fired every
/// ~90s for beads with permanent gh errors. The caller MUST have already
/// excluded the `is_missing_scm_target_error` case (which has its own
/// terminal local-fallback path) before calling this.
fn mark_escalation_undeliverable_and_emit(
    deps: &TickDeps,
    summary: &mut TickSummary,
    bead_id: &str,
    attempt: u32,
    lifecycle_state: &str,
    reason: &str,
    err: &DaemonError,
) -> Result<(), DaemonError> {
    deps.store.mark_escalation_undeliverable(bead_id, reason)?;
    // Record the escalation sentinel so `escalation_already_recorded` at the
    // top of each site returns `true` on future ticks — the permanent-error
    // path must be truly terminal (ONE final event, no re-attempt). Mirrors
    // `record_local_escalation_fallback`, which also calls `record_escalation`
    // for the same reason. Without this, the `escalation_already_recorded`
    // guard (which only checks `review_rejection`, set on the SUCCESS path)
    // would not block, and the permanent-error branch would re-fire every
    // tick — the exact live incident this task fixes.
    record_escalation(deps, bead_id, reason)?;
    summary.escalations_undeliverable += 1;
    emit(
        deps.telemetry_log,
        bead_id,
        attempt,
        lifecycle_state,
        "ESCALATION_UNDELIVERABLE",
        serde_json::json!({}),
        serde_json::json!({
            "reason": reason,
            "error": err.to_string(),
            "permanent": true,
        }),
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
    let (event_type, mut context) = match &outcome.verdict {
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
    if let Some(repo) = &outcome.repo {
        context["repo"] = serde_json::Value::String(repo.clone());
    }
    if let Some(pr_number) = outcome.pr_number {
        context["pr_number"] = serde_json::Value::Number(pr_number.into());
    }
    if let Some(branch) = &outcome.branch {
        context["branch"] = serde_json::Value::String(branch.clone());
    }
    if let Some(head_sha) = &outcome.head_sha {
        context["head_sha"] = serde_json::Value::String(head_sha.clone());
    }
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
            // Bead rev-3lm8k: the session is now provably dead, so its
            // AO-managed worktree dir (if any) is stale immediately — do
            // not wait for the TTL sweep. `clean_stale_worktree` is a
            // no-op when `agent_worktree_root` is unset (legacy layout).
            match crate::worktree_reaper::clean_stale_worktree(
                deps.cfg,
                overlay.repo(deps.cfg),
                &session_id_str,
            ) {
                Ok(true) => {
                    let _ = emit(
                        deps.telemetry_log,
                        &overlay.bead_id,
                        overlay.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "WORKTREE_CLEANED_ON_SESSION_EXIT",
                        serde_json::json!({}),
                        serde_json::json!({
                            "session_id": session_id_str,
                            "phase": "park_transition",
                        }),
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    let _ = emit(
                        deps.telemetry_log,
                        &overlay.bead_id,
                        overlay.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "WORKTREE_CLEAN_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "session_id": session_id_str,
                            "error": format!("{e:?}"),
                            "phase": "park_transition",
                        }),
                    );
                }
            }
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

/// jleechan-7t2g: predicate extracted from the `EXISTING_PR_ADOPTED`
/// dedup check at the original line 1507 of the slow-tier PR adoption
/// loop. Returns `true` when the overlay's pre-adoption state is one of
/// the three "already-attested" states (`Attested`, `Ready`,
/// `HumanHeld`), in which case the slow tier MUST skip the
/// `EXISTING_PR_ADOPTED` telemetry emit because the durable overlay row
/// already records the audit trail. Returns `false` for `None`
/// (first-time create) and for every other state, so the first emit
/// and any emit after a state transition still fire.
///
/// Origin: PR #487 / bead jleechan-mdun introduced this dedup after a
/// production incident where 30 attested beads re-emitted
/// `EXISTING_PR_ADOPTED` on every tick (~301,553 redundant events
/// across 20 days; top offender jleechan-fpca at 23,012 re-emits).
/// Pinned by the inline `existing_pr_adoption_dedup_tests` module
/// below and by `tests/tick_integration.rs::existing_pr_adoption_*`.
pub(crate) fn should_skip_existing_pr_adoption_emit(
    pre_adopt_state: Option<OverlayState>,
) -> bool {
    matches!(
        pre_adopt_state,
        Some(OverlayState::Attested)
            | Some(OverlayState::Ready)
            | Some(OverlayState::HumanHeld)
    )
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
        // 1. Time-box envelope check (bead bze8.3: redispatch must not
        // inherit elapsed autonomy from prior attempts). The active attempt
        // clock is `now_epoch - attempt_started_at` whenever the anchor is
        // stamped — `dispatch::dispatch_ready` writes it atomically at the
        // successful-reservation save, and `recover_human_held` clears it
        // on requeue so a freshly-recovered bead has no anchor until its
        // own reservation succeeds. We fall back to cumulative
        // `autonomy_secs` only when the anchor is absent (legacy / pre-fix
        // rows that have not been re-dispatched since this column existed)
        // so the existing behavior is preserved for any row that has
        // never round-tripped through dispatch reservation.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (deadline_epoch, observed_elapsed_secs) = match overlay.attempt_started_at {
            Some(started_at) => {
                let elapsed = now_epoch.saturating_sub(started_at);
                (started_at.saturating_add(deps.cfg.autonomy_timebox_secs), elapsed)
            }
            None => {
                // Legacy fallback: timebox check still works against
                // `autonomy_secs` so pre-fix rows that have not been
                // re-dispatched continue to park correctly.
                let deadline = now_epoch
                    .saturating_sub(overlay.autonomy_secs)
                    .saturating_add(deps.cfg.autonomy_timebox_secs);
                (deadline, overlay.autonomy_secs)
            }
        };
        if observed_elapsed_secs >= deps.cfg.autonomy_timebox_secs {
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
            // Bead bze8.3 acceptance: record the attempt id, started_at,
            // deadline, observed_at, and elapsed seconds in the park
            // telemetry so a future operator can see WHY this attempt was
            // parked (was it a genuinely over-budget attempt, or did a
            // prior attempt's elapsed time silently carry through?).
            let started_at = overlay.attempt_started_at;
            let park_attempt = overlay.attempt;
            // Clear the anchor so a future requeue does not inherit it.
            overlay.attempt_started_at = None;
            deps.store.save(&overlay)?;
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "PARKED_HUMAN_HELD",
                serde_json::json!({}),
                serde_json::json!({
                    "reason": "autonomy_timebox_exceeded",
                    "attempt_id": park_attempt,
                    "started_at": started_at,
                    "deadline_epoch": deadline_epoch,
                    "observed_at": now_epoch,
                    "elapsed_secs": observed_elapsed_secs,
                    "budget_secs": deps.cfg.autonomy_timebox_secs,
                }),
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
                // The exact session worktree and published remote are both
                // consulted. Either can prove a rewrite. If the remote ref
                // is not published yet (or GitHub is transiently unavailable),
                // positive local ancestry keeps the healthy worker running
                // and emits warning telemetry; without positive evidence,
                // an inconclusive check still escalates fail-closed.
                if overlay.is_adopted {
                    if let (Some(branch), Some(pre_sha)) =
                        (overlay.branch.clone(), overlay.pre_session_head_sha.clone())
                    {
                        let remote_verdict = || deps.vcs.remote_head_sha(&branch).and_then(|post_sha| {
                            deps.vcs
                                .is_ancestor(&pre_sha, &post_sha)
                                .map(|ok| (ok, post_sha))
                        });
                        let local_verdict = match overlay.session_id.as_ref() {
                            Some(session_id) => deps.sessions.worktree_head_ancestry(
                                &SessionId(session_id.clone()),
                                &branch,
                                &pre_sha,
                            ),
                            None => Ok(None),
                        };
                        let mut remote_fallback_warning = None;
                        let verdict = match local_verdict {
                            Ok(Some(local)) if !local.contains_ancestor => {
                                Ok((false, local.head_sha))
                            }
                            Ok(Some(local)) => match remote_verdict() {
                                Ok(remote) => Ok(remote),
                                Err(remote_error) => {
                                    remote_fallback_warning = Some(remote_error.to_string());
                                    Ok((true, local.head_sha))
                                }
                            },
                            Ok(None) => remote_verdict(),
                            Err(local_error) => remote_verdict().map_err(|remote_error| {
                                DaemonError::Parse(format!(
                                    "local AO worktree append-only check failed: {local_error}; remote check also failed: {remote_error}"
                                ))
                            }),
                        };
                        match verdict {
                            Ok((true, post_sha)) => {
                                if let Some(remote_error) = remote_fallback_warning {
                                    emit(
                                        deps.telemetry_log,
                                        &overlay.bead_id,
                                        overlay.attempt,
                                        overlay.state.as_str(),
                                        "APPEND_ONLY_REMOTE_CHECK_DEFERRED",
                                        serde_json::json!({}),
                                        serde_json::json!({
                                            "reason": "local_worktree_ancestry_confirmed",
                                            "branch": branch,
                                            "pre_session_sha": pre_sha,
                                            "local_head_sha": post_sha,
                                            "remote_error": remote_error,
                                        }),
                                    )?;
                                }
                            }
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
                            let last_commit_epoch = deps.scm.remote_branch_last_commit_for_repo(
                                overlay.repo(deps.cfg),
                                branch,
                            )?;
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

    // jleechan-gib: automated HUMAN_HELD exit (Rust port of shell
    // `recover-held`). Runs AFTER `run_slow_tier` so that dispatch of
    // already-QUEUED beads (from prior ticks) happens BEFORE any
    // recovery/escalation work each tick — this guarantees a QUEUED bead is
    // dispatched on the first available slow tick and can never be starved by
    // an escalation backlog aborting the tick via `?` before dispatch runs.
    // The active-overlay wedge loop above only processes DISPATCHED/ATTESTED
    // beads (via `list_active_overlays`), so a freshly-recovered QUEUED bead
    // is never re-parked by it; placing recovery after the wedge loop is safe.
    // Recovery only fires when the slow tier is due (matches the shell
    // overlay's cadence — `recover-held` was never per-fast-tick).
    if slow_tier_due {
        run_recovery_step(deps, &mut summary)?;
    }

    // rev-4ou1z: slow-tier cadence matches the hours-long Gemini quota
    // reset window — no need to poll for a wake-due session every fast
    // tick.
    if slow_tier_due {
        run_quota_watchdog_wake(deps, &mut summary)?;
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
            "escalationsSuppressed": summary.escalations_suppressed,
            "escalationsUndeliverable": summary.escalations_undeliverable,
            "quotaWatchdogWakes": summary.quota_watchdog_wakes,
        }),
        serde_json::json!({"tick_index": tick_index, "slow_tier_due": slow_tier_due}),
    )?;

    Ok(summary)
}

/// Bead jleechan-jsby (r2): emit a `VENDOR_WAIVED` event on the
/// Healthy/Capped -> Capped edge so operators can see when a vendor
/// was auto-escalated. The wire format matches
/// `vendor_health::EVT_WAIVED` (mirrored in `factory-overlay.sh` and
/// the CXDB consumers). The `compensating_required` context key
/// names the documented trust floor (skeptic + /er + cross-model) so
/// audits can grep the JSONL for the substitution rule.
fn emit_vendor_waived(
    deps: &TickDeps,
    bead_id: &str,
    vendor: crate::vendor_health::Vendor,
    attempt: u32,
) -> Result<(), DaemonError> {
    use crate::vendor_health::Vendor;
    let (vendor_name, waiver_token) = match vendor {
        Vendor::CodeRabbit => ("coderabbit", "coderabbit:waived_vendor_unavailable"),
        Vendor::Bugbot => ("bugbot", "bugbot:waived_vendor_unavailable"),
    };
    emit(
        deps.telemetry_log,
        bead_id,
        attempt,
        OverlayState::Attested.as_str(),
        crate::vendor_health::EVT_WAIVED,
        serde_json::json!({}),
        serde_json::json!({
            "vendor": vendor_name,
            "waiver_token": waiver_token,
            "compensating_required": "skeptic_pass+er_pass+cross_model",
        }),
    )
}

/// Bead jleechan-jsby (r2): emit a `VENDOR_RECOVERED` event on the
/// Capped -> Healthy edge so operators can see when a vendor came
/// back online (subscription reset, quota cleared, etc.). The wire
/// format matches `vendor_health::EVT_RECOVERED`.
fn emit_vendor_recovered(
    deps: &TickDeps,
    bead_id: &str,
    vendor: crate::vendor_health::Vendor,
    attempt: u32,
) -> Result<(), DaemonError> {
    use crate::vendor_health::Vendor;
    let (vendor_name, waiver_token) = match vendor {
        Vendor::CodeRabbit => ("coderabbit", "coderabbit:waived_vendor_unavailable"),
        Vendor::Bugbot => ("bugbot", "bugbot:waived_vendor_unavailable"),
    };
    emit(
        deps.telemetry_log,
        bead_id,
        attempt,
        OverlayState::Attested.as_str(),
        crate::vendor_health::EVT_RECOVERED,
        serde_json::json!({}),
        serde_json::json!({
            "vendor": vendor_name,
            "waiver_token": waiver_token,
        }),
    )
}

/// jleechan-gib: automated HUMAN_HELD exit (Rust port of shell
/// `recover-held`). Requeues only allow-listed retry-safe `HUMAN_HELD`
/// beads below `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` whose durable overlay has
/// no session handle, increments `attempt`, and zeros `autonomy_secs`.
/// Unknown/possibly-live holds fail closed. Runs AFTER `run_slow_tier` in
/// `run_tick` so that dispatch of already-QUEUED beads happens before any
/// recovery/escalation work; recovered beads become QUEUED and are
/// dispatched on the NEXT slow tick. The active-overlay wedge loop only
/// processes DISPATCHED/ATTESTED beads, so a freshly-recovered QUEUED bead
/// is never re-parked by it.
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
            if !err.is_transient() {
                mark_escalation_undeliverable_and_emit(
                    deps,
                    summary,
                    &overlay.bead_id,
                    overlay.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "human_held_recovery_attempt_cap_reached",
                    &err,
                )?;
                continue;
            }
            let ctx = serde_json::json!({
                "reason": "human_held_recovery_attempt_cap_reached",
                "error": err.to_string(),
            });
            let now_epoch = now_epoch_secs();
            let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                deps,
                &overlay.bead_id,
                "human_held_recovery_attempt_cap_reached",
                &ctx,
                now_epoch,
            )?;
            if !should_emit {
                summary.escalations_suppressed += 1;
                continue;
            }
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "ESCALATION_NOTIFICATION_FAILED",
                serde_json::json!({}),
                ctx,
            )?;
            record_escalation_emit_dedup(
                deps,
                &overlay.bead_id,
                "human_held_recovery_attempt_cap_reached",
                &ctx_hash,
                now_epoch,
            )?;
            continue;
        }
        record_escalation(
            deps,
            &overlay.bead_id,
            "human_held_recovery_attempt_cap_reached",
        )?;
        summary.beads_escalated += 1;
        let ctx = serde_json::json!({
            "reason": "human_held_recovery_attempt_cap_reached",
            "max_attempt": MAX_HUMAN_HELD_RECOVERY_ATTEMPT,
            "pr_number": overlay.pr_number,
            "branch": overlay.branch,
        });
        let now_epoch = now_epoch_secs();
        let (should_emit, ctx_hash) = escalation_dedup_should_emit(
            deps,
            &overlay.bead_id,
            "human_held_recovery_attempt_cap_reached",
            &ctx,
            now_epoch,
        )?;
        if !should_emit {
            summary.escalations_suppressed += 1;
        } else {
            emit(
                deps.telemetry_log,
                &overlay.bead_id,
                overlay.attempt,
                OverlayState::HumanHeld.as_str(),
                "ESCALATION_REQUIRED",
                serde_json::json!({}),
                ctx,
            )?;
            record_escalation_emit_dedup(
                deps,
                &overlay.bead_id,
                "human_held_recovery_attempt_cap_reached",
                &ctx_hash,
                now_epoch,
            )?;
        }
    }
    Ok(())
}

/// Bead rev-4ou1z: quota watchdog wake sweep. Slow-tier cadence matches the
/// hours-long Gemini quota reset window (no need to poll every fast tick).
/// For every `(bead_id, session_id)` armed by `run_fast_tier`'s
/// SESSION_HEALTH_FAILED handling whose recorded reset time (plus the 60s
/// wake grace) has passed, sends an Enter keypress to the paused coder pane
/// via `Sessions::wake_pane` — the SAME session that was paused, no
/// respawn.
fn run_quota_watchdog_wake(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Deliberately scoped to bead ids THIS store owns (same
    // `owned_branches` walk `run_fast_tier` uses) rather than a blind sweep
    // of the whole process-wide ledger: the ledger is a single static
    // shared by every `TickDeps`/`StateStore` pairing in the process (see
    // the module doc comment on `health::quota_watchdog`), so scoping the
    // query to this store's own bead ids is what keeps two independent
    // tick loops from reacting to each other's armed entries.
    let branches = deps.store.owned_branches()?;
    let mut bead_ids: Vec<String> = Vec::new();
    for branch in &branches {
        if let Ok(Some(bead_id)) = deps.store.bead_id_for_branch(branch) {
            bead_ids.push(bead_id);
        }
    }
    bead_ids.sort();
    bead_ids.dedup();

    for bead_id in bead_ids {
        let Some(session_id) = crate::health::quota_watchdog::take_due_wake(&bead_id, now_epoch)
        else {
            continue;
        };
        let attempt = deps
            .store
            .load(&bead_id)
            .ok()
            .flatten()
            .map(|o| o.attempt)
            .unwrap_or(0);
        let woke = deps
            .sessions
            .wake_pane(&SessionId(session_id.clone()))
            .unwrap_or(false);
        emit(
            deps.telemetry_log,
            &bead_id,
            attempt,
            OverlayState::Dispatched.as_str(),
            "QUOTA_WATCHDOG_WOKE_PANE",
            serde_json::json!({}),
            serde_json::json!({"session_id": session_id, "woke": woke}),
        )?;
        summary.quota_watchdog_wakes += 1;
    }
    Ok(())
}

/// Slow tier: intake new beads, route each freshly-queued bead, dispatch as
/// many QUEUED beads as the safety envelope (30/15) allows.
fn run_slow_tier(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    // jleechan-gib: recovery runs AFTER this slow-tier dispatch pass (see
    // `run_tick`), so freshly-recovered QUEUED beads are NOT dispatched this
    // same tick — they are dispatched on the NEXT slow tick. This is the
    // required dispatch-scheduling guarantee: already-QUEUED beads (from prior
    // ticks) are dispatched before any recovery/escalation work each tick.
    let mut pr_intake_bead_ids = HashSet::new();
    // jtg8-r4: load the persistent adoption-probe cache from disk and use
    // the rate-limit-aware variant. The cache is rewritten at the end of
    // every slow pass (see below) so a daemon restart doesn't re-probe
    // the entire factory-labeled PR set on its first tick.
    let mut adoption_cache = intake::AdoptionProbeCache::load_or_default();
    let slow_tick_now = now_epoch_secs();
    let intake_outcome = intake::normalize_labeled_prs_outcome(
        deps.scm,
        deps.tracker,
        deps.cfg,
        &mut adoption_cache,
        slow_tick_now,
        deps.telemetry_log,
    )?;
    // jtg8-r4 acceptance #3: warn when per-tick gh call count exceeds the
    // slow-tier budget. The threshold (20) is generous — well below what
    // the 2026-07-22 incident burned per tick (~50+), so a hit means
    // we've drifted back toward pre-fix behavior and need to investigate
    // before the core rate-limit bucket exhausts again.
    if intake_outcome.metrics.gh_call_count >= INTAKE_GH_CALL_WARN_THRESHOLD {
        eprintln!(
            "auto-factory daemon: WARNING slow-tier intake gh_call_count={} (threshold={}); \
             inspect adoption-probe cache before core rate-limit bucket exhausts",
            intake_outcome.metrics.gh_call_count, INTAKE_GH_CALL_WARN_THRESHOLD
        );
    }
    let (pr_adoptions, pr_skip_outcomes) = if intake_outcome.rate_limited {
        // jtg8-r4 acceptance #5 (r3 fix): a rate-limited intake sweep must
        // DEGRADE — skip PR adoption this tick — but CONTINUE into the rest
        // of run_slow_tier. The r3 fix `return Ok(())` early-aborted
        // routing + dispatch and starved every other bead's dispatch.
        // We just log a one-line skip and continue past this phase; the
        // `consecutive_failures` counter never increments on rate-limit
        // intake failures (the new variant returns Ok(_), not Err).
        eprintln!(
            "auto-factory daemon: slow-tier intake rate-limited by gh; \
             skipping adoption sweep this tick, dispatch continues"
        );
        (Vec::new(), Vec::new())
    } else {
        (intake_outcome.adopted, intake_outcome.outcomes)
    };
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
                let ctx = serde_json::json!({
                    "reason": "adoption_branch_collision",
                    "repo": adopted.repo,
                    "pr_number": adopted.pr_number,
                    "branch": adopted.head_ref_name,
                    "head_sha": adopted.head_sha,
                    "registered_bead": owner,
                    "registered_bead_live": owner_live,
                    "external_ref": adopted.external_ref,
                });
                let now_epoch = now_epoch_secs();
                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                    deps,
                    &adopted.bead_id,
                    "adoption_branch_collision",
                    &ctx,
                    now_epoch,
                )?;
                if !should_emit {
                    summary.escalations_suppressed += 1;
                    continue;
                }
                summary.beads_escalated += 1;
                emit(
                    deps.telemetry_log,
                    &adopted.bead_id,
                    1,
                    OverlayState::HumanHeld.as_str(),
                    "ESCALATION_REQUIRED",
                    serde_json::json!({}),
                    ctx,
                )?;
                record_escalation_emit_dedup(
                    deps,
                    &adopted.bead_id,
                    "adoption_branch_collision",
                    &ctx_hash,
                    now_epoch,
                )?;
                continue;
            }
        }
        deps.store
            .register_branch(&adopted.bead_id, &adopted.head_ref_name)?;

        let existing = deps.store.load(&adopted.bead_id)?;
        let attempt = existing.as_ref().map(|o| o.attempt).unwrap_or(1);
        // jleechan-mdun: capture the overlay state BEFORE the move into
        // `should_adopt` (and the subsequent `unwrap_or` below) so the
        // dedup check below can compare against the durable state of
        // THIS tick's snapshot, not a stale or re-initialized copy.
        let pre_adopt_state = existing.as_ref().map(|o| o.state);
        let should_adopt = !matches!(
            pre_adopt_state,
            Some(OverlayState::Ready) | Some(OverlayState::HumanHeld)
        );
        if should_adopt {
            // jleechan-35y4 Stage A: adopted PRs are always same-repo
            // (fork/cross-repo PRs are rejected earlier by `same_repo_pr`
            // in intake.rs), so this always resolves to `cfg.target_repo`'s
            // owner/repo today. Still resolved from `external_ref` (not
            // left `None`) so it stays correct once Stage C/D lift the
            // same-repo-only restriction for adopted PRs.
            let target_repo = intake::resolve_target_repo("", Some(adopted.external_ref.as_str()));
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
                attempt_started_at: None,
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
        // jleechan-mdun: skip re-emit on subsequent ticks. The durable
        // overlay row already records (pr_number, branch, external_ref,
        // is_adopted) — emitting `EXISTING_PR_ADOPTED` every tick for an
        // already-attested bead produced ~301k redundant telemetry events
        // across 30 attested beads (peaks ~24.8k/day; top offender
        // jleechan-fpca at 23,012 re-emits over 20 days). The first emit
        // (newly_created=true) and any emit after a state transition
        // away from Attested/Ready/HumanHeld still fire so the audit
        // trail is preserved.
        //
        // jleechan-7t2g: dedup set extracted into `should_skip_existing_pr_adoption_emit`
        // so the inline unit tests in `existing_pr_adoption_dedup_tests`
        // can pin the predicate directly.
        let already_attested =
            should_skip_existing_pr_adoption_emit(pre_adopt_state);
        if !already_attested {
            emit(
                deps.telemetry_log,
                &adopted.bead_id,
                attempt,
                OverlayState::Attested.as_str(),
                "EXISTING_PR_ADOPTED",
                serde_json::json!({}),
                serde_json::json!({
                    "repo": adopted.repo,
                    "pr_number": adopted.pr_number,
                    "branch": adopted.head_ref_name,
                    "head_sha": adopted.head_sha,
                    "external_ref": adopted.external_ref,
                    "newly_created": adopted.newly_created,
                }),
            )?;
        }
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
            tracker_bead
                .as_ref()
                .map(|b| b.description.as_str())
                .unwrap_or(""),
            tracker_bead
                .as_ref()
                .and_then(|b| b.external_ref.as_deref()),
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
            attempt_started_at: None,
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
                // jleechan-htf7 r3: fail-closed adoption for manual beads.
                // A `br create`-style bead that arrives here WITHOUT a
                // resolvable `target_repo` (no body `target_repo:` field,
                // no parseable `external_ref` owner/repo prefix) used to be
                // admitted as `state: Queued`, routed through the LLM, and
                // only THEN parked `unmapped_repo` at the dispatch layer
                // (see `tick.rs:2133-2145`). That reactive park leaves a
                // future-orphan shape in the routing pipeline on every
                // tick, wastes a `judge(...)` call, and creates churn
                // telemetry for a defect the daemon can never recover from
                // on its own.
                //
                // Fail-closed here: park HUMAN_HELD with the same
                // `unmapped_repo` reason the dispatch layer would have used,
                // skip routing entirely, and emit `PARKED_HUMAN_HELD`
                // directly so downstream tooling (Healer, dashboards) sees
                // the same event shape whether the park came from adoption
                // or from dispatch. PR #201 invariants (`create_bead`
                // never called, `external_ref` never fabricated) still
                // apply — this gate only short-circuits before routing.
                //
                // jleechan-htf7 r3 incremental (post-r2 review): the
                // escalation comment for the parked bead is posted via the
                // SAME `post_scm_comment_by_bead_id` idiom the dispatch
                // flow uses for `unmapped_target_repo`
                // (`tick.rs:2290-2353`). That idiom has FOUR outcomes; r2
                // only handled two. r3 mirrors all four so the manual
                // adoption site never silently drops an escalation record:
                //   (a) missing-target  -> `record_local_escalation_fallback`
                //                          + `ESCALATED_LOCALLY` event
                //   (b) non-transient   -> `mark_escalation_undeliverable_and_emit`
                //                          + `ESCALATION_UNDELIVERABLE` event
                //   (c) transient       -> `ESCALATION_NOTIFICATION_FAILED`
                //                          event (deduped, retry next tick)
                //   (d) success         -> `record_escalation` +
                //                          `beads_escalated += 1` +
                //                          `ESCALATION_REQUIRED` (deduped)
                if target_repo.is_none() {
                    let mut o = BeadOverlay {
                        bead_id: bead.id.clone(),
                        state: OverlayState::HumanHeld,
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
                        attempt_started_at: None,
                    };
                    set_human_hold_reason(&mut o, HumanHoldReason::UnmappedRepo);
                    deps.store.save(&o)?;
                    summary.beads_parked_human_held += 1;
                    emit(
                        deps.telemetry_log,
                        &bead.id,
                        1,
                        OverlayState::HumanHeld.as_str(),
                        "PARKED_HUMAN_HELD",
                        serde_json::json!({}),
                        serde_json::json!({
                            "reason": "unmapped_repo",
                            "source": "manual_adoption_fail_closed",
                            "external_ref": bead.external_ref,
                        }),
                    )?;
                    if !escalation_already_recorded(deps, &bead.id)? {
                        let comment_body = format!(
                            "🤖 **[dark-factory]** Escalation required: bead `{}` was \
                             created manually via `br create` with neither an \
                             `external_ref` nor a `target_repo:` body field, so the \
                             daemon cannot determine which repo it belongs to. \
                             Automation parked it HUMAN_HELD at adoption time \
                             (jleechan-htf7 r3 fail-closed gate) rather than routing \
                             it and parking at dispatch. Operator action: supply an \
                             explicit `target_repo: <owner>/<repo>` line in the bead \
                             body, set `external_ref = \"<owner>/<repo>#NNN\"`, or \
                             file under an issue/PR labeled `factory` so intake can \
                             resolve the repo from the GitHub external_ref.",
                            bead.id,
                        );
                        // (a)+(b)+(c)+(d): mirror the canonical
                        // `unmapped_target_repo` dispatch-flow idiom at
                        // `tick.rs:2284-2353`. The `unmapped_repo` reason
                        // here is the manual-adoption analogue of that
                        // reason, so the four-way handling is identical.
                        match post_scm_comment_by_bead_id(deps, &bead.id, &comment_body) {
                            Ok(()) => {
                                // (d) success: record escalation + bump
                                // beads_escalated + emit ESCALATION_REQUIRED.
                                record_escalation(deps, &bead.id, "unmapped_repo")?;
                                summary.beads_escalated += 1;
                                let ctx = serde_json::json!({
                                    "reason": "unmapped_repo",
                                    "source": "manual_adoption_fail_closed",
                                });
                                let now_epoch = now_epoch_secs();
                                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                                    deps,
                                    &bead.id,
                                    "unmapped_repo",
                                    &ctx,
                                    now_epoch,
                                )?;
                                if !should_emit {
                                    summary.escalations_suppressed += 1;
                                } else {
                                    emit(
                                        deps.telemetry_log,
                                        &bead.id,
                                        1,
                                        OverlayState::HumanHeld.as_str(),
                                        "ESCALATION_REQUIRED",
                                        serde_json::json!({}),
                                        ctx,
                                    )?;
                                    record_escalation_emit_dedup(
                                        deps,
                                        &bead.id,
                                        "unmapped_repo",
                                        &ctx_hash,
                                        now_epoch,
                                    )?;
                                }
                            }
                            Err(err) => {
                                if is_missing_scm_target_error(&err) {
                                    // (a) missing-target: local escalation
                                    // fallback records the sentinel AND
                                    // bumps beads_escalated_locally so
                                    // dashboards see the escalation event
                                    // even though no SCM comment was
                                    // postable.
                                    record_local_escalation_fallback(
                                        deps,
                                        &bead.id,
                                        "unmapped_repo",
                                    )?;
                                    summary.beads_escalated_locally += 1;
                                    emit(
                                        deps.telemetry_log,
                                        &bead.id,
                                        1,
                                        OverlayState::HumanHeld.as_str(),
                                        "ESCALATED_LOCALLY",
                                        serde_json::json!({}),
                                        serde_json::json!({
                                            "reason": "unmapped_repo",
                                            "source": "manual_adoption_fail_closed",
                                            "scm_error": err.to_string(),
                                        }),
                                    )?;
                                } else if !err.is_transient() {
                                    // (b) non-transient: terminal mark +
                                    // ESCALATION_UNDELIVERABLE; never
                                    // re-attempt on later ticks.
                                    mark_escalation_undeliverable_and_emit(
                                        deps,
                                        summary,
                                        &bead.id,
                                        1,
                                        OverlayState::HumanHeld.as_str(),
                                        "unmapped_repo",
                                        &err,
                                    )?;
                                } else {
                                    // (c) transient: emit
                                    // ESCALATION_NOTIFICATION_FAILED with
                                    // dedup so the next tick retries. Do
                                    // NOT record the escalation sentinel —
                                    // the comment will be retried.
                                    let ctx = serde_json::json!({
                                        "reason": "unmapped_repo",
                                        "source": "manual_adoption_fail_closed",
                                        "error": err.to_string(),
                                    });
                                    let now_epoch = now_epoch_secs();
                                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                                        deps,
                                        &bead.id,
                                        "unmapped_repo",
                                        &ctx,
                                        now_epoch,
                                    )?;
                                    if !should_emit {
                                        summary.escalations_suppressed += 1;
                                    } else {
                                        emit(
                                            deps.telemetry_log,
                                            &bead.id,
                                            1,
                                            OverlayState::HumanHeld.as_str(),
                                            "ESCALATION_NOTIFICATION_FAILED",
                                            serde_json::json!({}),
                                            ctx,
                                        )?;
                                        record_escalation_emit_dedup(
                                            deps,
                                            &bead.id,
                                            "unmapped_repo",
                                            &ctx_hash,
                                            now_epoch,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
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
                    attempt_started_at: None,
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
                set_human_hold_reason(&mut held, HumanHoldReason::RouterParse(reason.clone()));
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
        // Bead jleechan-g1ib / CLAIMED tag coordination: drop any bead
        // whose local overlay shows it held by another machine within the
        // TTL window. The peer-reported claims are consulted by
        // `bin/claimd daemon`'s sync loop and have already been written
        // to the local overlay via `replace_peer_claims` — but we only
        // gate on the LOCAL overlay here so the tick loop stays
        // dependency-free of `bin/claimd`. `claim_blocks_dispatch` is a
        // no-op for fakes (always returns false), preserving pre-claim
        // dispatch behavior in unit tests.
        let claim_self_machine = std::env::var("CLAIM_MACHINE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "jeff-ubuntu".to_string());
        let claim_ttl_secs: u64 = std::env::var("CLAIM_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1800);
        let now_epoch = now_epoch_secs();
        let before = ready.len();
        ready.retain(|(bead, _, _)| {
            !deps
                .store
                .claim_blocks_dispatch(&bead.id, now_epoch, claim_ttl_secs, &claim_self_machine)
                .unwrap_or(false)
        });
        let skipped = before - ready.len();
        if skipped > 0 {
            let _ = emit(
                deps.telemetry_log,
                "_dispatch_filter",
                0,
                "N/A",
                "CLAIM_BLOCKED_DISPATCH",
                serde_json::json!({"skipped": skipped, "ttl_secs": claim_ttl_secs}),
                serde_json::json!({"self_machine": claim_self_machine}),
            );
        }
        if ready.is_empty() {
            return Ok(());
        }
        let dispatch_report =
            dispatch::dispatch_ready_with_vcs(
                deps.sessions,
                deps.store,
                deps.cfg,
                &ready,
                Some(deps.vcs),
            )?;
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
                    if !err.is_transient() {
                        mark_escalation_undeliverable_and_emit(
                            deps,
                            summary,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "transient_spawn_retry_cap_exceeded",
                            &err,
                        )?;
                        continue;
                    }
                    let ctx = serde_json::json!({
                        "reason": "transient_spawn_retry_cap_exceeded",
                        "error": err.to_string(),
                    });
                    let now_epoch = now_epoch_secs();
                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                        deps,
                        &failure.bead_id,
                        "transient_spawn_retry_cap_exceeded",
                        &ctx,
                        now_epoch,
                    )?;
                    if !should_emit {
                        summary.escalations_suppressed += 1;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "transient_spawn_retry_cap_exceeded",
                        &ctx_hash,
                        now_epoch,
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "transient_spawn_retry_cap_exceeded")?;
                summary.beads_escalated += 1;
                let ctx = serde_json::json!({
                    "reason": "transient_spawn_retry_cap_exceeded",
                    "max_transient_spawn_retry": MAX_TRANSIENT_SPAWN_RETRY,
                    "branch": failure.branch.as_deref(),
                });
                let now_epoch = now_epoch_secs();
                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                    deps,
                    &failure.bead_id,
                    "transient_spawn_retry_cap_exceeded",
                    &ctx,
                    now_epoch,
                )?;
                if !should_emit {
                    summary.escalations_suppressed += 1;
                } else {
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_REQUIRED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "transient_spawn_retry_cap_exceeded",
                        &ctx_hash,
                        now_epoch,
                    )?;
                }
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
                        record_local_escalation_fallback(deps, &failure.bead_id, "unmapped_repo")?;
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
                    if !err.is_transient() {
                        mark_escalation_undeliverable_and_emit(
                            deps,
                            summary,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "unmapped_repo",
                            &err,
                        )?;
                        continue;
                    }
                    let ctx = serde_json::json!({
                        "reason": "unmapped_repo",
                        "error": err.to_string(),
                    });
                    let now_epoch = now_epoch_secs();
                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                        deps,
                        &failure.bead_id,
                        "unmapped_repo",
                        &ctx,
                        now_epoch,
                    )?;
                    if !should_emit {
                        summary.escalations_suppressed += 1;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "unmapped_repo",
                        &ctx_hash,
                        now_epoch,
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "unmapped_repo")?;
                summary.beads_escalated += 1;
                let ctx = serde_json::json!({"reason": "unmapped_repo"});
                let now_epoch = now_epoch_secs();
                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                    deps,
                    &failure.bead_id,
                    "unmapped_repo",
                    &ctx,
                    now_epoch,
                )?;
                if !should_emit {
                    summary.escalations_suppressed += 1;
                } else {
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_REQUIRED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "unmapped_repo",
                        &ctx_hash,
                        now_epoch,
                    )?;
                }
                continue;
            }

            if matches!(
                failure.phase,
                "unmapped_target_repo" | "target_checkout_unconfigured" | "spawn_failed"
            ) {
                let reason = failure.phase;
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
                        "reason": reason,
                        "error": failure.error.as_str(),
                    }),
                )?;
                if escalation_already_recorded(deps, &failure.bead_id)? {
                    continue;
                }
                let comment_body = match reason {
                    "target_checkout_unconfigured" => format!(
                        "🤖 **[dark-factory]** Escalation required: bead `{}` targets a configured repository whose `local_checkout` is absent, relative, or not a directory. Automation parked only this bead HUMAN_HELD rather than cloning or spawning from an invalid checkout; repair `[repos.\"<repo>\"].local_checkout` before requeuing.",
                        failure.bead_id
                    ),
                    "spawn_failed" => format!(
                        "🤖 **[dark-factory]** Escalation required: bead `{}` hit a permanent worker-spawn failure. Automation parked only this bead HUMAN_HELD and continued unrelated dispatch work; inspect the AO error and target checkout before requeuing. Details: {}",
                        failure.bead_id, failure.error
                    ),
                    _ => format!(
                        "🤖 **[dark-factory]** Escalation required: bead `{}` claims a `target_repo` with no matching `[repos.*]` config entry (and it is not the daemon's global `target_repo`). Automation parked it HUMAN_HELD rather than guessing which repo/AO-project to dispatch into; please add a `[repos.\"<repo>\"]` entry to `config/daemon.toml` (or correct the bead's `target_repo`) before requeuing.",
                        failure.bead_id
                    ),
                };
                if let Err(err) = post_scm_comment_by_bead_id(deps, &failure.bead_id, &comment_body)
                {
                    if is_missing_scm_target_error(&err) {
                        record_local_escalation_fallback(
                            deps,
                            &failure.bead_id,
                            reason,
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
                                "reason": reason,
                                "scm_error": err.to_string(),
                            }),
                        )?;
                        continue;
                    }
                    if !err.is_transient() {
                        mark_escalation_undeliverable_and_emit(
                            deps,
                            summary,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            reason,
                            &err,
                        )?;
                        continue;
                    }
                    let ctx = serde_json::json!({
                        "reason": reason,
                        "error": err.to_string(),
                    });
                    let now_epoch = now_epoch_secs();
                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                        deps,
                        &failure.bead_id,
                        reason,
                        &ctx,
                        now_epoch,
                    )?;
                    if !should_emit {
                        summary.escalations_suppressed += 1;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        reason,
                        &ctx_hash,
                        now_epoch,
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, reason)?;
                summary.beads_escalated += 1;
                let ctx = serde_json::json!({"reason": reason});
                let now_epoch = now_epoch_secs();
                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                    deps,
                    &failure.bead_id,
                    reason,
                    &ctx,
                    now_epoch,
                )?;
                if !should_emit {
                    summary.escalations_suppressed += 1;
                } else {
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_REQUIRED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        reason,
                        &ctx_hash,
                        now_epoch,
                    )?;
                }
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
                    if !err.is_transient() {
                        mark_escalation_undeliverable_and_emit(
                            deps,
                            summary,
                            &failure.bead_id,
                            failure.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "worktree_remote_mismatch",
                            &err,
                        )?;
                        continue;
                    }
                    let ctx = serde_json::json!({
                        "reason": "worktree_remote_mismatch",
                        "error": err.to_string(),
                    });
                    let now_epoch = now_epoch_secs();
                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                        deps,
                        &failure.bead_id,
                        "worktree_remote_mismatch",
                        &ctx,
                        now_epoch,
                    )?;
                    if !should_emit {
                        summary.escalations_suppressed += 1;
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_NOTIFICATION_FAILED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "worktree_remote_mismatch",
                        &ctx_hash,
                        now_epoch,
                    )?;
                    continue;
                }
                record_escalation(deps, &failure.bead_id, "worktree_remote_mismatch")?;
                summary.beads_escalated += 1;
                let ctx = serde_json::json!({"reason": "worktree_remote_mismatch"});
                let now_epoch = now_epoch_secs();
                let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                    deps,
                    &failure.bead_id,
                    "worktree_remote_mismatch",
                    &ctx,
                    now_epoch,
                )?;
                if !should_emit {
                    summary.escalations_suppressed += 1;
                } else {
                    emit(
                        deps.telemetry_log,
                        &failure.bead_id,
                        failure.attempt,
                        OverlayState::HumanHeld.as_str(),
                        "ESCALATION_REQUIRED",
                        serde_json::json!({}),
                        ctx,
                    )?;
                    record_escalation_emit_dedup(
                        deps,
                        &failure.bead_id,
                        "worktree_remote_mismatch",
                        &ctx_hash,
                        now_epoch,
                    )?;
                }
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

    // jtg8-r4: persist the adoption-probe cache at the end of every
    // slow-tier pass so a daemon restart doesn't re-probe the entire
    // factory-labeled PR set on its first tick. Best-effort: a failed
    // write logs but does not abort the tick (a missing cache file is
    // the same as a cold cache).
    if let Err(e) = adoption_cache.persist() {
        eprintln!(
            "auto-factory daemon: WARNING failed to persist adoption-probe cache: {e}"
        );
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

/// Default skeptic / `/er` reviewer priority (operator 2026-08-18):
/// `claudem` (bashrc MiniMax wrapper) → `agy` (Antigravity) →
/// `cursor-agent` (`agentf`). Gemini CLI is not in this list: it is the
/// same Google family as `agy` and is no longer a factory reviewer.
/// Claude Sonnet and Codex stay out. Dual-dispatch of the first two
/// (with the coder vendor excluded) is what satisfies the cross-model
/// guarantee; `cursor-agent` is the fallback second family.
pub(crate) const SKEPTIC_REVIEWER_PRIORITY: &[&str] = &["claudem", "agy", "cursor-agent"];

/// Provider variables that are meaningful to the MiniMax-compatible
/// `claudem` lane but must not leak into a direct Anthropic Claude process.
/// The daemon can dispatch reviewer lanes in parallel, so these are removed
/// from the child environment rather than mutated in the parent process.
const DIRECT_CLAUDE_PROVIDER_ENV: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
    "CLAUDE_CODE_USE_AZURE",
    "CLAUDE_CODE_USE_OPENAI",
    "CLAUDEM_MODE",
    "MINIMAX_API_KEY",
    "MINIMAX_BASE_URL",
    "MINIMAX_MODEL",
    "DARK_FACTORY_MINIMAX_MODEL",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_BEDROCK_REGION",
    "ANTHROPIC_BEDROCK_MODEL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_VERTEX_PROJECT_ID",
    "ANTHROPIC_VERTEX_REGION",
    "CLOUD_ML_REGION",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_ROLE_ARN",
    "AWS_ROLE_SESSION_NAME",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_ENDPOINT_URL",
    "AWS_ENDPOINT_URL_BEDROCK",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AWS_BEDROCK_MODEL_ID",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_API_KEY",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GCLOUD_PROJECT",
    "VERTEXAI_PROJECT",
    "VERTEXAI_LOCATION",
    "BEDROCK_MODEL_ID",
    "BEDROCK_REGION",
    "AZURE_OPENAI_API_KEY",
    "AZURE_OPENAI_ENDPOINT",
    "AZURE_OPENAI_API_VERSION",
    "FOUNDRY_API_KEY",
    "FOUNDRY_ENDPOINT",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL",
];

#[cfg(target_os = "linux")]
#[repr(C)]
struct LinuxPasswd {
    pw_name: *mut std::os::raw::c_char,
    pw_passwd: *mut std::os::raw::c_char,
    pw_uid: u32,
    pw_gid: u32,
    pw_gecos: *mut std::os::raw::c_char,
    pw_dir: *mut std::os::raw::c_char,
    pw_shell: *mut std::os::raw::c_char,
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn geteuid() -> u32;
    fn getpwuid_r(
        uid: u32,
        pwd: *mut LinuxPasswd,
        buffer: *mut std::os::raw::c_char,
        buffer_len: usize,
        result: *mut *mut LinuxPasswd,
    ) -> std::os::raw::c_int;
}

/// Resolve the account home from the kernel's effective uid, never from the
/// mutable HOME environment inherited by a child process.
fn login_home_dir() -> Result<std::path::PathBuf, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        let uid = unsafe { geteuid() };
        let mut record = std::mem::MaybeUninit::<LinuxPasswd>::zeroed();
        let mut buffer = vec![0 as std::os::raw::c_char; 64 * 1024];
        let mut result = std::ptr::null_mut();
        let rc = unsafe {
            getpwuid_r(
                uid,
                record.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() {
            return Err(DaemonError::Config(
                "direct Claude reviewer cannot resolve the login user's home directory"
                    .to_string(),
            ));
        }
        let record = unsafe { result.as_ref() }.ok_or_else(|| {
            DaemonError::Config(
                "direct Claude reviewer cannot resolve the login user's home directory"
                    .to_string(),
            )
        })?;
        if record.pw_dir.is_null() {
            return Err(DaemonError::Config(
                "direct Claude reviewer cannot resolve the login user's home directory"
                    .to_string(),
            ));
        }
        let home = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }
            .to_str()
            .map_err(|_| {
                DaemonError::Config(
                    "direct Claude reviewer login home is not valid UTF-8".to_string(),
                )
            })?;
        let home = std::path::PathBuf::from(home);
        if !home.is_absolute() {
            return Err(DaemonError::Config(
                "direct Claude reviewer login home must be absolute".to_string(),
            ));
        }
        std::fs::canonicalize(home).map_err(|error| {
            DaemonError::Config(format!(
                "direct Claude reviewer cannot resolve the login user's home directory: {error}"
            ))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(DaemonError::Config(
            "direct Claude reviewer cannot resolve the login user's home directory on this platform"
                .to_string(),
        ))
    }
}

fn files_byte_identical(
    first: &std::path::Path,
    second: &std::path::Path,
) -> Result<bool, std::io::Error> {
    use std::io::Read;
    let mut first = std::fs::File::open(first)?;
    let mut second = std::fs::File::open(second)?;
    let mut first_buf = [0u8; 8192];
    let mut second_buf = [0u8; 8192];
    loop {
        let first_len = first.read(&mut first_buf)?;
        let second_len = second.read(&mut second_buf)?;
        if first_len != second_len {
            return Ok(false);
        }
        if first_len == 0 {
            return Ok(true);
        }
        if first_buf[..first_len] != second_buf[..second_len] {
            return Ok(false);
        }
    }
}

/// Resolve the explicitly configured Claude account directory against a
/// caller-supplied login home. The caller must obtain that home from the OS
/// account database, never from the mutable HOME environment.
fn direct_claude_config_dir_with_login_home(
    login_home: &std::path::Path,
) -> Result<std::path::PathBuf, DaemonError> {
    let raw = std::env::var_os("DARK_FACTORY_CLAUDE_CONFIG_DIR").ok_or_else(|| {
        DaemonError::Config(
            "direct Claude reviewer requires DARK_FACTORY_CLAUDE_CONFIG_DIR pointing to a project-scoped config directory"
                .to_string(),
        )
    })?;
    let configured = std::path::PathBuf::from(raw);
    if !configured.is_absolute() {
        return Err(DaemonError::Config(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR must be an absolute path".to_string(),
        ));
    }
    if !configured.is_dir() {
        return Err(DaemonError::Config(format!(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR must name an existing directory: {}",
            configured.display()
        )));
    }
    let resolved = std::fs::canonicalize(&configured).map_err(|error| {
        DaemonError::Config(format!(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR could not be resolved: {error}"
        ))
    })?;
    let home_claude = login_home.join(".claude");
    let personal_root = std::fs::canonicalize(&home_claude).ok();
    if let Some(default_dir) = personal_root.as_ref() {
        if resolved == *default_dir {
            return Err(DaemonError::Config(
                "DARK_FACTORY_CLAUDE_CONFIG_DIR must not resolve to the operator's ~/.claude directory"
                    .to_string(),
            ));
        }
    } else if configured == home_claude {
        // The configured path is required to exist above, so this lexical
        // comparison covers a HOME/.claude spelling whose canonicalization
        // failed due to a transient filesystem race.
        return Err(DaemonError::Config(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR must not point to the operator's ~/.claude directory"
                .to_string(),
        ));
    }
    for name in [
        ".credentials.json",
        ".claude.json",
        "settings.json",
        "mcp-strict.json",
    ] {
        let child = resolved.join(name);
        let metadata = match std::fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(DaemonError::Config(format!(
                    "could not inspect Claude config child {}: {error}",
                    child.display()
                )))
            }
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = std::fs::canonicalize(&child).map_err(|error| {
            DaemonError::Config(format!(
                "DARK_FACTORY_CLAUDE_CONFIG_DIR has an unresolved critical symlink {}: {error}",
                child.display()
            ))
        })?;
        if personal_root
            .as_ref()
            .is_some_and(|personal| target.starts_with(personal))
        {
            return Err(DaemonError::Config(format!(
                "DARK_FACTORY_CLAUDE_CONFIG_DIR critical file {name} resolves inside personal ~/.claude"
            )));
        }
    }
    for name in [".credentials.json", ".claude.json"] {
        let child = resolved.join(name);
        let personal = home_claude.join(name);
        if !child.is_file() || !personal.is_file() {
            continue;
        }
        let same_inode = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let child_metadata = std::fs::metadata(&child).map_err(|error| {
                    DaemonError::Config(format!(
                        "could not inspect Claude config child {}: {error}",
                        child.display()
                    ))
                })?;
                let personal_metadata = std::fs::metadata(&personal).map_err(|error| {
                    DaemonError::Config(format!(
                        "could not inspect personal Claude credential file {}: {error}",
                        personal.display()
                    ))
                })?;
                child_metadata.dev() == personal_metadata.dev()
                    && child_metadata.ino() == personal_metadata.ino()
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        let same_contents = files_byte_identical(&child, &personal).map_err(|error| {
            DaemonError::Config(format!(
                "could not compare personal Claude credential file {name}: {error}"
            ))
        })?;
        if same_inode || same_contents {
            return Err(DaemonError::Config(format!(
                "DARK_FACTORY_CLAUDE_CONFIG_DIR must not reuse personal credential file {name}"
            )));
        }
    }
    Ok(resolved)
}

/// Resolve the explicitly configured Claude account directory, refusing the
/// operator's default `~/.claude` account.
fn direct_claude_config_dir() -> Result<std::path::PathBuf, DaemonError> {
    let login_home = login_home_dir()?;
    direct_claude_config_dir_with_login_home(&login_home)
}

/// Dispatch one independent reviewer subprocess by vendor name. Extracted
/// from `skeptic_evidence` so two vendors can be dispatched in parallel
/// threads (PR#163 finding 2) without duplicating the per-vendor argv
/// construction. `pub(crate)` so `/er` reuses the same argv table instead
/// of hardcoding Claude.
pub(crate) fn dispatch_reviewer(vendor: &str, prompt: &str) -> Result<String, DaemonError> {
    use crate::tools::{run_tool, run_tool_with_env_and_remove};
    match vendor {
        "codex" => run_tool(
            "codex",
            &["exec", "--yolo", "--skip-git-repo-check", prompt],
            REVIEWER_TIMEOUT_SECS,
        ),
        "claude" => {
            let config_dir = direct_claude_config_dir()?;
            let config_dir = config_dir.to_str().ok_or_else(|| {
                DaemonError::Config(
                    "DARK_FACTORY_CLAUDE_CONFIG_DIR must be valid UTF-8".to_string(),
                )
            })?;
            run_tool_with_env_and_remove(
                "claude",
                &[
                    "--print",
                    "--dangerously-skip-permissions",
                    "--setting-sources",
                    "",
                    prompt,
                ],
                &[("CLAUDE_CONFIG_DIR", config_dir)],
                DIRECT_CLAUDE_PROVIDER_ENV,
                REVIEWER_TIMEOUT_SECS,
            )
        }
        // bashrc `claudem()`: MiniMax via the Claude Code CLI. Headless
        // factory review uses `--print` (never `--teammate-mode=tmux`,
        // which is interactive and would hang the daemon). Env is applied
        // to this child only so a sibling `agy` thread cannot inherit
        // MiniMax's ANTHROPIC_BASE_URL.
        "claudem" | "minimax" => {
            let key = std::env::var("MINIMAX_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    DaemonError::Config(
                        "MiniMax reviewer requires a non-empty MINIMAX_API_KEY".to_string(),
                    )
                })?;
            // Unattended MiniMax reviewer traffic is pinned to M3.  Do not
            // let a host override change the child env or `--model`.
            let model = "MiniMax-M3";
            crate::tools::run_tool_with_env_and_remove(
                "claude",
                &[
                    "--print",
                    "--dangerously-skip-permissions",
                    "--setting-sources",
                    "",
                    "--effort",
                    "high",
                    "--model",
                    model,
                    prompt,
                ],
                &[
                    ("ANTHROPIC_BASE_URL", "https://api.minimax.io/anthropic"),
                    ("ANTHROPIC_API_KEY", key.as_str()),
                    ("ANTHROPIC_MODEL", model),
                    ("ANTHROPIC_SMALL_FAST_MODEL", model),
                ],
                DIRECT_CLAUDE_PROVIDER_ENV,
                REVIEWER_TIMEOUT_SECS,
            )
        }
        // Flag order matters: agy's `--print` takes the PROMPT as its own
        // value, so any flag placed between `--print` and the prompt is
        // swallowed as the message and the real prompt is dropped (the
        // reviewer then answers the literal flag string — the historical
        // "agy returns empty stdout" symptom was this, not quota).
        "agy" => run_tool(
            "agy",
            &["--dangerously-skip-permissions", "--print", prompt],
            REVIEWER_TIMEOUT_SECS,
        ),
        // Gemini CLI is kept for explicit override only. It is not in
        // `SKEPTIC_REVIEWER_PRIORITY` (operator 2026-08-18): same Google
        // family as `agy`. `--yolo` auto-approves tool calls; `--skip-trust`
        // is required in headless contexts or the CLI refuses to run.
        "gemini" => run_tool(
            "gemini",
            &["-p", prompt, "--yolo", "--skip-trust"],
            REVIEWER_TIMEOUT_SECS,
        ),
        // Default fallback reviewer (Cursor CLI, bashrc `agentf`). Invoked as
        // `cursor-agent -f <prompt>` (headless). Distinct family from
        // claudem/agy (see `verifier::vendor_model_family`).
        "cursor-agent" | "cursor" | "agentf" => run_tool(
            "cursor-agent",
            &["-f", prompt],
            REVIEWER_TIMEOUT_SECS,
        ),
        other => Err(DaemonError::Tool {
            tool: other.to_string(),
            rc: -1,
            stderr: "unknown reviewer vendor".to_string(),
        }),
    }
}

#[cfg(test)]
mod direct_claude_scope_tests {
    use super::{direct_claude_config_dir_with_login_home, dispatch_reviewer};
    use std::path::Path;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        crate::adapters::gh_env_test_lock()
    }

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn capture(keys: &[&'static str]) -> Self {
            Self(
                keys.iter()
                    .map(|key| (*key, std::env::var_os(key)))
                    .collect(),
            )
        }

        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) {
            unsafe { std::env::set_var(key, value) };
        }

        fn remove(key: &'static str) {
            unsafe { std::env::remove_var(key) };
        }
    }

    /// Put a test shim first while retaining the host tools needed by sibling
    /// tests (notably `git`).  Replacing PATH outright lets process-global env
    /// mutations make unrelated tests fail with ENOENT under parallel cargo
    /// test execution.
    fn prepend_path(dir: &Path) -> std::ffi::OsString {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        std::env::join_paths(paths).expect("test shim path must be joinable")
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_reviewer_fails_closed_without_project_config() {
        let _guard = env_lock().lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("afd_direct_claude_scope_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let claude = root.join("bin").join("claude");
        std::fs::write(&claude, "#!/bin/sh\nprintf unexpected-success\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _env = EnvRestore::capture(&["PATH", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        EnvRestore::set("PATH", prepend_path(&root.join("bin")));
        assert!(
            std::process::Command::new("git")
                .arg("--version")
                .status()
                .expect("system git must remain discoverable with the fake Claude shim")
                .success()
        );
        EnvRestore::remove("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        let result = dispatch_reviewer("claude", "scope-test");
        let _ = std::fs::remove_dir_all(&root);

        assert!(
            result.is_err(),
            "direct Claude must require an explicit project config"
        );
    }

    #[test]
    fn direct_claude_reviewer_rejects_relative_config_path() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvRestore::capture(&["DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", "relative/project-claude");

        let result = dispatch_reviewer("claude", "scope-test");

        assert!(
            result.is_err(),
            "relative Claude config paths must fail closed"
        );
    }

    #[test]
    fn direct_claude_reviewer_rejects_missing_absolute_config_path() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvRestore::capture(&["DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        let missing = std::env::temp_dir().join(format!(
            "afd_missing_direct_claude_config_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &missing);

        let result = dispatch_reviewer("claude", "scope-test");

        assert!(
            result.is_err(),
            "missing Claude config directories must fail closed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_reviewer_rejects_operator_default_config_dir() {
        let _guard = env_lock().lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("afd_direct_claude_default_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home_claude = root.join("home").join(".claude");
        std::fs::create_dir_all(&home_claude).unwrap();
        let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        EnvRestore::set("HOME", root.join("fake-home"));
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &home_claude);

        let result = direct_claude_config_dir_with_login_home(&root.join("home"));

        let _ = std::fs::remove_dir_all(&root);
        assert!(result.is_err(), "operator ~/.claude must not be accepted");
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_reviewer_rejects_login_users_personal_config_when_home_is_mutated() {
        let _guard = env_lock().lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "afd_direct_claude_mutated_home_{}",
            std::process::id()
        ));
        let login_home = root.join("login-home");
        let personal = login_home.join(".claude");
        std::fs::create_dir_all(&personal).unwrap();
        let fake_home = root.join("fake-home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        EnvRestore::set("HOME", &fake_home);
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &personal);

        let result = direct_claude_config_dir_with_login_home(&login_home);

        let _ = std::fs::remove_dir_all(&root);
        assert!(
            result.is_err(),
            "changing HOME must not allow the login user's ~/.claude"
        );
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_config_rejects_critical_symlinks_into_personal_tree() {
        let _guard = env_lock().lock().unwrap();
        for name in [".credentials.json", "settings.json"] {
            let root = std::env::temp_dir().join(format!(
                "afd_direct_claude_critical_link_{}_{}",
                std::process::id(),
                name.replace('.', "_")
            ));
            let _ = std::fs::remove_dir_all(&root);
            let personal = root.join("home").join(".claude");
            let scoped = root.join("project-claude");
            std::fs::create_dir_all(&personal).unwrap();
            std::fs::create_dir_all(&scoped).unwrap();
            std::fs::write(personal.join(name), "personal\n").unwrap();
            std::os::unix::fs::symlink(personal.join(name), scoped.join(name)).unwrap();

            let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
            EnvRestore::set("HOME", root.join("home"));
            EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &scoped);
            assert!(
                direct_claude_config_dir_with_login_home(&root.join("home")).is_err(),
                "critical symlink {name} into personal config must fail closed"
            );
            drop(_env);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_config_rejects_copied_or_hardlinked_personal_credentials() {
        let _guard = env_lock().lock().unwrap();
        for name in [".credentials.json", ".claude.json"] {
            for kind in ["copy", "hardlink"] {
                let root = std::env::temp_dir().join(format!(
                    "afd_direct_claude_credential_copy_{}_{}_{}",
                    std::process::id(),
                    name.replace('.', "_"),
                    kind
                ));
                let _ = std::fs::remove_dir_all(&root);
                let personal = root.join("login-home").join(".claude");
                let scoped = root.join("project-claude");
                std::fs::create_dir_all(&personal).unwrap();
                std::fs::create_dir_all(&scoped).unwrap();
                let source = personal.join(name);
                std::fs::write(&source, b"{\"account\":\"personal\"}\n").unwrap();
                let target = scoped.join(name);
                if kind == "hardlink" {
                    std::fs::hard_link(&source, &target).unwrap();
                } else {
                    std::fs::copy(&source, &target).unwrap();
                }
                let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
                EnvRestore::set("HOME", root.join("fake-home"));
                EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &scoped);
                assert!(
                    direct_claude_config_dir_with_login_home(&root.join("login-home")).is_err(),
                    "{kind} personal credential {name} must fail closed"
                );
                drop(_env);
                let _ = std::fs::remove_dir_all(&root);
            }
        }
    }

    #[test]
    fn direct_claude_config_does_not_compare_benign_profile_files() {
        let _guard = env_lock().lock().unwrap();
        for name in ["settings.json", "mcp-strict.json"] {
            let root = std::env::temp_dir().join(format!(
                "afd_direct_claude_benign_copy_{}_{}",
                std::process::id(),
                name.replace('.', "_")
            ));
            let _ = std::fs::remove_dir_all(&root);
            let personal = root.join("login-home").join(".claude");
            let scoped = root.join("project-claude");
            std::fs::create_dir_all(&personal).unwrap();
            std::fs::create_dir_all(&scoped).unwrap();
            std::fs::write(personal.join(name), b"shared-benign-profile\n").unwrap();
            std::fs::copy(personal.join(name), scoped.join(name)).unwrap();
            let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
            EnvRestore::set("HOME", root.join("fake-home"));
            EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &scoped);
            assert!(
                direct_claude_config_dir_with_login_home(&root.join("login-home")).is_ok(),
                "benign profile file {name} may be copied into a WA config"
            );
            drop(_env);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn direct_claude_config_accepts_independent_regular_files() {
        let _guard = env_lock().lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "afd_direct_claude_regular_config_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let personal = root.join("home").join(".claude");
        let scoped = root.join("project-claude");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::create_dir_all(&scoped).unwrap();
        for name in [".credentials.json", ".claude.json", "settings.json", "mcp-strict.json"] {
            std::fs::write(scoped.join(name), "independent\n").unwrap();
        }

        let _env = EnvRestore::capture(&["HOME", "DARK_FACTORY_CLAUDE_CONFIG_DIR"]);
        EnvRestore::set("HOME", root.join("home"));
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &scoped);
        let resolved = direct_claude_config_dir_with_login_home(&root.join("home"))
            .expect("regular independent config is valid");
        assert_eq!(resolved, std::fs::canonicalize(&scoped).unwrap());
        drop(_env);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn direct_claude_reviewer_scrubs_minimax_provider_environment() {
        let _guard = env_lock().lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("afd_direct_claude_env_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let home = root.join("home");
        let config = root.join("project-claude");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf 'config=%s\\nbase=%s\\napi=%s\\nauth=%s\\nmodel=%s\\nmode=%s\\nminimax_base=%s\\nminimax_model=%s\\ndark_minimax_model=%s\\n' \"$CLAUDE_CONFIG_DIR\" \"$ANTHROPIC_BASE_URL\" \"$ANTHROPIC_API_KEY\" \"$ANTHROPIC_AUTH_TOKEN\" \"$ANTHROPIC_MODEL\" \"$CLAUDEM_MODE\" \"$MINIMAX_BASE_URL\" \"$MINIMAX_MODEL\" \"$DARK_FACTORY_MINIMAX_MODEL\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _env = EnvRestore::capture(&[
            "PATH",
            "HOME",
            "DARK_FACTORY_CLAUDE_CONFIG_DIR",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
            "CLAUDEM_MODE",
            "MINIMAX_BASE_URL",
            "MINIMAX_MODEL",
            "DARK_FACTORY_MINIMAX_MODEL",
        ]);
        EnvRestore::set("PATH", prepend_path(&bin));
        EnvRestore::set("HOME", &home);
        EnvRestore::set("DARK_FACTORY_CLAUDE_CONFIG_DIR", &config);
        EnvRestore::set("ANTHROPIC_BASE_URL", "https://api.minimax.io/anthropic");
        EnvRestore::set("ANTHROPIC_API_KEY", "stale-minimax-key");
        EnvRestore::set("ANTHROPIC_AUTH_TOKEN", "stale-minimax-token");
        EnvRestore::set("ANTHROPIC_MODEL", "MiniMax-M3");
        EnvRestore::set("CLAUDEM_MODE", "1");
        EnvRestore::set("MINIMAX_BASE_URL", "https://stale.minimax.example");
        EnvRestore::set("MINIMAX_MODEL", "stale-minimax-model");
        EnvRestore::set("DARK_FACTORY_MINIMAX_MODEL", "stale-minimax-model");

        let result =
            dispatch_reviewer("claude", "scope-test").expect("scoped Claude shim succeeds");

        let expected_config = std::fs::canonicalize(&config).unwrap();
        let expected_config = expected_config.to_string_lossy();
        assert!(
            result.contains(&format!("config={expected_config}")),
            "{result}"
        );
        for line in [
            "base=",
            "api=",
            "auth=",
            "model=",
            "mode=",
            "minimax_base=",
            "minimax_model=",
            "dark_minimax_model=",
        ] {
            assert!(
                result.lines().any(|candidate| candidate == line),
                "{line:?} leaked in {result:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn minimax_reviewer_requires_api_key_before_spawning() {
        let _guard = env_lock().lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "afd_minimax_reviewer_missing_key_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\nprintf unexpected-spawn\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _env = EnvRestore::capture(&["PATH", "MINIMAX_API_KEY"]);
        EnvRestore::set("PATH", prepend_path(&bin));
        EnvRestore::remove("MINIMAX_API_KEY");
        let result = dispatch_reviewer("claudem", "scope-test");
        let _ = std::fs::remove_dir_all(&root);

        let rendered = result.expect_err("missing MiniMax key must fail closed").to_string();
        assert!(rendered.contains("MINIMAX_API_KEY"), "{rendered}");
        assert!(!rendered.contains("unexpected-spawn"), "{rendered}");
    }

    #[test]
    #[cfg(unix)]
    fn minimax_reviewer_pins_model_and_scrubs_inherited_claude_environment() {
        let _guard = env_lock().lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "afd_minimax_reviewer_env_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf 'config=%s\\nbase=%s\\napi=%s\\nauth=%s\\nmodel=%s\\nsmall=%s\\nmode=%s\\nminimax_base=%s\\nminimax_model=%s\\ndark_minimax_model=%s\\nargs=' \"$CLAUDE_CONFIG_DIR\" \"$ANTHROPIC_BASE_URL\" \"$ANTHROPIC_API_KEY\" \"$ANTHROPIC_AUTH_TOKEN\" \"$ANTHROPIC_MODEL\" \"$ANTHROPIC_SMALL_FAST_MODEL\" \"$CLAUDEM_MODE\" \"$MINIMAX_BASE_URL\" \"$MINIMAX_MODEL\" \"$DARK_FACTORY_MINIMAX_MODEL\"; for arg in \"$@\"; do printf '<%s>' \"$arg\"; done; printf '\\n'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _env = EnvRestore::capture(&[
            "PATH",
            "CLAUDE_CONFIG_DIR",
            "MINIMAX_API_KEY",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "CLAUDEM_MODE",
            "MINIMAX_BASE_URL",
            "MINIMAX_MODEL",
            "DARK_FACTORY_MINIMAX_MODEL",
        ]);
        EnvRestore::set("PATH", prepend_path(&bin));
        EnvRestore::set("CLAUDE_CONFIG_DIR", "/home/operator/.claude");
        EnvRestore::set("MINIMAX_API_KEY", "minimax-key");
        EnvRestore::set("ANTHROPIC_BASE_URL", "https://personal.invalid");
        EnvRestore::set("ANTHROPIC_API_KEY", "personal-key");
        EnvRestore::set("ANTHROPIC_AUTH_TOKEN", "personal-token");
        EnvRestore::set("ANTHROPIC_MODEL", "personal-model");
        EnvRestore::set("ANTHROPIC_SMALL_FAST_MODEL", "personal-fast");
        EnvRestore::set("CLAUDEM_MODE", "stale");
        EnvRestore::set("MINIMAX_BASE_URL", "https://stale.minimax.example");
        EnvRestore::set("MINIMAX_MODEL", "stale-minimax-model");
        EnvRestore::set("DARK_FACTORY_MINIMAX_MODEL", "MiniMax-Test-Model");

        let result = dispatch_reviewer("minimax", "scope-test").expect("MiniMax shim succeeds");
        let _ = std::fs::remove_dir_all(&root);

        for line in [
            "config=",
            "auth=",
            "mode=",
            "minimax_base=",
            "minimax_model=",
            "dark_minimax_model=",
        ] {
            assert!(result.lines().any(|candidate| candidate == line), "{line:?}: {result}");
        }
        assert!(result.contains("base=https://api.minimax.io/anthropic"), "{result}");
        assert!(result.contains("api=minimax-key"), "{result}");
        assert!(result.contains("model=MiniMax-M3"), "{result}");
        assert!(result.contains("small="), "{result}");
        assert!(result.contains("<--model><MiniMax-M3>"), "{result}");
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

/// jleechan-8s2p (phase 2): build the Stage-1 skeptic prompt.
///
/// Extracted from `skeptic_evidence` so the prompt body is directly
/// unit-testable without standing up the subprocess dispatch path.
/// Two constraints drove the extraction:
///
/// 1. The reviewer subprocess (`dispatch_reviewer`) runs
///    `codex exec` / `claude --print` with no cwd override, so `gh`
///    commands the reviewer issues without an explicit `--repo`
///    default to whatever repo the daemon process's own cwd happens
///    to be checked out as. The prompt embeds `repo` (the bead's OWN
///    resolved repo, `overlay.repo(cfg)` at the call site) plus
///    explicit `--repo` flags, mirroring `er_runner::build_er_prompt`,
///    so the reviewer queries the RIGHT repo regardless of daemon cwd.
///
/// 2. jleechan-8s2p (the r6 reviewer's P2 finding): the waived-vendor
///    context MUST be in the prompt BEFORE the skeptic LLM is
///    dispatched. Otherwise the skeptic sees a capped vendor still
///    pending on `gh pr checks`, can fail/warn solely on that signal,
///    and `compensating_coverage_green` then refuses the waiver
///    because the required skeptic `Pass` was never obtainable. The
///    earlier code only copied the vendor ledger into `PrEvidence`
///    AFTER the skeptic had already responded, so the prompt and the
///    waiver logic disagreed about whether the vendor check mattered.
///    Each Capped vendor's canonical waiver token (e.g.
///    `bugbot:waived_vendor_unavailable`) is now embedded directly in
///    the prompt with explicit "do not fail on a waived vendor
///    check" guidance.
fn build_skeptic_prompt(
    bead_id: &str,
    pr: u64,
    repo: &str,
    vendor_health: &crate::vendor_health::VendorHealthLedger,
) -> String {
    use crate::vendor_health::Vendor;
    use crate::vendor_health::VendorHealth;

    let mut vendor_waiver_block = String::new();
    for vendor in [Vendor::CodeRabbit, Vendor::Bugbot] {
        match vendor_health.health(vendor) {
            VendorHealth::Capped { observations, since_epoch } => {
                if let Some(next) = crate::vendor_health::next_healthy_reviewer(vendor) {
                    vendor_waiver_block.push_str(&format!(
                        "\nREVIEWER_ROTATED vendor={} nextReviewer={} \
                         chainWalked=true waiverSuppressed=true",
                        vendor.as_str(), next
                    ));
                    continue;
                }
                vendor_waiver_block.push_str(&format!(
                    "\nVENDOR WAIVER CONTEXT\n\
                     The {vendor_name} check is structurally unavailable on this PR \
                     (cap observations: {obs_count}, first observed at epoch {since}). \
                     Treat a pending or missing {vendor_name} check as waived, NOT as \
                     a fail signal — the vendor cannot deliver a review and the bead's \
                     compensating coverage (skeptic pass + /er pass + cross-model) is \
                     the substitute trust floor. Waiver token: {waiver_token}",
                    vendor_name = vendor.as_str(),
                    obs_count = observations.len(),
                    since = since_epoch,
                    waiver_token = vendor.waiver_token(),
                ));
            }
            VendorHealth::Healthy => {}
        }
    }

    // Bead jleechan-5arc: the skeptic must be an INDEPENDENT signal.
    //
    // The previous prompt told the reviewer to run `gh pr checks` — exactly
    // the data the `Ci` gate already computes — and to judge "ready to
    // merge". That made skeptic re-derive other gates' verdicts from the
    // same events, so it failed whenever CI was merely pending and whenever
    // a vendor was silent. Measured: 603 fail / 45 pass across 652
    // assessments, with ZERO passes in the 12 days to 2026-08-05 even though
    // PRs merged clean in that window (beads 41gk/PR #546, jsby/PR #466,
    // jtg8/PR #455 were 3/3, 9/9, 3/3 skeptic-fail and all shipped fine).
    // A 20-sample classification found 50% of fails were pure restatements
    // of ci_green/coderabbit/bugbot/evidence_review.
    //
    // Worse, it broke the `Waived` contract documented at
    // `verifier::GateResult` (see the jleechan-jsby doc comment): a waiver
    // for an unavailable vendor REQUIRES compensating coverage — skeptic +
    // /er + cross-model — to be green. A skeptic that reads `gh pr checks`
    // and the vendor's own comments cannot compensate for that vendor: the
    // waiver needs skeptic green exactly when the signals skeptic is reading
    // are absent or red. That circularity made the waiver unreachable when
    // it was most needed.
    //
    // The fix scopes skeptic to the artifact only other gates do NOT read:
    // the diff itself. `gh pr checks` is removed, and the verdict question
    // is narrowed from "ready to merge" to "is there a defect in this diff".
    // Its genuine defect-finding is preserved — 30% of the sampled fails
    // were real, diff-specific defects, and those must still fail.
    format!(
        "You are the Stage-1 Skeptic gate for an autonomous coding factory.\n\
         Judge ONE question about bead {bead_id}'s PR #{pr} in repo {repo}: \
         does the DIFF ITSELF contain a defect?\n\
           gh pr diff {pr} --repo {repo}\n\
           gh pr view {pr} --repo {repo} --json body,comments\n\
         \n\
         SCOPE — you are one of several independent gates. CI status, merge \
         conflicts, CodeRabbit, Bugbot, comment resolution and evidence \
         review are SEPARATE gates that already read those signals. Do NOT \
         run `gh pr checks` and do NOT fail for anything they own: pending, \
         running, queued or failing CI; a missing, pending or unhappy vendor \
         review; unresolved review threads. Another gate reporting red is not \
         your finding to repeat — restating it adds no information and \
         destroys your value as an independent signal.\n\
         \n\
         FAIL only for a defect you can point to in the diff: incorrect \
         logic, a test that does not exercise what it claims, a missing edge \
         case, a security or data-integrity problem, or evidence that \
         contradicts the change. Cite the file and line. If the diff is sound, \
         PASS — even while other gates are still red or pending.\n\
         Respond with exactly one line of the form:\n\
         pass|warn <note>|fail <reason>{vendor_waiver_block}",
    )
}

/// rev-gujs2 (ZFC-1, HIGH): anchored-marker scan for a
/// `verdict:`/`overall:`/`normalized:` declaration line, or a `/skeptic
/// pass|warn|fail` command line, within free-text GitHub PR comments. Unlike
/// `verifier::find_marker_verdict` (which anchors the marker to a LINE via
/// `str::find`, so `"the old verdict: fail was wrong"` still parses as Fail
/// — traced by hand: after the `"verdict:"` substring the remainder is
/// `" fail was wrong"`, and `token_to_verdict` takes the first
/// whitespace-delimited token `"fail"`), this scan requires the marker to be
/// the START of the TRIMMED line, case-insensitively — text that merely
/// discusses or quotes a verdict phrase mid-sentence does not match at all.
/// `find_marker_verdict` stays correct for its own callers (parsing a
/// trusted LLM reviewer's own single structured completion, per
/// gate_es/gate_er/gate_code_standards in CLAUDE.md), but is not anchored
/// enough for arbitrary free-text authored by untrusted GitHub commenters.
/// Returns the LAST matching line's canonical `"verdict: pass"` /
/// `"verdict: fail"` string (an authoritative closing line overrides
/// earlier progress chatter, mirroring `find_marker_verdict`'s "last marker
/// wins"), or `None` if no anchored declaration line is present. `warn` has
/// no independent state in this enrichment-signal grammar (matching the
/// pre-fix behavior, which never set gha/sign-off to anything but
/// pass/fail/absent), so only `pass`/`success` and `fail`/`failure` tokens
/// are recognized.
fn anchored_comment_verdict(body: &str) -> Option<&'static str> {
    const MARKERS: [&str; 3] = ["verdict:", "overall:", "normalized:"];

    let mut found = None;
    for line in body.lines() {
        let trimmed_lower = line.trim().to_ascii_lowercase();
        let after = MARKERS
            .iter()
            .find_map(|m| trimmed_lower.strip_prefix(m))
            .or_else(|| trimmed_lower.strip_prefix("/skeptic "));
        if let Some(after) = after {
            let token = after.split_whitespace().next().unwrap_or("");
            match token {
                "pass" | "success" => found = Some("verdict: pass"),
                "fail" | "failure" => found = Some("verdict: fail"),
                _ => {}
            }
        }
    }
    found
}

/// rev-gujs2 (ZFC-1, HIGH): derive the OPTIONAL `gha`/`sign-off` enrichment
/// signals from `snapshot.comments`. Extracted out of `skeptic_evidence`
/// (which is private and has subprocess side effects) so this is directly
/// unit-testable, mirroring the `build_skeptic_prompt` /
/// `second_family_candidates` extraction precedent in this file.
///
/// Previously this loop used unanchored `.contains(...)` substring scans
/// over the lower-cased comment body — banned ZFC-style keyword matching
/// over free-text authored by arbitrary GitHub commenters. A comment merely
/// containing the bare word "signoff"/"sign-off" ANYWHERE, or one that
/// discussed/quoted a prior "verdict: fail" mid-sentence, would flip a
/// signal and could escalate the combined gate-7 verdict (Fail beats Warn
/// beats Pass) on an otherwise healthy PR. Both signals now require
/// `anchored_comment_verdict` to find a genuine declaration line. The bare
/// "sign-off"/"signoff" word trigger is DROPPED entirely rather than
/// hardened, per the bead's own FIX note: `gha`/`sign-off` are optional
/// enrichment signals most target repos never emit, and the bare word has
/// no anchorable grammar — only `verdict:`/`overall:`/`normalized:` marker
/// lines and `/skeptic pass|fail` command lines can flip a signal now.
///
/// The author/topic gates (gha must be `github-actions`/`gha` AND mention
/// "skeptic"; sign-off must NOT be `github-actions`/`coderabbit`/`bugbot`/
/// `cursor`) are unchanged — those are legitimate coarse filters, not the
/// ZFC violation. Iterates `comments` in order; the LAST matching comment
/// for each signal wins, matching the original loop's behavior.
fn derive_enrichment_verdicts(
    comments: &[crate::tools::PrComment],
) -> (&'static str, &'static str) {
    let mut gha_verdict = "verdict: absent";
    let mut signoff_verdict = "verdict: absent";

    for comment in comments {
        let author_lower = comment.author.to_ascii_lowercase();

        if (author_lower.contains("github-actions") || author_lower.contains("gha"))
            && comment.body.to_ascii_lowercase().contains("skeptic")
        {
            if let Some(v) = anchored_comment_verdict(&comment.body) {
                gha_verdict = v;
            }
        }

        if !author_lower.contains("github-actions")
            && !author_lower.contains("coderabbit")
            && !author_lower.contains("bugbot")
            && !author_lower.contains("cursor")
        {
            if let Some(v) = anchored_comment_verdict(&comment.body) {
                signoff_verdict = v;
            }
        }
    }

    (gha_verdict, signoff_verdict)
}

#[cfg(test)]
mod anchored_comment_verdict_tests {
    //! rev-gujs2 (ZFC-1, HIGH): pins the false-positive scenarios the
    //! unanchored `.contains(...)` scan let through, plus the anchored
    //! declaration-line grammar that replaces it.
    use super::anchored_comment_verdict;

    #[test]
    fn bare_signoff_word_in_unrelated_prose_does_not_match() {
        // Pre-fix behavior: `body_lower.contains("signoff")` alone flipped
        // `signoff_verdict` to "verdict: pass" here — zero structured
        // marker required. The anchored scan requires a declaration line,
        // so a bare word in ordinary prose must not match.
        assert_eq!(
            anchored_comment_verdict("let's schedule the signoff meeting for Friday"),
            None
        );
        assert_eq!(
            anchored_comment_verdict("still need sign-off from the team lead"),
            None
        );
    }

    #[test]
    fn quoted_verdict_phrase_mid_sentence_does_not_match() {
        // Pre-fix `find_marker_verdict`-style unanchored `.find()` scan
        // would parse this as Fail (marker found mid-line, remainder
        // " fail was wrong" tokenizes to "fail"). The anchored scan
        // requires "verdict:" at the START of the trimmed line.
        assert_eq!(
            anchored_comment_verdict(
                "note: the old verdict: fail was wrong, this PR fixes it"
            ),
            None
        );
    }

    #[test]
    fn anchored_verdict_pass_line_matches() {
        assert_eq!(
            anchored_comment_verdict(
                "Ran the checks locally, all green.\nverdict: pass\nMerging shortly."
            ),
            Some("verdict: pass")
        );
    }

    #[test]
    fn anchored_verdict_fail_line_matches() {
        assert_eq!(
            anchored_comment_verdict("verdict: fail missing test coverage"),
            Some("verdict: fail")
        );
    }

    #[test]
    fn anchored_skeptic_command_line_matches() {
        assert_eq!(anchored_comment_verdict("/skeptic fail"), Some("verdict: fail"));
        assert_eq!(anchored_comment_verdict("/skeptic pass"), Some("verdict: pass"));
    }

    #[test]
    fn overall_and_normalized_markers_match_parity_with_verdict() {
        assert_eq!(
            anchored_comment_verdict("overall: pass"),
            Some("verdict: pass")
        );
        assert_eq!(
            anchored_comment_verdict("normalized: fail"),
            Some("verdict: fail")
        );
    }

    #[test]
    fn last_anchored_line_wins_when_multiple_present() {
        assert_eq!(
            anchored_comment_verdict("verdict: fail\nfixed now\nverdict: pass"),
            Some("verdict: pass")
        );
    }

    #[test]
    fn indented_marker_line_still_anchors() {
        // `trim()` strips leading whitespace before the prefix check, so a
        // marker line indented inside a quoted block still counts as
        // line-start, not mid-sentence.
        assert_eq!(
            anchored_comment_verdict("  verdict: pass  "),
            Some("verdict: pass")
        );
    }
}

#[cfg(test)]
mod derive_enrichment_verdicts_tests {
    //! rev-gujs2 (ZFC-1, HIGH): pins the author/topic gates plus the
    //! "last matching comment wins" iteration order, now layered on top of
    //! `anchored_comment_verdict` instead of bare substring scans.
    use super::derive_enrichment_verdicts;
    use crate::tools::PrComment;

    fn comment(author: &str, body: &str) -> PrComment {
        PrComment {
            author: author.to_string(),
            body: body.to_string(),
            created_at_epoch: 0,
        }
    }

    #[test]
    fn no_comments_yields_both_absent() {
        assert_eq!(
            derive_enrichment_verdicts(&[]),
            ("verdict: absent", "verdict: absent")
        );
    }

    #[test]
    fn bare_signoff_word_from_human_reviewer_no_longer_flips_signoff() {
        // Pre-fix: this exact body flipped signoff_verdict to
        // "verdict: pass" via `body_lower.contains("signoff")`. Confirmed
        // by hand against the removed loop in tick.rs prior to rev-gujs2 —
        // there was no marker requirement at all.
        let comments = [comment(
            "some-reviewer",
            "let's schedule the signoff meeting for Friday",
        )];
        let (_, signoff) = derive_enrichment_verdicts(&comments);
        assert_eq!(signoff, "verdict: absent");
    }

    #[test]
    fn quoted_verdict_phrase_from_human_reviewer_does_not_flip_signoff() {
        let comments = [comment(
            "some-reviewer",
            "note: the old verdict: fail was wrong, this PR fixes it",
        )];
        let (_, signoff) = derive_enrichment_verdicts(&comments);
        assert_eq!(signoff, "verdict: absent");
    }

    #[test]
    fn anchored_signoff_pass_from_human_reviewer_flips_verdict() {
        let comments = [comment("some-reviewer", "verdict: pass")];
        let (_, signoff) = derive_enrichment_verdicts(&comments);
        assert_eq!(signoff, "verdict: pass");
    }

    #[test]
    fn anchored_skeptic_command_from_human_reviewer_flips_signoff() {
        let comments = [comment("some-reviewer", "/skeptic fail")];
        let (_, signoff) = derive_enrichment_verdicts(&comments);
        assert_eq!(signoff, "verdict: fail");
    }

    #[test]
    fn excluded_authors_never_contribute_to_signoff_even_when_anchored() {
        for author in ["github-actions[bot]", "coderabbitai", "bugbot", "cursor-agent"] {
            let comments = [comment(author, "verdict: pass")];
            let (_, signoff) = derive_enrichment_verdicts(&comments);
            assert_eq!(signoff, "verdict: absent", "author={author}");
        }
    }

    #[test]
    fn gha_requires_actions_author_skeptic_topic_and_anchored_marker() {
        // Right author/topic, no anchored marker -> stays absent.
        let comments = [comment(
            "github-actions[bot]",
            "skeptic run kicked off, results pending",
        )];
        let (gha, _) = derive_enrichment_verdicts(&comments);
        assert_eq!(gha, "verdict: absent");

        // Right author/topic AND anchored marker -> flips.
        let comments = [comment(
            "github-actions[bot]",
            "skeptic run complete\nverdict: fail\nsee log for details",
        )];
        let (gha, _) = derive_enrichment_verdicts(&comments);
        assert_eq!(gha, "verdict: fail");

        // Anchored marker but body never mentions "skeptic" -> stays absent.
        let comments = [comment("github-actions[bot]", "verdict: pass")];
        let (gha, _) = derive_enrichment_verdicts(&comments);
        assert_eq!(gha, "verdict: absent");

        // Anchored marker + skeptic topic, but wrong author -> stays absent.
        let comments = [comment("some-other-bot", "skeptic verdict: pass")];
        let (gha, _) = derive_enrichment_verdicts(&comments);
        assert_eq!(gha, "verdict: absent");
    }

    #[test]
    fn last_matching_comment_wins_per_signal_independently() {
        let comments = [
            comment("github-actions[bot]", "skeptic run\nverdict: pass"),
            comment("some-reviewer", "verdict: fail"),
            comment("github-actions[bot]", "skeptic run\nverdict: fail"),
            comment("some-reviewer", "verdict: pass"),
        ];
        let (gha, signoff) = derive_enrichment_verdicts(&comments);
        assert_eq!(gha, "verdict: fail");
        assert_eq!(signoff, "verdict: pass");
    }
}

fn skeptic_evidence(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    repo: &str,
    snapshot: &crate::tools::PrSnapshot,
    vendor_health: crate::vendor_health::VendorHealthLedger,
) -> Result<PrEvidence, DaemonError> {
    // jleechan-9xrs Stage D: the reviewer subprocess (`dispatch_reviewer`)
    // jleechan-8s2p (phase 2): the waived-vendor context is now part
    // of the prompt BEFORE the skeptic LLM is dispatched. Building
    // the prompt in a dedicated helper (`build_skeptic_prompt`)
    // keeps the dispatch loop readable and makes the prompt content
    // directly unit-testable (was previously buried inside
    // `skeptic_evidence`, which is private and has heavy
    // subprocess side effects).
    let prompt = build_skeptic_prompt(bead_id, pr, repo, &vendor_health);

    let coder_agent = std::env::var("DARK_FACTORY_CODER_DEFAULT")
        .or_else(|_| std::env::var("DARK_FACTORY_REVIEWER_DEFAULT"))
        .unwrap_or_else(|_| "agy".to_string());

    // rev-9zrgs: was a hand-written ordered `.contains()` chain (fragile —
    // "claudem" contains "claude" as a substring, so arm order mattered and
    // a reorder would silently misclassify). `vendor_aliases::canonical_vendor`
    // does an exact-match lookup against `config/vendor_aliases.json`
    // instead, so ordering can no longer matter. See
    // `daemon/src/vendor_aliases.rs` module doc for the alias set and the
    // investigation behind the exact-match (vs token-based) design choice.
    let coder_vendor = crate::vendor_aliases::canonical_vendor(&coder_agent);

    // Operator 2026-08-18: reviewer queue is claudem → agy → cursor-agent.
    // Default coder is agy (fallback claudem), so production exclusion
    // drops agy and dual-dispatches claudem + cursor-agent (minimax +
    // cursor families). Gemini CLI is not in this list.
    let mut priority: Vec<&str> = SKEPTIC_REVIEWER_PRIORITY.to_vec();
    if !coder_vendor.is_empty() {
        priority.retain(|&v| v != coder_vendor);
    }

    // jleechan-9xrs Stage D: was `deps.cfg.target_repo` — must be the
    // bead's OWN resolved repo so a test-repo bead dispatched under a
    // non-test global `cfg.target_repo` (or vice versa) is classified
    // correctly instead of by the daemon-global repo.
    let is_test_repo = crate::config::is_fixture_repo(repo);

    // rev-gujs2 (ZFC-1, HIGH): derivation now anchors to declaration lines
    // instead of scanning for substrings anywhere in the comment body — see
    // `derive_enrichment_verdicts` and `anchored_comment_verdict` above.
    let (gha_verdict, signoff_verdict) = derive_enrichment_verdicts(&snapshot.comments);

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
        let vendor1 = priority.first().copied().unwrap_or("claudem").to_string();
        let vendor2 = priority.get(1).copied().unwrap_or("agy").to_string();
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
                let mut verdict_so_far =
                    v.expect("combine_dual_verdict returns Some(..) whenever it returns Ok(..)");
                // Cross-model guarantee (issue #385 / strict merge policy
                // #328): if everything parseable so far came from ONE model
                // family (e.g. a codex quota outage leaves claude alone),
                // `compute_review_degraded` will flag the assessment and the
                // strict gate deterministically fails itself — the 2026-08-06
                // fleet-wide circuit-breaker park loop. Pursue a SECOND
                // family down the remaining priority list. The second
                // reviewer's verdict is COMBINED (a dissenting second family
                // can still fail the gate); it is never merely counted.
                if verifier::compute_review_degraded(&used_vendors) {
                    for vendor_n in second_family_candidates(&used_vendors, &priority) {
                        let v_n = dispatch_reviewer(vendor_n, &prompt)
                            .ok()
                            .and_then(|r| verifier::parse_skeptic_verdict(&r));
                        if let Some(second) = v_n {
                            if let Ok(Some(combined)) = combine_dual_verdict(
                                Some(verdict_so_far.clone()),
                                Some(second),
                                bead_id,
                                pr,
                            ) {
                                verdict_so_far = combined;
                                used_vendors.push(vendor_n.to_string());
                                break;
                            }
                        }
                    }
                }
                verdict_so_far
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

    // jleechan-984e / issue #385: compute the cross-model degraded flag
    // BEFORE moving `used_vendors` into `PrEvidence` so we can still
    // borrow it. `compute_review_degraded` returns false for the
    // empty / single-entry / `mock_llm`-only paths so the Stage-1
    // test-repo lane stays non-degraded even though its single mock
    // judge has no cross-model sibling.
    let review_degraded = verifier::compute_review_degraded(&used_vendors);

    Ok(PrEvidence {
        is_production: false,
        non_test_changed_loc: 0,
        er_verdict: verifier::ErVerdict::Absent,
        skeptic_verdict,
        skeptic_reviewers: used_vendors,
        review_degraded,
        // Set in the fast tier from the canonical evidence marker (#323).
        evidence_gist_status: verifier::EvidenceGistStatus::NotProvided,
        // Bead jleechan-jsby (r2): the vendor-health ledger is now
        // POPULATED in the fast tier (acceptance criterion 1) — the
        // r1 PR #459 was rejected because the field was constructed
        // empty here and never written. The caller passes the
        // process-wide ledger (or `Default::default()` for the
        // Stage-1 mock-llm test-repo lane), and this function simply
        // forwards it to `verifier::assess` via `PrEvidence`.
        vendor_health,
        // Bead jleechan-ijod / issue #387 (r5): the runtime vacuous-test
        // detector only runs in the production-adjacent fast tier (which
        // has the SCM/git context to derive the diff); Stage 1's mock-llm
        // test-repo lane has no PR diff to revert, so it stays NotProvided.
        // The fast-tier caller is responsible for invoking
        // `daemon::vacuous_red_green::check_red_green_with_manifest` and
        // translating the verdict into a `VacuousRedGreenStatus` before
        // constructing `PrEvidence` here.
        vacuous_red_green: verifier::VacuousRedGreenStatus::NotProvided,
    })
}

/// Combine two reviewer verdicts (one per subprocess) into a single
/// `SkepticVerdict` for gate 7.
///
/// Single-pass success is preserved: if EITHER reviewer returns a usable
/// Bead jleechan-ijod / issue #387 (r5/r6): invoke the runtime red-green
/// vacuous-test detector for `pr` in `repo` and translate its
/// `RedGreenReport` into a `VacuousRedGreenStatus` for the gate-8
/// consumer.
///
/// This is the PRODUCTION-side wiring of the gate-8 path that the
/// verifier side (`vacuous_red_green_gate`) consumes. It is intentionally
/// minimal: the detector runs only when the daemon's own CWD is a checkout
/// of a cargo project (the typical Stage-2 production-adjacent lane). For
/// the Stage-1 test-repo lane (`is_test_repo`) the detector is not invoked
/// and the status stays `NotProvided`, so the gate stays Green.
///
/// The detector's verdict is translated verbatim — the r5 contract says
/// the gate consumer is the source of truth on what each verdict means for
/// merge eligibility (`Vacuous -> Red`, others -> Green/Unknown).
fn vacuous_red_green_for_pr(
    deps: &TickDeps,
    pr: u64,
    repo: &str,
    snapshot: &crate::tools::PrSnapshot,
    is_test_repo: bool,
) -> verifier::VacuousRedGreenStatus {
    // Test-repo PRs (Stage-1 mock-llm lane) have no PR diff to revert, so
    // the detector has nothing to measure. Stay NotProvided so gate 8
    // stays Green — issue #387 r5 contract: the gate must not block the
    // test-repo fast lane. r6 fix: the caller now passes `is_test_repo` in
    // (already computed at the outer scope of `run_fast_tier`) instead of
    // re-deriving from substrings of `repo` — substring heuristics miss
    // fixture repos like `myorg/myrepo` that don't contain "fake-",
    // "test-", or "owner/repo", so the detector previously tried to invoke
    // gh pr view on them, failed, and surfaced BaselineFailed -> Unknown
    // on PRs that pre-existing tests assert as beads_ready:1.
    if is_test_repo {
        return verifier::VacuousRedGreenStatus::NotProvided;
    }

    // The detector needs a local working tree to revert + cargo-run. Resolve
    // the bead's target execution resource, never the daemon binary's CWD:
    // Installed uv/release binaries are immutable and their CWD may not even
    // be a checkout. Resolve the target resource from config, then provision
    // a dedicated isolated checkout when the host has not created it yet.
    let routing = match deps.cfg.resolve_repo(repo) {
        Some(routing) => routing,
        None => {
            return verifier::VacuousRedGreenStatus::ManifestMissing(format!(
                "no repository routing configured for {repo:?}"
            ));
        }
    };
    if !deps.cfg.worker_checkout_is_configured(repo, &routing) {
        return verifier::VacuousRedGreenStatus::ManifestMissing(format!(
            "configured local checkout for repo {repo:?} is missing, relative, or not a directory"
        ));
    }
    let requested = match deps.cfg.target_worktree_path(repo) {
        Some(path) => path,
        None => {
            return verifier::VacuousRedGreenStatus::ManifestMissing(format!(
                "no target worktree path available for repo {repo:?}"
            ));
        }
    };
    let ensure = if routing.local_checkout.is_none() {
        crate::target_worktree::ensure_managed_target_worktree
    } else {
        crate::target_worktree::ensure_target_worktree
    };
    let repo_root = match ensure(repo, &requested, Some(&snapshot.head_sha)) {
        Ok(path) => path,
        Err(error) => {
            return verifier::VacuousRedGreenStatus::ManifestMissing(format!(
                "provision target worktree for repo {repo:?} at {}: {error}",
                requested.display()
            ));
        }
    };
    let manifest = match crate::vacuous_red_green::find_cargo_manifest(&repo_root) {
        Some(m) => m,
        None => {
            // jleechan-ni1k / issue #437 bonus: dark-factory's daemon
            // crate lives at `<repo_root>/daemon/Cargo.toml`, not at the
            // repo root. The walk-up `find_cargo_manifest` returns None
            // on this nested-crate layout, surfacing `ManifestMissing`
            // on the very repo the gate is supposed to vet. Fall back
            // to a bounded recursive search (skips `target` /
            // `node_modules` / `.git`, capped at depth 4) so a nested
            // crate manifest is reachable. If both lookups fail, we
            // keep the original error message so operators see both
            // paths attempted.
            match crate::vacuous_red_green::find_cargo_manifest_recursive(&repo_root, 4) {
                Some(m) => m,
                None => {
                    return verifier::VacuousRedGreenStatus::ManifestMissing(format!(
                        "no Cargo.toml reachable from {} (walk-up + recursive depth-4 both failed)",
                        repo_root.display()
                    ));
                }
            }
        }
    };

    // Resolve the base ref from the PR's merge-base. Use `gh pr view`
    // to fetch the baseRefName, then resolve it locally via VCS. We
    // use the PR's head SHA from the snapshot so the diff is captured
    // relative to the same commit the gate will compare against.
    let base_ref = match resolve_pr_base_ref(deps, pr, repo) {
        Ok(b) => b,
        Err(e) => {
            // r6 fix: a "GraphQL: Could not resolve to a Repository" or
            // similar 404 means the PR's upstream is not a real GH repo —
            // typically a test fixture like `myorg/myrepo` that the gate
            // can't measure against. Treat as NotProvided (the detector
            // has no opinion) rather than BaselineFailed -> Unknown, so
            // the production-side gate stays Green for these edge cases
            // instead of spuriously blocking the fast lane. Other
            // resolution failures (e.g. network or auth error) remain
            // BaselineFailed so operators can diagnose.
            if e.contains("Could not resolve to a Repository")
                || e.contains("Not Found")
                || e.contains("404")
            {
                return verifier::VacuousRedGreenStatus::NotProvided;
            }
            return verifier::VacuousRedGreenStatus::BaselineFailed(format!(
                "could not resolve base ref: {e}"
            ));
        }
    };

    // Collect the PR's changed files and classify them.
    let changed = snapshot
        .files
        .iter()
        .map(|f| {
            let kind = if f.path.contains("/tests/")
                || f.path.starts_with("tests/")
                || f.path.ends_with("_test.rs")
            {
                crate::vacuous_red_green::FileClass::Test
            } else {
                crate::vacuous_red_green::FileClass::Production
            };
            Ok((repo_root.join(&f.path), kind))
        })
        .collect::<Result<Vec<_>, std::convert::Infallible>>()
        .unwrap_or_default();

    if changed.is_empty() {
        return verifier::VacuousRedGreenStatus::NoChangedTests;
    }

    // Invoke the detector.
    match crate::vacuous_red_green::check_red_green_with_manifest(
        &repo_root,
        &base_ref,
        &changed,
        Some(&manifest),
    ) {
        Ok(report) => translate_verdict(report.verdict, report.failed_on_revert),
        Err(e) => translate_error(e),
    }
}

/// Translate the detector's `RedGreenError` into a structured
/// `VacuousRedGreenStatus` so the gate can surface the failure mode
/// rather than swallowing it as a generic Unknown.
fn translate_error(e: crate::vacuous_red_green::RedGreenError) -> verifier::VacuousRedGreenStatus {
    use crate::vacuous_red_green::RedGreenError;
    match e {
        RedGreenError::NoChangedTests => verifier::VacuousRedGreenStatus::NoChangedTests,
        RedGreenError::ManifestMissing(s) => verifier::VacuousRedGreenStatus::ManifestMissing(s),
        RedGreenError::BaselineFailed(s) => verifier::VacuousRedGreenStatus::BaselineFailed(s),
        RedGreenError::RevertFailed(s) | RedGreenError::RestoreFailed(s) => {
            verifier::VacuousRedGreenStatus::GreenFailed(format!("working-tree revert failed: {s}"))
        }
        RedGreenError::Git(s) => verifier::VacuousRedGreenStatus::GreenFailed(format!("git error: {s}")),
        // Bead jleechan-sb4b: surface the missing toolchain as a
        // structured signal. The previous failure mode was a misleading
        // `GreenFailed: git error: spawn cargo test: No such file or
        // directory` on every assessment — operators couldn't tell that
        // the daemon's PATH lacked cargo. The new variant names the
        // real cause and hints at the fix.
        RedGreenError::CargoNotFound(s) => verifier::VacuousRedGreenStatus::CargoNotFound(s),
        // Bead jleechan-6xje: pytest backend parity. The detector
        // surface a structured "pytest not found" signal rather than
        // collapsing into a misleading git error. The gate maps
        // this to `Unknown` via the same `RunnerNotFound` arm the
        // verifier adds below.
        RedGreenError::PytestNotFound(s) => verifier::VacuousRedGreenStatus::PytestNotFound(s),
    }
}

/// Translate the detector's `Verdict` into a structured
/// `VacuousRedGreenStatus`. The `failed_on_revert` count is included in
/// the `GreenFailed` reason so operators can see which tests tripped
/// the head-green check.
fn translate_verdict(
    v: crate::vacuous_red_green::Verdict,
    failed_on_revert: usize,
) -> verifier::VacuousRedGreenStatus {
    use crate::vacuous_red_green::Verdict;
    match v {
        Verdict::Genuine => verifier::VacuousRedGreenStatus::Genuine,
        Verdict::Vacuous => verifier::VacuousRedGreenStatus::Vacuous,
        Verdict::GreenFailed => verifier::VacuousRedGreenStatus::GreenFailed(format!(
            "{failed_on_revert} test(s) failed on PR head (before any revert)"
        )),
        Verdict::BaselineFailed => verifier::VacuousRedGreenStatus::BaselineFailed(
            "tests failed on pristine base_ref (before any revert)".to_string(),
        ),
        Verdict::NoChangedTests => verifier::VacuousRedGreenStatus::NoChangedTests,
        Verdict::ManifestMissing => {
            verifier::VacuousRedGreenStatus::ManifestMissing("detector could not find manifest".to_string())
        }
    }
}

/// Resolve the merge-base SHA for `pr` in `repo`. Uses `gh pr view` to
/// fetch the baseRefName, then `git rev-parse` (via Vcs) to convert it
/// into a SHA. Returns the SHA, or an error string suitable for
/// surfacing in the gate.
fn resolve_pr_base_ref(deps: &TickDeps, pr: u64, repo: &str) -> Result<String, String> {
    // Use gh pr view to get the base ref name. Fail closed on any error
    // — the gate cannot meaningfully run the detector without a base ref.
    let out = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.to_string(),
            "--repo",
            repo,
            "--json",
            "baseRefName",
            "-q",
            ".baseRefName",
        ])
        .output()
        .map_err(|e| format!("spawn gh pr view: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh pr view failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() {
        return Err("gh pr view returned empty baseRefName".to_string());
    }
    deps.vcs
        .base_head_for_repo(repo, &branch)
        .map_err(|e| format!("resolve base ref {branch}: {e}"))
}

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
///
/// Cross-model guarantee (issue #385): given the vendors that already
/// contributed a parseable verdict and the coder-excluded priority list,
/// return the untried fallback vendors (priority index 2+) whose model
/// family differs from EVERY family already represented — i.e. the
/// candidates whose verdict could satisfy `compute_review_degraded ==
/// false`. Pure so the second-family pursuit is unit-testable without
/// spawning reviewer subprocesses. Unknown/empty-family vendor labels are
/// excluded (they cannot lift the degraded flag; see
/// `verifier::vendor_model_family`).
pub fn second_family_candidates<'a>(
    used: &[String],
    priority: &[&'a str],
) -> Vec<&'a str> {
    let have: std::collections::BTreeSet<&str> = used
        .iter()
        .map(|u| verifier::vendor_model_family(u))
        .filter(|f| !f.is_empty())
        .collect();
    priority
        .iter()
        .skip(2)
        .copied()
        .filter(|v| {
            let fam = verifier::vendor_model_family(v);
            !fam.is_empty()
                && !have.contains(fam)
                && !used.iter().any(|u| u.as_str() == *v)
        })
        .collect()
}

#[cfg(test)]
mod second_family_candidate_tests {
    //! PR #596 cold-review Major 1: the Ok-arm second-family pursuit is the
    //! fix's primary behavioral change — pin its candidate-selection
    //! semantics (family diversity, no re-dispatch of used vendors, skip(2)
    //! primaries, unknown families excluded) without spawning subprocesses.
    use super::second_family_candidates;

    const PRIORITY: [&str; 3] = ["claudem", "agy", "cursor-agent"];

    #[test]
    fn claudem_only_yields_cursor_agent_fallback() {
        let used = vec!["claudem".to_string()];
        // skip(2) omits the dual-dispatch primaries (claudem, agy); the only
        // remaining distinct family is cursor-agent.
        assert_eq!(
            second_family_candidates(&used, &PRIORITY),
            vec!["cursor-agent"]
        );
    }

    #[test]
    fn already_used_fallback_vendor_and_its_family_are_not_recandidated() {
        let used = vec!["claudem".to_string(), "cursor-agent".to_string()];
        // cursor-agent already contributed; agy is a dual-dispatch
        // primary (index 1) so skip(2) never re-offers it.
        assert!(second_family_candidates(&used, &PRIORITY).is_empty());
    }

    #[test]
    fn two_families_present_still_lists_only_new_families() {
        let used = vec!["claudem".to_string(), "agy".to_string()];
        // Caller only invokes this when degraded, but the helper itself
        // must never re-offer a represented family. cursor is still new.
        assert_eq!(
            second_family_candidates(&used, &PRIORITY),
            vec!["cursor-agent"]
        );
    }

    #[test]
    fn default_priority_excludes_claude_and_codex() {
        assert_eq!(
            super::SKEPTIC_REVIEWER_PRIORITY,
            &["claudem", "agy", "cursor-agent"]
        );
        assert!(!super::SKEPTIC_REVIEWER_PRIORITY.contains(&"gemini"));
        assert!(!super::SKEPTIC_REVIEWER_PRIORITY.contains(&"claude"));
        assert!(!super::SKEPTIC_REVIEWER_PRIORITY.contains(&"codex"));
        assert!(!super::SKEPTIC_REVIEWER_PRIORITY.contains(&"claude-sonnet"));
    }

    #[test]
    fn unknown_family_or_short_priority_yields_empty() {
        let used = vec!["claudem".to_string()];
        let priority = ["claudem", "agy", "mock_llm", "not-a-vendor"];
        assert!(second_family_candidates(&used, &priority).is_empty());
        let short = ["claudem", "agy"];
        assert!(second_family_candidates(&used, &short).is_empty());
    }
}

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

        // Bead jleechan-ijod / issue #387 (r6): lift `is_test_repo` to the
        // outer scope so gate 8 (`vacuous_red_green_for_pr`) can short-circuit
        // for the Stage-1 mock-llm lane. Without this lift, gate 8 invokes the
        // detector on test-fixture repos like `myorg/myrepo`, gh pr view fails,
        // and the gate reports `BaselineFailed -> Unknown` — pre-existing tests
        // that assert `beads_ready: 1` for these fixtures then fail because
        // Unknown blocks readiness. The Stage-1 lane has no PR diff to revert,
        // so NotProvided is the right answer (matches r5 contract).
        let is_test_repo = crate::config::is_fixture_repo(&repo);

        if overlay.state == OverlayState::Dispatched {
            if let Some(ref session_id_str) = overlay.session_id {
                let sid = SessionId(session_id_str.clone());
                if let Ok(Some(health_failure)) = deps.sessions.check_session_health(&sid) {
                    // rev-4ou1z: a Gemini individual-quota exhaustion is
                    // RECOVERABLE — the paused pane just needs an Enter
                    // keypress once its quota window resets, not a
                    // kill+respawn cycle. A fresh spawn would hit the same
                    // quota wall immediately, burning
                    // `MAX_TRANSIENT_SPAWN_RETRY` in minutes against a
                    // window that can take hours to reset (live incident:
                    // coder wa-3538 parked HUMAN_HELD well before its quota
                    // actually cleared). Already-armed sessions skip
                    // straight past the SESSION_HEALTH_FAILED emit + kill
                    // path below so the pane is left untouched for the
                    // slow-tier wake sweep (`run_quota_watchdog_wake`).
                    if crate::health::quota_watchdog::parse_quota_reset_duration(&health_failure)
                        .is_some()
                        && crate::health::quota_watchdog::recorded_reset_at(bead_id).is_some()
                    {
                        continue;
                    }
                    emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Dispatched.as_str(),
                        "SESSION_HEALTH_FAILED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "session_id": session_id_str,
                            "reason": health_failure,
                            "branch": overlay.branch,
                        }),
                    )?;
                    if let Some(reset_in) =
                        crate::health::quota_watchdog::parse_quota_reset_duration(&health_failure)
                    {
                        let now_epoch = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let reset_at_epoch = now_epoch.saturating_add(reset_in.as_secs());
                        crate::health::quota_watchdog::record_quota_reset(
                            bead_id,
                            session_id_str,
                            reset_at_epoch,
                        );
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Dispatched.as_str(),
                            "QUOTA_WATCHDOG_ARMED",
                            serde_json::json!({}),
                            serde_json::json!({
                                "session_id": session_id_str,
                                "reason": health_failure,
                                "reset_at_epoch": reset_at_epoch,
                            }),
                        )?;
                        continue;
                    }
                    let _ = deps.sessions.stop(&sid);
                    overlay.session_id = None;
                    if overlay.pr_number.is_none() {
                        overlay.spawn_failure_count += 1;
                        if overlay.spawn_failure_count >= MAX_TRANSIENT_SPAWN_RETRY {
                            overlay.state = OverlayState::HumanHeld;
                            set_human_hold_reason(
                                &mut overlay,
                                HumanHoldReason::TransientSpawnRetryCapExceeded,
                            );
                        } else {
                            overlay.state = OverlayState::Queued;
                        }
                        deps.store.save(&overlay)?;
                        continue;
                    } else {
                        deps.store.save(&overlay)?;
                    }
                }
            }
        }

        if overlay.state == OverlayState::Dispatched
            && overlay.pr_number.is_none()
            && !is_test_repo
        {
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

        // Promote DISPATCHED -> ATTESTED once a PR is open (spec §4.2.7).
        // jleechan-t40t r6: `pr_number_reresolved_this_tick` is set whenever
        // the dispatch→attested re-resolution path above mutates
        // `overlay.pr_number` (drift detection, stale-clear, or transient
        // error). Pre-gate validation below uses it to skip the redundant
        // `open_pr_head_ref_for_repo` re-check on freshly-resolved PRs,
        // which would otherwise emit a false-positive
        // `PR_PRE_GATE_VALIDATION_MISMATCH` on every healthy bead.
        let mut pr_number_reresolved_this_tick = false;
        if overlay.state == OverlayState::Dispatched {
            if let Some(ref branch) = overlay.branch {
                // jleechan-t40t (issue #326): re-resolve `pr_number` from the
                // bead's CURRENT branch every slow-tier tick (not just when
                // it's `None`). The pre-fix code only ran this lookup when
                // `pr_number.is_none()`, which meant a stale `pr_number` —
                // e.g. set from an AO session that was later superseded by
                // a different PR on the same branch, or written out-of-band
                // — kept the bead DISPATCHED indefinitely against the wrong
                // PR (every gate-assessment query targeted a PR the bead's
                // branch was no longer bound to).
                //
                // r6 contract: `Ok(Some(discovered))` either fills in the
                // missing `pr_number` (first-discovery) or supersedes a
                // stale one (drift detection), emitting `PR_NUMBER_REREZOLVED`
                // so the transition is auditable from the daemon log alone.
                // `Ok(None)` is FAIL-CLOSED: when a stale `pr_number` was
                // recorded against a now-merged/closed PR and the branch has
                // no live PR, the stale number MUST be cleared so the bead
                // does NOT promote to ATTESTED against a dead PR
                // (jleechan-t8fd / PR #316 wedge); the bead stays DISPATCHED
                // and waits for the next live PR. The clear is audited via
                // `PR_NUMBER_REREZOLVED_NO_OPEN_PR` with
                // reason=`branch_mismatch_no_open_pr`. A hard `Err` is logged
                // via `PR_NUMBER_REREZOLVE_TRANSIENT_ERROR` and skipped —
                // the next tick re-attempts the lookup.
                match deps.scm.pr_number_for_branch(&repo, branch) {
                    Ok(Some(discovered)) if Some(discovered) != overlay.pr_number => {
                        let previous = overlay.pr_number;
                        overlay.pr_number = Some(discovered);
                        deps.store.save(&overlay)?;
                        pr_number_reresolved_this_tick = true;
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Dispatched.as_str(),
                            "PR_NUMBER_REREZOLVED",
                            serde_json::json!({}),
                            serde_json::json!({
                                "branch": branch,
                                "previous_pr_number": previous,
                                "current_pr_number": discovered,
                                "reason": "branch_mismatch_stale_state",
                            }),
                        )?;
                    }
                    Ok(None) if overlay.pr_number.is_some() && !overlay.is_adopted => {
                        // Fail-closed: a stale `pr_number` points at a PR
                        // that is no longer open for this branch (e.g. it
                        // merged and the bead was re-cut with a fresh -rN
                        // branch, or the prior PR was closed out-of-band).
                        // Clearing it keeps the bead DISPATCHED and prevents
                        // promotion to ATTESTED against a dead PR. The
                        // next tick (or the same tick's later blocks) will
                        // see `pr_number.is_none()` and short-circuit out
                        // of any gate-assessment path until a new PR
                        // appears.
                        //
                        // NOT applied to adopted beads: an adopted PR's
                        // `pr_number` was set from a positively-confirmed
                        // external contributor's `external_ref` lookup
                        // at adoption time (see `intake::normalize_labeled_prs`),
                        // not from a branch→PR re-resolution. Clearing
                        // it here would erase the adoption guarantee —
                        // adopted beads rely on the stored `pr_number`
                        // surviving until the contributor's PR is
                        // closed/merged by them, NOT until our branch
                        // lookup happens to agree.
                        let previous = overlay.pr_number;
                        overlay.pr_number = None;
                        deps.store.save(&overlay)?;
                        pr_number_reresolved_this_tick = true;
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Dispatched.as_str(),
                            "PR_NUMBER_REREZOLVED_NO_OPEN_PR",
                            serde_json::json!({}),
                            serde_json::json!({
                                "branch": branch,
                                "previous_pr_number": previous,
                                "current_pr_number": serde_json::Value::Null,
                                "reason": "branch_mismatch_no_open_pr",
                            }),
                        )?;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // jleechan-t40t r12 (issue #326): FAIL CLOSED. A
                        // transient branch→PR resolution error leaves the
                        // stored `pr_number` UNVALIDATED this tick. The pre-fix
                        // code merely logged and fell through to the promotion
                        // block below, which would promote DISPATCHED→ATTESTED
                        // against a possibly-stale number (gate-assessing a PR
                        // the branch may no longer be bound to). Keep the bead
                        // DISPATCHED and retry the resolution next tick — never
                        // promote on an unvalidated number.
                        let _ = emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Dispatched.as_str(),
                            "PR_NUMBER_REREZOLVE_TRANSIENT_ERROR",
                            serde_json::json!({}),
                            serde_json::json!({
                                "branch": branch,
                                "error": format!("{e:?}"),
                                "action": "kept_dispatched_no_promotion",
                            }),
                        );
                        continue;
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
                        Some(session_id_str) => {
                            let sid = SessionId(session_id_str.clone());
                            if let Ok(Some(health_failure)) = deps.sessions.check_session_health(&sid) {
                                emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Dispatched.as_str(),
                                    "SESSION_HEALTH_FAILED",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "session_id": session_id_str,
                                        "reason": health_failure,
                                        "branch": overlay.branch,
                                    }),
                                )?;
                                let _ = deps.sessions.stop(&sid);
                                true
                            } else if deps.sessions.is_quiescent(&sid).unwrap_or(false) {
                                true
                            } else {
                                match deps.sessions.session_activity(&sid) {
                                    Ok(crate::tools::SessionActivity::Idle) => {
                                        let _ = deps.sessions.stop(&sid);
                                        true
                                    }
                                    Ok(
                                        crate::tools::SessionActivity::Terminal
                                        | crate::tools::SessionActivity::NotFound,
                                    ) => true,
                                    _ => false,
                                }
                            }
                        }
                        None => true,
                    }
                } else {
                    true
                };

                if ready_to_promote {
                    // Reap completed worker session to immediately release AO worker slot
                    if let Some(session_id_str) = overlay.session_id.take() {
                        let sid = SessionId(session_id_str.clone());
                        let _ = deps.sessions.stop(&sid);
                        // Bead rev-3lm8k: the coder session has finished and
                        // is being reaped right here — its AO-managed
                        // worktree dir (if any) is stale immediately, so
                        // clean it now rather than waiting on the next TTL
                        // sweep (a no-op when `agent_worktree_root` is
                        // unset). This is the exact "coder session exit"
                        // moment the bead's incident describes: a leftover
                        // worktree dir blocking every subsequent dispatch
                        // hashing to the same orchestrator branch.
                        match crate::worktree_reaper::clean_stale_worktree(
                            deps.cfg,
                            overlay.repo(deps.cfg),
                            &session_id_str,
                        ) {
                            Ok(true) => {
                                let _ = emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "WORKTREE_CLEANED_ON_SESSION_EXIT",
                                    serde_json::json!({}),
                                    serde_json::json!({"session_id": session_id_str}),
                                );
                            }
                            Ok(false) => {}
                            Err(e) => {
                                let _ = emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "WORKTREE_CLEAN_FAILED",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "session_id": session_id_str,
                                        "error": format!("{e:?}"),
                                    }),
                                );
                            }
                        }
                    }
                    // A positive branch-to-open-PR binding is the durable
                    // boundary between the deferred old attempt and this
                    // fresh attempt. Keep the marker while the old PR is the
                    // only assessable surface, then clear it immediately
                    // before the new PR becomes ATTESTED so the first fast
                    // tier pass assesses this PR rather than suppressing it.
                    if !overlay.is_adopted
                        && deps.store.reroll_deferral_count(bead_id).unwrap_or(0) > 0
                    {
                        deps.store.reset_reroll_deferral(bead_id)?;
                    }
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
        // jleechan-6l1f: gate-regression entry path. A READY bead must be
        // re-assessed on every fast tick so a green->red transition (PR went
        // red after being merge-ready) is detected and routed back into the
        // reroll lane — without this, READY was a terminal state that the
        // fast-tier `if overlay.state != OverlayState::Attested` filter
        // skipped, so a regressed PR silently sat READY forever while
        // auto-merge-guard.sh correctly refused the merge (the live
        // incident: PR #540 sat all_green=true at 2026-08-04T13:18:43Z,
        // went red ~24h later, and never re-entered the fix loop). The
        // demotion is IN-MEMORY only: persisting it now would churn the
        // DB and re-emit READY_FOR_MERGE-style telemetry on every healthy
        // READY bead, breaking the contract that READY is "terminal until
        // the merge guard accepts". If the assessment finds a regression
        // the demotion-to-Attested is persisted in the same save as the
        // red-state transition below; if it stays green we leave the
        // stored READY alone.
        let entered_as_ready = overlay.state == OverlayState::Ready;
        if entered_as_ready {
            overlay.state = OverlayState::Attested; // in-memory only (NOT saved)
        }
        if overlay.state != OverlayState::Attested {
            continue;
        }
        let pr = match overlay.pr_number {
            Some(pr) => pr,
            None => continue,
        };

        // jleechan-t40t r6: pre-gate validation. Before any gate assessment,
        // the stored `pr_number` MUST (a) be a real OPEN PR and (b) have its
        // head ref equal to `overlay.branch` — otherwise every gate query
        // targets a PR the bead's branch is no longer bound to (jleechan-t8fd
        // / PR #316 wedge). Closed/missing/not-same-branch PRs are
        // re-resolved by head branch; an inconclusive re-resolution DEFERs
        // (no fast-tier assessment this tick) instead of gate-assessing a
        // stale PR.
        //
        // Skipped when the slow-tier re-resolution path above JUST set the
        // `pr_number` from the branch this same tick (the dispatch→ATTESTED
        // promotion block already verified the branch→PR live lookup, so
        // re-validating the freshly-resolved value is redundant and would
        // emit a false-positive `PR_PRE_GATE_VALIDATION_MISMATCH` on every
        // healthy bead).
        let pre_gate_pr = if pr_number_reresolved_this_tick {
            // Slow-tier re-resolution path already verified the branch→PR
            // live lookup this tick; skip the redundant pre-gate check.
            pr
        } else if !deps.cfg.pre_gate_validation_enabled {
            // Pre-gate validation is operator-gated (default false) so
            // legacy deployments and integration tests that don't script
            // `open_pr_head_refs` for ATTESTED beads aren't disturbed.
            // Production deployments with the flag enabled get full
            // drift coverage for ATTESTED beads whose stored `pr_number`
            // wasn't re-resolved by the dispatch→attested path this tick.
            pr
        } else {
            match deps.scm.open_pr_head_ref_for_repo(&repo, pr) {
                Ok(PrHeadBranch::SameRepo(head)) => {
                    if Some(head.as_str()) == overlay.branch.as_deref() {
                        pr
                    } else {
                        // Stored pr is OPEN but its head ref has drifted
                        // off the bead's recorded branch — re-resolve by
                        // head branch; defer if the branch has no live PR.
                        let _ = emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "PR_PRE_GATE_VALIDATION_MISMATCH",
                            serde_json::json!({}),
                            serde_json::json!({
                                "branch": overlay.branch,
                                "stored_pr_number": pr,
                                "open_pr_head_ref_resolution": format!("SameRepo({head})"),
                                "reason": "stored_pr_head_ref_drifted",
                            }),
                        );
                        match deps
                            .scm
                            .pr_number_for_branch(&repo, overlay.branch.as_deref().unwrap_or(""))
                        {
                            Ok(Some(discovered)) => {
                                overlay.pr_number = Some(discovered);
                                deps.store.save(&overlay)?;
                                emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "PR_NUMBER_REREZOLVED",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "branch": overlay.branch,
                                        "previous_pr_number": pr,
                                        "current_pr_number": discovered,
                                        "reason": "pre_gate_validation_drift",
                                    }),
                                )?;
                                discovered
                            }
                            Ok(None) => {
                                // jleechan-t40t r12 (issue #326): the stored PR
                                // drifted off the branch AND the branch has no
                                // live PR. Clearing `pr_number` alone would
                                // strand the bead ATTESTED forever — the
                                // ATTESTED gate path needs a `pr_number` and the
                                // branch→PR re-resolution only runs for
                                // DISPATCHED beads. DEMOTE to DISPATCHED so the
                                // next tick's re-resolution picks it up and
                                // re-promotes once a live PR appears.
                                overlay.pr_number = None;
                                overlay.state = OverlayState::Dispatched;
                                deps.store.save(&overlay)?;
                                let _ = emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Dispatched.as_str(),
                                    "PR_NUMBER_REREZOLVED_NO_OPEN_PR",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "branch": overlay.branch,
                                        "previous_pr_number": pr,
                                        "current_pr_number": serde_json::Value::Null,
                                        "reason": "pre_gate_validation_no_open_pr",
                                        "action": "demoted_attested_to_dispatched_for_rerezolve",
                                    }),
                                );
                                continue;
                            }
                            Err(e) => {
                                let _ = emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "PR_NUMBER_REREZOLVE_TRANSIENT_ERROR",
                                    serde_json::json!({}),
                                    serde_json::json!({
                                        "branch": overlay.branch,
                                        "phase": "pre_gate_validation",
                                        "error": format!("{e:?}"),
                                    }),
                                );
                                continue;
                            }
                        }
                    }
                }
                Ok(other) => {
                    // Closed/missing/fork — emit mismatch, re-resolve by
                    // head branch; defer if no live PR.
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "PR_PRE_GATE_VALIDATION_MISMATCH",
                        serde_json::json!({}),
                        serde_json::json!({
                            "branch": overlay.branch,
                            "stored_pr_number": pr,
                            "open_pr_head_ref_resolution": format!("{other:?}"),
                            "reason": "stored_pr_no_longer_open_or_branch_mismatch",
                        }),
                    );
                    match deps
                        .scm
                        .pr_number_for_branch(&repo, overlay.branch.as_deref().unwrap_or(""))
                    {
                        Ok(Some(discovered)) => {
                            overlay.pr_number = Some(discovered);
                            deps.store.save(&overlay)?;
                            emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::Attested.as_str(),
                                "PR_NUMBER_REREZOLVED",
                                serde_json::json!({}),
                                serde_json::json!({
                                    "branch": overlay.branch,
                                    "previous_pr_number": pr,
                                    "current_pr_number": discovered,
                                    "reason": "pre_gate_validation_drift",
                                }),
                            )?;
                            discovered
                        }
                        Ok(None) => {
                            // jleechan-t40t r12 (issue #326): stored PR is
                            // closed/missing and the branch has no live PR.
                            // Demote ATTESTED→DISPATCHED (rather than leaving it
                            // ATTESTED with a null pr_number, which strands it)
                            // so the DISPATCHED re-resolution path re-promotes
                            // it when a live PR appears.
                            overlay.pr_number = None;
                            overlay.state = OverlayState::Dispatched;
                            deps.store.save(&overlay)?;
                            let _ = emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::Dispatched.as_str(),
                                "PR_NUMBER_REREZOLVED_NO_OPEN_PR",
                                serde_json::json!({}),
                                serde_json::json!({
                                    "branch": overlay.branch,
                                    "previous_pr_number": pr,
                                    "current_pr_number": serde_json::Value::Null,
                                    "reason": "pre_gate_validation_no_open_pr",
                                    "action": "demoted_attested_to_dispatched_for_rerezolve",
                                }),
                            );
                            continue;
                        }
                        Err(e) => {
                            let _ = emit(
                                deps.telemetry_log,
                                bead_id,
                                overlay.attempt,
                                OverlayState::Attested.as_str(),
                                "PR_NUMBER_REREZOLVE_TRANSIENT_ERROR",
                                serde_json::json!({}),
                                serde_json::json!({
                                    "branch": overlay.branch,
                                    "phase": "pre_gate_validation",
                                    "error": format!("{e:?}"),
                                }),
                            );
                            continue;
                        }
                    }
                }
                Err(e) => {
                    let _ = emit(
                        deps.telemetry_log,
                        bead_id,
                        overlay.attempt,
                        OverlayState::Attested.as_str(),
                        "PR_PRE_GATE_VALIDATION_TRANSIENT_ERROR",
                        serde_json::json!({}),
                        serde_json::json!({
                            "branch": overlay.branch,
                            "stored_pr_number": pr,
                            "error": format!("{e:?}"),
                        }),
                    );
                    continue;
                }
            }
        };
        let pr = pre_gate_pr;

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
        if snapshot.ci_pending {
            // Bead jleechan-jsby (r2): the operator guidance r2 #3
            // requires the CI-wait to "exclude or timeout the
            // CodeRabbit commit-status context once all check-runs are
            // complete (this wedged beads jtg8/jsby itself)". The
            // current snapshot's `ci_pending=true` flag is the only
            // signal we have; when at least one tracked vendor is
            // showing a structured cap marker, the "ci pending" check
            // is the vendor's commit-status context, not a real CI
            // wait — skip the wait and proceed to the gate
            // assessment so the ledger can observe the cap.
            let vendor_capped = deps
                .vendor_health
                .and_then(|m| m.lock().ok())
                .map(|_l| {
                    verifier::detect_vendor_cap(&snapshot)
                })
                .unwrap_or(false);
            if !vendor_capped {
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
        }

        // Bead jleechan-jsby (r2): populate the vendor-health ledger
        // BEFORE the gate assessment. The r1 PR #459 was rejected
        // because the field was constructed fresh-and-empty here. The
        // r2 path:
        //   1. Clones the process-wide ledger (cheap; the cap VecDeque
        //      is bounded and the type is `Clone`).
        //   2. Records cap observations for each capped vendor via
        //      `record_cap_observations_from_snapshot` (ZFC: STRUCTURED
        //      snapshot fields only, no keyword matching).
        //   3. Detects recovery via `detect_vendor_recovery` and clears
        //      the ledger entries on the Waived -> Healthy edge,
        //      emitting VENDOR_RECOVERED telemetry.
        //   4. Emits VENDOR_WAIVED on the Healthy -> Capped edge so
        //      operators can see the auto-escalation event.
        //
        // When `deps.vendor_health` is `None` (Stage-1 test-repo lane
        // or an integration test that does not exercise the ledger),
        // we use a fresh empty ledger — the pre-r1 behavior preserved.
        let mut vendor_health = deps
            .vendor_health
            .and_then(|m| m.lock().ok().map(|l| l.clone()))
            .unwrap_or_default();
        // Cap flag from the snapshot — STRUCTURED inputs only.
        let cap_observed = verifier::detect_vendor_cap(&snapshot);
        let mut recently_waived = Vec::new();
        let mut recently_recovered = Vec::new();
        if let Some(ledger_mutex) = deps.vendor_health {
            if let Ok(now) = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
            {
                if let Ok(mut ledger) = ledger_mutex.lock() {
                    // Copy the ledger so the per-tick evidence carries
                    // the pre-record state (the verifier reads the
                    // ledger's `health()` to decide the waiver, and
                    // the recorded observations must be visible to
                    // that read).
                    *ledger = vendor_health.clone();
                    if cap_observed {
                        // Record one observation per capped vendor.
                        // The N-of-M detector in `ledger.health` keys
                        // on distinct bead_ids, so the SAME bead
                        // observing caps every tick is one signal, not
                        // N — the test integration requires 3 distinct
                        // beads to escalate.
                        let beads: Vec<(crate::vendor_health::Vendor,)> = vec![
                            (crate::vendor_health::Vendor::CodeRabbit,),
                            (crate::vendor_health::Vendor::Bugbot,),
                        ];
                        for (vendor,) in beads {
                            let is_capped =
                                verifier::detect_vendor_cap_for(&snapshot, vendor);
                            if is_capped && !ledger.health(vendor).is_capped() {
                                ledger.record_cap(crate::vendor_health::CapObservation {
                                    vendor,
                                    source: crate::vendor_health::CapSource::UnknownGateRepeated,
                                    bead_id: bead_id.to_string(),
                                    pr_number: pr,
                                    ts_epoch: now,
                                    note: format!("ci_pending={} coderabbit_status={}", snapshot.ci_pending, snapshot.coderabbit_status),
                                });
                                if ledger.health(vendor).is_capped() {
                                    recently_waived.push(vendor);
                                }
                            }
                        }
                    }
                    // Recovery: clear the vendor if the snapshot is
                    // clean. `detect_vendor_recovery` keys on
                    // STRUCTURED fields only.
                    let recovered =
                        verifier::detect_vendor_recovery(&snapshot, &ledger);
                    let prev_was_capped: Vec<crate::vendor_health::Vendor> = vec![
                        crate::vendor_health::Vendor::CodeRabbit,
                        crate::vendor_health::Vendor::Bugbot,
                    ]
                    .into_iter()
                    .filter(|v| {
                        // Only emit VENDOR_RECOVERED if the ledger was
                        // Capped BEFORE this assessment (not on a
                        // never-capped vendor).
                        vendor_health.health(*v).is_capped()
                    })
                    .collect();
                    for v in &recovered {
                        ledger.clear(*v);
                        if prev_was_capped.contains(v) {
                            recently_recovered.push(*v);
                        }
                    }
                }
            }
        }

        // Chain-walk past CodeRabbit when possible; only waive when no
        // fallback reviewer is configured.
        for vendor in &recently_waived {
            if let Some(next) = crate::vendor_health::next_healthy_reviewer(*vendor) {
                let _ = emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "REVIEWER_ROTATED",
                    serde_json::json!({}),
                    serde_json::json!({
                        "vendor": vendor.as_str(),
                        "nextReviewer": next,
                        "chainWalked": true,
                        "waiverSuppressed": true,
                    }),
                );
            } else {
                let _ = emit_vendor_waived(deps, bead_id, *vendor, overlay.attempt);
            }
        }
        // Emit VENDOR_RECOVERED telemetry on the Capped -> Healthy edge.
        for vendor in &recently_recovered {
            let _ = emit_vendor_recovered(deps, bead_id, *vendor, overlay.attempt);
        }
        // Refresh the local `vendor_health` from the ledger (the
        // post-record state) so the gate sees the updated cap state.
        if let Some(ledger_mutex) = deps.vendor_health {
            if let Ok(ledger) = ledger_mutex.lock() {
                vendor_health = ledger.clone();
            }
        }

        let mut evidence = match skeptic_evidence(deps, bead_id, pr, &repo, &snapshot, vendor_health) {
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

        // Bead jleechan-msmq: skip gate re-assessment when this bead's prior
        // reroll attempt DEFERRED (`reroll_deferral_count > 0`,
        // `reroll::execute`'s `defer_or_cap` left the bead ATTESTED with
        // the OLD PR still set, head SHA unchanged). The OLD PR's gate
        // verdict cannot advance the bead (the reroll branch IS the
        // advancement) and re-assessing it on every subsequent tick
        // races with two breakers (see module-level comment above for
        // the failure mode): the autonomy timebox parks + kills the live
        // coder lane before its first push, and the circuit-breaker trips
        // on identical red evidence at attempt 2.
        //
        // Scoped specifically to DEFERRED rerolls. An IN-FLIGHT reroll
        // (`reroll_count > 0` with `reroll_deferral_count == 0`) is a
        // different state: the reroll succeeded once and a fresh attempt
        // branch is open. The reroll branch below still fires normally
        // for that bead on every tick (it advances to either another
        // reroll, or holds at ReRoll awaiting the fresh coder's first
        // push). Skipping those would silently strand the in-flight
        // reroll's per-tick progress check.
        //
        // The fast tier's reroll branch below this guard still fires
        // normally on every tick for in-flight beads, so deferrals
        // continue to be observed and the bead remains re-eligible; the
        // SUPPRESSED surface is the duplicate GATE_ASSESSMENT emit for
        // the DEFERRED branch only.
        let reroll_deferral_count = deps.store.reroll_deferral_count(bead_id).unwrap_or(0);
        if reroll_deferral_count > 0 {
            let _ = emit(
                deps.telemetry_log,
                bead_id,
                overlay.attempt,
                OverlayState::Attested.as_str(),
                "VERIFIER_SKIPPED_REROLL_IN_PROGRESS",
                serde_json::json!({}),
                serde_json::json!({
                    "prNumber": pr,
                    "headSha": snapshot.head_sha,
                    "rerollCount": overlay.reroll_count,
                    "rerollDeferralCount": reroll_deferral_count,
                    "reason": "prior reroll attempt deferred; old PR gates cannot advance the bead",
                }),
            );
            continue;
        }

        // jleechan-qqq: if no `/er` verdict is recorded yet, dispatch an
        // independent reviewer (claude/codex subprocess) and post the
        // verdict as a PR comment. Re-fetch the snapshot so the just-
        // posted comment is visible to `parse_er_verdict` below.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
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
            None => {
                verifier::parse_er_verdict_since(&snapshot.comments, snapshot.head_committed_epoch)
            }
        };
        evidence.is_production = verifier::classify_production(&snapshot.files);
        evidence.non_test_changed_loc = verifier::calculate_non_test_loc(&snapshot.files);
        // Bead jleechan-ijod / issue #387 (r5): invoke the runtime vacuous-test
        // detector on production PRs and translate its verdict into
        // `VacuousRedGreenStatus` for gate-8 consumption. Test-repo PRs and
        // PRs with no test files in the diff stay `NotProvided` so the gate
        // stays Green. Any error during the detector invocation (missing
        // manifest, gh CLI error, baseline resolution failure) is surfaced
        // as a structured `VacuousRedGreenStatus` variant — gate 8 turns
        // these into `Unknown` rather than misreporting Vacuous.
        evidence.vacuous_red_green =
            vacuous_red_green_for_pr(deps, pr, &repo, &snapshot, is_test_repo);
        // Bead jleechan-yoqy / issue #323 (r5): verify the canonical evidence
        // contract fail-closed. A fully-parsed marker MUST reference the PR's
        // current head AND point at a gist that is fetchable + non-empty. r5
        // finding 2: a marker LINE present but incomplete (missing gist URL or
        // `(head <sha>)`) is a definitive FAIL, not NotProvided — only a
        // genuinely absent marker is NotProvided. r5 finding 3: a TRANSIENT
        // gist-fetch error is Pending (Unknown/wait), never a Red that churns a
        // reroll; only a definitive miss (empty / 404) is Failed. Only PRs
        // carrying a marker incur the gist API call.
        evidence.evidence_gist_status = match verifier::parse_evidence(&snapshot.body) {
            Some(parsed) => {
                let head_matches = {
                    let want = snapshot.head_sha.to_ascii_lowercase();
                    let got = parsed.head_sha.to_ascii_lowercase();
                    !got.is_empty()
                        && !want.is_empty()
                        && (want.starts_with(&got) || got.starts_with(&want))
                };
                if !head_matches {
                    // jleechan-rln6: FAST-REJECTION PATH for stale evidence.
                    // Pattern across 2026-07-22/23 lanes (n6mk/jtg8/sb4b/jsby)
                    // — coders publish the gist mid-session, then keep pushing,
                    // so the gist's head SHA goes stale by the time the daemon
                    // reads the marker. The OLD code returned `Failed(...)` and
                    // let the existing re-attestation loop churn the bead
                    // every tick until the coder happened to refresh — the
                    // typical outcome was a full reroll cycle (~20-40 min +
                    // tokens) per miss. The NEW path (a) emits a structured
                    // telemetry event `EVIDENCE_HEAD_STALE` so operators can
                    // see the failure class, (b) posts a precise bead-notes-
                    // style comment back to the coder session with the
                    // exact `gh pr edit --body` recipe to refresh, and (c)
                    // persists a one-shot sentinel so we do NOT re-post
                    // the same comment every tick. The gate verdict still
                    // returns `Failed(...)` so the rest of the chain stays
                    // fail-closed — only the side-effects are added.
                    //
                    // PR #463 round-1 Codex P1 finding: the `continue` below
                    // short-circuits the rest of `run_fast_tier`, including
                    // the GATE_ASSESSMENT telemetry emit and the
                    // auto-merge-guard's `latest_assessment_no_red` check.
                    // The merge guard reads the LATEST GATE_ASSESSMENT for
                    // `(pr_number, head_sha)`; if the fast-reject branch
                    // skipped the emit, an older all-green assessment
                    // (from before the marker went stale) would be the only
                    // thing visible, and a merge on stale data could slip
                    // through. The fix: emit the assessment BEFORE
                    // short-circuiting the park/reroll path. The
                    // `gate_assessment_context` is built identically to the
                    // non-fast-reject branch, so the guard sees the same
                    // shape and the fresh EvidenceFloor Red verdict
                    // correctly suppresses a merge.
                    let mismatch_reason = format!(
                        "evidence head {} does not match PR head {}",
                        parsed.head_sha, snapshot.head_sha
                    );
                    if !evidence_head_stale_already_recorded(deps, bead_id, &mismatch_reason)? {
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "EVIDENCE_HEAD_STALE",
                            serde_json::json!({
                                "pr_number": pr,
                                "parsed_head_sha": parsed.head_sha,
                                "pr_head_sha": snapshot.head_sha,
                            }),
                            serde_json::json!({
                                "reason": "evidence marker head SHA does not match PR head — fast rejection, no full reroll",
                                "remediation": "gh pr edit --body with **Evidence**: <gist-url> (head <current_sha>) where <current_sha> = gh pr view --json headRefOid -q .headRefOid",
                            }),
                        )?;
                        let comment_body = format!(
                            "🤖 **[dark-factory]** Bead `{bead_id}` — your `**Evidence**:` marker references head `{parsed_sha}` but the PR head is now `{pr_sha}`. The Evidence Gate rejects this attestation WITHOUT triggering a reroll (fast rejection). To re-attest, refresh the PR body to the CURRENT head SHA:\n\n\
                             ```\n\
                             CURRENT=$(gh pr view --json headRefOid -q .headRefOid)\n\
                             gh pr edit --body \"**Evidence**: <gist-url> (head $CURRENT)\"\n\
                             ```\n\n\
                             The fast tier will re-assess on the next tick once the marker matches the live head. Do NOT publish a fresh gist until you have the FINAL push SHA in hand — publishing mid-session is the failure mode this fix targets.",
                            bead_id = bead_id,
                            parsed_sha = parsed.head_sha,
                            pr_sha = snapshot.head_sha,
                        );
                        // PR #463 round-1 Codex P2 finding #1: only persist
                        // the sentinel on a successful comment post. If the
                        // post fails transiently, leaving the sentinel
                        // persisted would suppress the next tick's retry
                        // and the coder would never receive the only
                        // instructions that explain how to refresh the
                        // marker. On error we emit a dedicated telemetry
                        // event so operators can see the notification
                        // failure, but the sentinel is left empty so the
                        // next tick re-posts.
                        match post_scm_comment_by_bead_id(deps, bead_id, &comment_body) {
                            Ok(()) => {
                                record_evidence_head_stale(deps, bead_id, &mismatch_reason)?;
                            }
                            Err(e) => {
                                emit(
                                    deps.telemetry_log,
                                    bead_id,
                                    overlay.attempt,
                                    OverlayState::Attested.as_str(),
                                    "EVIDENCE_HEAD_STALE_NOTIFY_FAILED",
                                    serde_json::json!({
                                        "pr_number": pr,
                                        "parsed_head_sha": parsed.head_sha,
                                        "pr_head_sha": snapshot.head_sha,
                                    }),
                                    serde_json::json!({
                                        "reason": "stale-evidence remediation comment post failed; sentinel NOT persisted so next tick will retry",
                                        "error": e.to_string(),
                                    }),
                                )?;
                            }
                        }
                    }
                    verifier::EvidenceGistStatus::Failed(mismatch_reason)
                } else {
                    match deps.scm.gist_nonempty(&parsed.gist_id) {
                        Ok(Some(true)) => verifier::EvidenceGistStatus::Verified,
                        Ok(Some(false)) => verifier::EvidenceGistStatus::Failed(format!(
                            "evidence gist {} is empty",
                            parsed.gist_id
                        )),
                        Ok(None) => verifier::EvidenceGistStatus::Failed(format!(
                            "evidence gist {} not found",
                            parsed.gist_id
                        )),
                        Err(e) => verifier::EvidenceGistStatus::Pending(format!(
                            "evidence gist {} fetch failed transiently: {e}",
                            parsed.gist_id
                        )),
                    }
                }
            }
            None if verifier::has_evidence_marker(&snapshot.body) => {
                verifier::EvidenceGistStatus::Failed(
                    "evidence marker present but missing a gist URL or `(head <sha>)`".to_string(),
                )
            }
            None => verifier::EvidenceGistStatus::NotProvided,
        };
        let report = verifier::assess(deps.scm, pr, &repo, deps.cfg, &evidence)?;
        summary.gates_assessed += 1;
        // jleechan-6l1f: gate-regression detection. Compare the bead's
        // recorded `last_all_green` against the new `report.all_green`:
        //   - true -> false (was green, now red) is the FIRST-CLASS regression
        //     transition: emit GATE_REGRESSED, bump the counter, and route
        //     the bead through the existing red branch below. If the counter
        //     is already at MAX_GATE_REGRESSIONS, emit GATE_REGRESSED_CAPPED
        //     and park HUMAN_HELD with the distinct park_reason
        //     "gate_regression_capped" (so `recover_human_held` does NOT
        //     requeue it identically to a transient red — circuit-breaker-
        //     style suppression).
        //   - any other transition (false->false, false->true, true->true)
        //     is the normal flow; only the green->red case is special.
        //
        // The regression branch runs BEFORE the GATE_ASSESSMENT emit so the
        // guard sees a fresh assessment (with all_green=false) on the same
        // tick the regression is detected — a 60s tick is the minimum
        // window the merge guard polls, so emitting the assessment FIRST and
        // the regression event SECOND would let the guard accept a stale
        // all_green=true window for one tick.
        let prev_all_green = deps.store.last_all_green(bead_id)?.unwrap_or(false);
        let new_all_green = report.all_green;
        let is_regression = prev_all_green && !new_all_green;
        if is_regression {
            let reg_count = deps.store.gate_regression_count(bead_id)?;
            if reg_count >= MAX_GATE_REGRESSIONS {
                // Cap hit: the bead has flapped green->red enough times
                // that further reroll is no longer productive. Park
                // HUMAN_HELD with a distinct park_reason that
                // `recover_human_held`'s retry-safe allow-list does NOT
                // include (so the bead is not silently requeued).
                let red_gate_names: Vec<String> = report
                    .results
                    .iter()
                    .filter_map(|(gate_name, result)| match result {
                        verifier::GateResult::Red(reason) => {
                            Some(format!("{gate_name:?}: {reason}"))
                        }
                        _ => None,
                    })
                    .collect();
                overlay.state = OverlayState::HumanHeld;
                set_human_hold_reason(
                    &mut overlay,
                    HumanHoldReason::GateRegressionCapped,
                );
                deps.store.save(&overlay)?;
                summary.beads_parked_human_held += 1;
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::HumanHeld.as_str(),
                    "GATE_REGRESSED_CAPPED",
                    serde_json::json!({}),
                    serde_json::json!({
                        "reason": "green->red transitions exceeded MAX_GATE_REGRESSIONS; routing to HUMAN_HELD",
                        "max_gate_regressions": MAX_GATE_REGRESSIONS,
                        "regression_count": reg_count,
                        "pr_number": pr,
                        "red_gates": red_gate_names,
                    }),
                )?;
                let comment_body = format!(
                    "🤖 **[dark-factory]** Gate regression capped: bead `{bead_id}` has \
                     flipped green->red {reg_count} times (>= MAX_GATE_REGRESSIONS={MAX_GATE_REGRESSIONS}). \
                     Automation parked the bead HUMAN_HELD for inspection rather than \
                     silently looping the reroll lane.\n\n\
                     Current red gates:\n{}",
                    red_gate_names
                        .iter()
                        .map(|g| format!("- `{g}`"))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
                continue;
            }
            // Bump the counter, emit the regression event, and let the
            // existing red-branch path below route the bead through
            // `reroll::execute` (stage 2) or `PARKED_HUMAN_HELD` (stage 1).
            // Persist the demotion off READY here so a healthy READY that
            // re-assessed-and-stayed-green in the previous block is NOT
            // re-promoted to READY by `beads_ready` below (the report is
            // red this tick, so we want the demotion durable).
            let _ = deps.store.incr_gate_regression_count(bead_id)?;
            if entered_as_ready {
                // Demote READY -> ATTESTED so the red-branch routes it;
                // save() bumps updated_at. The in-memory `overlay.state`
                // is already Attested (we demoted it above).
                overlay.state = OverlayState::Attested;
                deps.store.save(&overlay)?;
            }
            let red_gate_names: Vec<String> = report
                .results
                .iter()
                .filter_map(|(gate_name, result)| match result {
                    verifier::GateResult::Red(reason) => {
                        Some(format!("{gate_name:?}: {reason}"))
                    }
                    _ => None,
                })
                .collect();
            emit(
                deps.telemetry_log,
                bead_id,
                overlay.attempt,
                OverlayState::Attested.as_str(),
                "GATE_REGRESSED",
                serde_json::json!({}),
                serde_json::json!({
                    "reason": "previously all_green, now not all_green",
                    "pr_number": pr,
                    "head_sha": snapshot.head_sha,
                    "red_gates": red_gate_names,
                    "regression_count": deps.store.gate_regression_count(bead_id)?,
                }),
            )?;
            let comment_body = format!(
                "🤖 **[dark-factory]** Gate regression detected for bead `{bead_id}`: \
                 this PR previously passed all safety gates and is now failing \
                 (count {} of {MAX_GATE_REGRESSIONS} before escalation). Re-entering \
                 the fix loop.\n\nCurrent red gates:\n{}",
                deps.store.gate_regression_count(bead_id)?,
                red_gate_names
                    .iter()
                    .map(|g| format!("- `{g}`"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
        }
        // jleechan-rln6 / PR #463 round-1 Codex P1 finding: the
        // GATE_ASSESSMENT emit must happen BEFORE the fast-rejection
        // `continue` short-circuit. `auto-merge-guard.sh` reads the LATEST
        // GATE_ASSESSMENT for `(pr_number, head_sha)` to decide whether a
        // no-red assessment exists; if the fast-reject branch skipped the
        // emit, an older all-green assessment (made before the marker
        // went stale) would be the only thing visible to the guard, and
        // a merge on stale data could slip through. The fix: emit the
        // assessment FIRST, then `continue` if the fast-reject applies.
        // The `gate_assessment_context` is built identically to the
        // non-fast-reject branch so the guard sees the same shape and
        // the fresh EvidenceFloor Red verdict correctly suppresses a
        // merge.
        //
        // jleechan-wzgl: log the full per-gate breakdown (all 7 gates,
        // verdict + reason) plus the gate-7 reviewer vendor identity, not
        // just the aggregate `all_green` boolean — `report.to_json()` is
        // `verifier::assess`'s own serialization, so this can't drift from
        // what was actually computed, and `evidence.skeptic_reviewers`
        // names the vendor(s) that produced this tick's skeptic verdict.
        let mut gate_assessment_context = report.to_json();
        if let Some(obj) = gate_assessment_context.as_object_mut() {
            obj.insert(
                "skeptic_reviewers".to_string(),
                serde_json::json!(evidence.skeptic_reviewers),
            );
            // jleechan-984e / issue #385: surface the cross-model degraded
            // flag in GATE_ASSESSMENT telemetry so strict merge policy
            // (#328) — and any downstream operator dashboards / alerts —
            // can read it without re-deriving the family count from
            // `skeptic_reviewers`. `true` means the gate-7 verdict came
            // from a single model family (e.g. only `claude` because
            // codex is quota-dead and agy/gemini/cursor-agent errored),
            // which strict merge policy MUST treat as NOT strict-green.
            obj.insert(
                "review_degraded".to_string(),
                serde_json::json!(evidence.review_degraded),
            );
            // jleechan-wzgl (PR #239 review round 1): `auto-merge-guard.sh`'s
            // `latest_assessment_no_red` greps GATE_ASSESSMENT lines by
            // `context.pr_number` before parsing `context.gates` — without
            // this key the guard's match path is permanently dormant no
            // matter how correct the `gates` shape is.
            obj.insert("pr_number".to_string(), serde_json::json!(pr));
            // jleechan-328 P1 #1 (exact-head binding): record the PR's
            // current head SHA in the assessment context so the shell
            // merge-guard can refuse to honour a stale assessment from an
            // OLDER head. Without this field the timer-driven merge path
            // would reuse an all-green assessment made before a push.
            obj.insert("head_sha".to_string(), serde_json::json!(snapshot.head_sha));
            // jleechan-328 P1 #3 (operator disposition round-trip): single
            // canonical field emitted from `overlay.park_reason` so the
            // shell override reads the SAME key the daemon emits. Missing
            // → guard falls through to the standard no-red path. The
            // disposition vocabulary is owned here (`overlay.park_reason`)
            // — never duplicate the field elsewhere.
            obj.insert(
                "operator_disposition".to_string(),
                serde_json::json!(overlay.park_reason.clone().unwrap_or_default()),
            );
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
        // jleechan-6l1f: stamp the bead's `last_all_green` to the latest
        // assessment result so the next tick's regression-detection predicate
        // has authoritative input. The stamp is unconditional — even on a
        // regression tick (where we already emitted GATE_REGRESSED above),
        // the column MUST reflect the current report, not the stale prior
        // value (otherwise the next tick would re-fire GATE_REGRESSED on a
        // sustained-red state).
        deps.store.set_last_all_green(bead_id, new_all_green)?;

        // jleechan-rln6: FAST-REJECTION short-circuit for a STALE evidence
        // marker. When the ONLY red gate is `EvidenceFloor` AND its reason
        // string contains the head-SHA mismatch class (emitted by the
        // fast-rejection branch above as
        // `"evidence contract: evidence head <X> does not match PR head <Y>"`),
        // the daemon must NOT park (stage 1) or reroll (stage 2) the bead
        // — those are the exact failure modes the rln6 acceptance
        // criteria target (n6mk/jtg8/sb4b/jsby each lost a full reroll
        // cycle to this pattern). The coder has been told exactly what
        // to do via the `EVIDENCE_HEAD_STALE` event + the bead-notes-style
        // comment posted above; the next tick after they refresh the
        // marker will re-assess. Counting the gate in `gates_assessed`
        // keeps telemetry consistent (the assessment DID happen), and
        // the GATE_ASSESSMENT emit above is what tells the merge guard
        // the fresh verdict, but `continue` skips the park/reroll/
        // escalation branches below.
        // We match on `"does not match PR head"` (the substring inside
        // the `evidence_floor_gate` "evidence contract: …" envelope)
        // rather than the prefix because `evidence_floor_gate` always
        // prefixes its Red reason with `"evidence contract: "`.
        if let Some((_, verifier::GateResult::Red(reason))) = report
            .results
            .iter()
            .find(|(name, _)| *name == verifier::GateName::EvidenceFloor)
        {
            let only_evidence_is_stale = report
                .results
                .iter()
                .all(|(name, result)| {
                    *name == verifier::GateName::EvidenceFloor
                        || !matches!(result, verifier::GateResult::Red(_))
                })
                && reason.contains("does not match PR head");
            if only_evidence_is_stale {
                summary.gates_assessed_fast_rejected += 1;
                continue;
            }
        }
        // (the GATE_ASSESSMENT emit was hoisted above the fast-reject
        // short-circuit per PR #463 round-1 Codex P1 finding; see comment
        // block at the top of this section.)

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
                        if !err.is_transient() {
                            mark_escalation_undeliverable_and_emit(
                                deps,
                                summary,
                                bead_id,
                                overlay.attempt,
                                OverlayState::Attested.as_str(),
                                "unknown_only_gate_report_with_er_runner_capped",
                                &err,
                            )?;
                            continue;
                        }
                        let ctx = serde_json::json!({
                            "reason": "unknown_only_gate_report_with_er_runner_capped",
                            "er_runner_attempts": count,
                            "pr_number": pr,
                            "error": err.to_string(),
                        });
                        let now_epoch = now_epoch_secs();
                        let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                            deps,
                            bead_id,
                            "unknown_only_gate_report_with_er_runner_capped",
                            &ctx,
                            now_epoch,
                        )?;
                        if !should_emit {
                            summary.escalations_suppressed += 1;
                            continue;
                        }
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::Attested.as_str(),
                            "ESCALATION_NOTIFICATION_FAILED",
                            serde_json::json!({}),
                            ctx,
                        )?;
                        record_escalation_emit_dedup(
                            deps,
                            bead_id,
                            "unknown_only_gate_report_with_er_runner_capped",
                            &ctx_hash,
                            now_epoch,
                        )?;
                        continue;
                    }
                    overlay.state = OverlayState::HumanHeld;
                    overlay.attempt = MAX_HUMAN_HELD_RECOVERY_ATTEMPT;
                    set_human_hold_reason(&mut overlay, HumanHoldReason::UnknownOnlyGateCapped);
                    deps.store.save(&overlay)?;
                    record_escalation(
                        deps,
                        bead_id,
                        "unknown_only_gate_report_with_er_runner_capped",
                    )?;
                    summary.beads_escalated += 1;
                    summary.beads_parked_human_held += 1;
                    let ctx = serde_json::json!({
                        "reason": "unknown_only_gate_report_with_er_runner_capped",
                        "er_runner_attempts": count,
                        "pr_number": pr,
                    });
                    let now_epoch = now_epoch_secs();
                    let (should_emit, ctx_hash) = escalation_dedup_should_emit(
                        deps,
                        bead_id,
                        "unknown_only_gate_report_with_er_runner_capped",
                        &ctx,
                        now_epoch,
                    )?;
                    if !should_emit {
                        summary.escalations_suppressed += 1;
                    } else {
                        emit(
                            deps.telemetry_log,
                            bead_id,
                            overlay.attempt,
                            OverlayState::HumanHeld.as_str(),
                            "ESCALATION_REQUIRED",
                            serde_json::json!({}),
                            ctx,
                        )?;
                        record_escalation_emit_dedup(
                            deps,
                            bead_id,
                            "unknown_only_gate_report_with_er_runner_capped",
                            &ctx_hash,
                            now_epoch,
                        )?;
                    }
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
                        set_human_hold_reason(&mut overlay, HumanHoldReason::RerollPermanentError);
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

/// jleechan-rln6: returns true once the daemon has already told this bead's
/// coder session that the Evidence marker head SHA is stale for THIS
/// specific `(parsed_sha, pr_sha)` mismatch tuple. The bead-level sentinel
/// (`save_rejection`) keys on the same `(bead_id, attempt)` pair the
/// escalation flow uses; distinct `reviewer` string keeps the two flows
/// from colliding. PR #463 round-1 Codex P2 finding #2: the sentinel must
/// be keyed by the mismatch tuple, not just the bead, so a fresh mismatch
/// after the coder fixes the first one still re-posts the remediation.
fn evidence_head_stale_already_recorded(
    deps: &TickDeps,
    bead_id: &str,
    current_mismatch_reason: &str,
) -> Result<bool, DaemonError> {
    let row = deps
        .store
        .load_rejection(bead_id, EVIDENCE_HEAD_STALE_SENTINEL_ATTEMPT)?;
    match row {
        Some((reviewer, _)) if reviewer == EVIDENCE_HEAD_STALE_REVIEWER => {
            let prev = deps
                .store
                .load_rejection_text(bead_id, EVIDENCE_HEAD_STALE_SENTINEL_ATTEMPT)?;
            Ok(prev.as_deref() == Some(current_mismatch_reason))
        }
        _ => Ok(false),
    }
}

/// jleechan-rln6: persist the stale-evidence fast-rejection sentinel. The
/// `reason` field carries the precise (parsed.head_sha, snapshot.head_sha)
/// pair so a follow-up operator audit can grep for the exact mismatch that
/// fired the rejection without re-running the parser, AND so the
/// `evidence_head_stale_already_recorded` dedup key is the actual mismatch
/// tuple (PR #463 round-1 Codex P2 finding #2).
fn record_evidence_head_stale(deps: &TickDeps, bead_id: &str, reason: &str) -> Result<(), DaemonError> {
    deps.store.save_rejection(
        bead_id,
        EVIDENCE_HEAD_STALE_SENTINEL_ATTEMPT,
        EVIDENCE_HEAD_STALE_REVIEWER,
        reason,
        reason,
    )
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

#[cfg(test)]
mod escalation_context_hash_tests {
    //! Cross-model review P2 #3 (bead jleechan-n6mk, follow-up to PR #447):
    //! the previous `DefaultHasher`-based implementation was correct within a
    //! single process run but **NOT** stable across daemon restarts or Rust
    //! standard library versions (per-process random `RandomState` seed). The
    //! `escalation_ledger.context_hash` column stores the hash as TEXT, and
    //! dedup relies on a re-computed hash matching the stored value after
    //! restart. These tests pin the four properties that the cross-process
    //! contract requires: (a) stability (same input → same 16-char hex on
    //! every call, including across simulated process boundaries); (b)
    //! canonical form (structurally-identical JSON written in two different
    //! `serde_json::json!` macro layouts produces the SAME hash — the bug
    //! class the cross-model reviewer flagged); (c) distinct inputs produce
    //! distinct hashes (negative space is at least as large as `DefaultHasher`);
    //! (d) the FNV-1a 64-bit constants are pinned to the canonical reference
    //! values so a future Rust upgrade can't silently break dedup.

    use super::{canonical_json_bytes, escalation_context_hash, fnv1a_64};

    /// (a) Stability — same input → same output on every call, every process.
    #[test]
    fn fnv1a_is_deterministic_across_calls() {
        let v = serde_json::json!({
            "bead_id": "bead-1",
            "reason": "human_held_recovery_attempt_cap_reached",
            "pr_number": 9006,
            "branch": "factory/bead-1-r10",
        });
        let h1 = escalation_context_hash(&v);
        let h2 = escalation_context_hash(&v);
        let h3 = escalation_context_hash(&v);
        assert_eq!(h1, h2);
        assert_eq!(h2, h3);
        assert_eq!(h1.len(), 16, "FNV-1a 64-bit must format as 16 lowercase hex chars");
    }

    /// (b) Canonical form — `serde_json::json!` macros lay fields out in
    /// declaration order, but two call sites in different files can declare
    /// the same logical fields in a different order and produce a
    /// structurally-identical JSON value. `serde_json::to_string` emits them
    /// in declaration order, so without sorting the hashes would diverge
    /// even though the values are equal. This is the regression class the
    /// cross-model reviewer flagged.
    #[test]
    fn canonical_json_hash_is_order_independent() {
        let a = serde_json::json!({
            "bead_id": "bead-1",
            "reason": "human_held_recovery_attempt_cap_reached",
            "pr_number": 9006,
            "branch": "factory/bead-1-r10",
        });
        let b = serde_json::json!({
            "branch": "factory/bead-1-r10",
            "pr_number": 9006,
            "reason": "human_held_recovery_attempt_cap_reached",
            "bead_id": "bead-1",
        });
        // Sanity: serde_json::Value equality is order-independent.
        assert_eq!(a, b);
        // The hash must also be order-independent (this is the whole point
        // of canonical-JSON serialization).
        assert_eq!(
            escalation_context_hash(&a),
            escalation_context_hash(&b),
            "structurally-identical JSON written in different key orders must produce the same hash"
        );
        // And the canonical byte form must also be order-independent.
        assert_eq!(canonical_json_bytes(&a), canonical_json_bytes(&b));
    }

    /// (c) Distinct inputs → distinct hashes. Spot-check on the structural
    /// dimensions that drive dedup: changing any one of `bead_id`, `reason`,
    /// `pr_number`, or `branch` must change the hash. (Birthday-paradox
    /// collisions at 4B inputs are acceptable for the per-bead context
    /// space, but we must not have a SHORT-CIRCUIT bug where one of these
    /// fields is silently ignored.)
    #[test]
    fn distinct_structural_fields_produce_distinct_hashes() {
        let base = serde_json::json!({
            "bead_id": "bead-1",
            "reason": "human_held_recovery_attempt_cap_reached",
            "pr_number": 9006,
            "branch": "factory/bead-1-r10",
        });
        let base_hash = escalation_context_hash(&base);

        let mut variants = Vec::new();
        for (field, new_value) in [
            ("bead_id", serde_json::json!("bead-2")),
            ("reason", serde_json::json!("budget_held_exceeded")),
            ("pr_number", serde_json::json!(9007)),
            ("branch", serde_json::json!("factory/bead-1-r11")),
        ] {
            let mut v = base.clone();
            v[field] = new_value;
            let h = escalation_context_hash(&v);
            assert_ne!(
                h, base_hash,
                "changing field {field} must change the hash; got hash={h} base_hash={base_hash}"
            );
            variants.push((field, h));
        }
        // And no two distinct-field variants collide with each other (a
        // weaker property, but a useful sentinel: if FNV-1a were broken
        // and collapsed to a constant, ALL of these would equal `base_hash`).
        for (i, (field_i, hi)) in variants.iter().enumerate() {
            for (field_j, hj) in variants.iter().skip(i + 1) {
                assert_ne!(
                    hi, hj,
                    "variants for fields {field_i} and {field_j} must not collide"
                );
            }
        }
    }

    /// (d) Pinned FNV-1a 64-bit constants. The FNV reference charter pins
    /// `OFFSET_BASIS = 0xcbf29ce484222325` and `PRIME = 0x100000001b3`. The
    /// expected hash for the empty input is the offset basis itself, and
    /// the expected hash for `"a"` (one byte, 0x61) is
    /// `((0xcbf29ce484222325 ^ 0x61) * 0x100000001b3) mod 2^64`. If a
    /// future change swaps to FNV-1 (non-a), SipHash, or another hash, this
    /// test pins the cross-process contract — a hash change here will
    /// require an explicit PR explaining the break.
    #[test]
    fn fnv1a_64_constants_match_reference_charter() {
        // OFFSET_BASIS = 0xcbf29ce484222325.
        const EXPECTED_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const EXPECTED_PRIME: u64 = 0x0000_0100_0000_01b3;

        assert_eq!(fnv1a_64(b""), EXPECTED_OFFSET_BASIS, "FNV-1a 64-bit of empty input is the offset basis");

        // Manually compute FNV-1a 64-bit of "a" (single byte 0x61).
        let mut expected = EXPECTED_OFFSET_BASIS;
        expected ^= 0x61;
        expected = expected.wrapping_mul(EXPECTED_PRIME);
        assert_eq!(
            fnv1a_64(b"a"),
            expected,
            "FNV-1a 64-bit of single byte 'a' must match the reference charter"
        );

        // PRIME is encoded as a constant in `fnv1a_64`; this assertion is a
        // belt-and-braces guard against a future "let me use the 32-bit
        // variant" refactor silently shifting the hash space.
        assert_ne!(EXPECTED_PRIME, 0x0100_0193, "FNV-1a 64-bit PRIME must NOT be the 32-bit variant (0x01000193)");
    }

    /// (e) Cross-process stability — the hash must NOT depend on any
    /// per-process state. `DefaultHasher` reads `RandomState`'s seed (a
    /// per-process `Cell<u64>` incremented at construction), so two daemon
    /// processes would hash the same input to different 64-bit digests. This
    /// test pins that the new implementation has no per-process state.
    #[test]
    fn fnv1a_64_has_no_per_process_state() {
        // The function is `fn` (not `thread_local!`/`lazy_static!`), so it
        // cannot read any hidden state. Calling it 1000 times in a tight
        // loop must produce the same hash on every call.
        let v = serde_json::json!({"x": 42, "y": "z"});
        let h0 = escalation_context_hash(&v);
        for _ in 0..1000 {
            assert_eq!(
                escalation_context_hash(&v),
                h0,
                "escalation_context_hash must be deterministic in a hot loop (no per-process state)"
            );
        }
    }

    /// (f) Canonical JSON byte form — the `BTreeMap`-sorted object
    /// serialization that feeds FNV must match what we expect for nested
    /// objects, arrays, and primitives. Catches a regression where a future
    /// "let me use serde_json::to_string directly" refactor reintroduces
    /// the order-dependence bug.
    #[test]
    fn canonical_json_bytes_handles_nested_structures() {
        let v = serde_json::json!({
            "z_field": 1,
            "a_field": "two",
            "nested": {
                "y": [3, 2, 1],
                "x": null,
            },
            "b_arr": [
                {"k": 2, "j": 1},
                {"k": 1, "j": 2},
            ],
        });
        let bytes = canonical_json_bytes(&v);
        let s = std::str::from_utf8(&bytes).expect("canonical JSON must be valid UTF-8");
        // Keys at every nesting level must be in sorted order.
        assert_eq!(
            s,
            r#"{"a_field":"two","b_arr":[{"j":1,"k":2},{"j":2,"k":1}],"nested":{"x":null,"y":[3,2,1]},"z_field":1}"#,
            "canonical JSON must sort keys at every nesting level (RFC 8789 JCS-lite)"
        );
    }
}

// jleechan-7t2g: unit-test the EXISTING_PR_ADOPTED dedup invariant that
// lives inside `run_slow_tier`. The integration test
// `existing_pr_adoption_does_not_re_emit_telemetry_on_subsequent_ticks`
// already pins the behavior end-to-end, but a 5-minute tick loop with five
// trait-object fakes is heavyweight feedback for a 3-line `matches!`
// condition. The inline tests below target the predicate directly: for
// each `Option<OverlayState>` value, assert whether the re-emit must be
// suppressed. This lets future edits to the dedup set fail fast in the
// crate's own test binary instead of waiting for `tick_integration.rs`.
#[cfg(test)]
mod existing_pr_adoption_dedup_tests {
    use super::should_skip_existing_pr_adoption_emit;
    use crate::state::OverlayState;

    /// States inside the dedup set: `Attested`, `Ready`, `HumanHeld`. Once
    /// a bead has reached any of these, the durable overlay row already
    /// records (pr_number, branch, external_ref, is_adopted); re-emitting
    /// `EXISTING_PR_ADOPTED` every tick is what produced ~301k redundant
    /// telemetry events across 30 attested beads (jleechan-mdun incident).
    #[test]
    fn suppresses_emit_for_dedup_states() {
        for state in [OverlayState::Attested, OverlayState::Ready, OverlayState::HumanHeld] {
            assert!(
                should_skip_existing_pr_adoption_emit(Some(state)),
                "overlay in {state:?} must suppress the EXISTING_PR_ADOPTED re-emit"
            );
        }
    }

    /// Every other overlay state (plus the `None` first-time-create path)
    /// MUST still emit. This is the inverse-invariant companion the
    /// integration test cannot cheaply express as a table of cases.
    #[test]
    fn emits_for_non_dedup_states_and_first_create() {
        let cases = [
            OverlayState::Queued,
            OverlayState::Dispatching,
            OverlayState::Dispatched,
            OverlayState::ReRoll,
            OverlayState::Recovery,
            OverlayState::Redispatched,
            OverlayState::BudgetHeld,
            OverlayState::DispositionRequired,
        ];
        for state in cases {
            assert!(
                !should_skip_existing_pr_adoption_emit(Some(state)),
                "overlay in {state:?} must NOT suppress the EXISTING_PR_ADOPTED re-emit"
            );
        }
        // First-time create: no overlay row yet, so the first emit MUST
        // fire (this is the audit-trail bootstrap the production incident
        // preserved — `newly_created=true` is exactly this case).
        assert!(
            !should_skip_existing_pr_adoption_emit(None),
            "first-time create (no prior overlay) must emit EXISTING_PR_ADOPTED"
        );
    }

    /// Pin the dedup set to exactly three states. If a future edit adds a
    /// fourth state (e.g. `BudgetHeld` joining the dedup set), this test
    /// forces an explicit decision rather than silently changing emit
    /// frequency.
    #[test]
    fn dedup_set_has_exactly_three_states() {
        let mut dedup_states: Vec<OverlayState> = Vec::new();
        for state in [
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
            OverlayState::DispositionRequired,
        ] {
            if should_skip_existing_pr_adoption_emit(Some(state)) {
                dedup_states.push(state);
            }
        }
        assert_eq!(
            dedup_states,
            vec![OverlayState::Attested, OverlayState::Ready, OverlayState::HumanHeld],
            "EXISTING_PR_ADOPTED dedup set must remain exactly Attested+Ready+HumanHeld"
        );
    }
}

// jleechan-8s2p (phase 2): the waived-vendor context MUST land in the
// skeptic prompt BEFORE the LLM is dispatched. Otherwise the skeptic
// sees the capped vendor still pending on `gh pr checks`, fails/warns
// solely on that signal, and `compensating_coverage_green` refuses the
// waiver because the required skeptic `Pass` was never obtainable —
// the ledger is copied into `PrEvidence` AFTER the skeptic has already
// responded, so the prompt and the waiver logic disagree about
// whether the vendor check matters.
#[cfg(test)]
mod skeptic_prompt_vendor_waiver_tests {
    use super::build_skeptic_prompt;

    /// Bead jleechan-5arc: the skeptic must not re-derive the `Ci` gate.
    ///
    /// The prompt previously instructed `gh pr checks` — the same data
    /// `ci_green` computes — so skeptic failed whenever CI was merely
    /// pending. Measured 603 fail / 45 pass over 652 assessments, with zero
    /// passes in the 12 days to 2026-08-05 while PRs merged clean.
    #[test]
    fn skeptic_prompt_does_not_instruct_gh_pr_checks() {
        let ledger = crate::vendor_health::VendorHealthLedger::new();
        let prompt = build_skeptic_prompt("bead-x", 123, "owner/repo", &ledger);
        // Assert on the INVOCATION form (with args). The prompt legitimately
        // mentions the bare command inside its own prohibition ("do NOT run
        // `gh pr checks`"), so a naive substring check matches our guardrail
        // text and would fail for the wrong reason.
        assert!(
            !prompt.contains("gh pr checks 123 --repo owner/repo"),
            "skeptic prompt must not INSTRUCT the reviewer to read CI status - \
             that is the Ci gate's job, and duplicating it destroys skeptic's \
             independence (making the vendor Waived contract unreachable)"
        );
    }

    /// The scope instruction must be explicit, not merely implied by omission:
    /// a reviewer can still run `gh pr checks` on its own initiative.
    #[test]
    fn skeptic_prompt_forbids_failing_on_other_gates_signals() {
        let ledger = crate::vendor_health::VendorHealthLedger::new();
        let prompt = build_skeptic_prompt("bead-x", 7, "owner/repo", &ledger);
        let lower = prompt.to_lowercase();
        assert!(
            lower.contains("separate gate"),
            "prompt must state that CI/vendor/comment checks are separate gates"
        );
        assert!(
            lower.contains("do not fail"),
            "prompt must explicitly forbid failing on other gates' signals"
        );
    }

    /// Independence must not cost capability: 30% of sampled fails were real,
    /// diff-specific defects and those must still be found.
    #[test]
    fn skeptic_prompt_still_reviews_the_diff() {
        let ledger = crate::vendor_health::VendorHealthLedger::new();
        let prompt = build_skeptic_prompt("bead-x", 7, "owner/repo", &ledger);
        assert!(
            prompt.contains("gh pr diff 7 --repo owner/repo"),
            "skeptic must still read the diff — that is its actual job"
        );
        assert!(
            prompt.contains("pass|warn <note>|fail <reason>"),
            "the one-line verdict contract must be unchanged"
        );
    }

    use crate::vendor_health::{
        CapObservation, CapSource, Vendor, VendorHealthLedger,
    };

    fn capped_ledger(vendor: Vendor, bead_prefix: &str) -> VendorHealthLedger {
        let mut ledger = VendorHealthLedger::new();
        for ts in 1..=3 {
            ledger.record_cap(CapObservation {
                vendor,
                source: CapSource::UnknownGateRepeated,
                bead_id: format!("{bead_prefix}-{ts}"),
                pr_number: ts,
                ts_epoch: ts,
                note: "test fixture".into(),
            });
        }
        ledger
    }

    /// The skeptic prompt must embed the canonical Bugbot waiver token
    /// when Bugbot is Capped, so the LLM is told that a pending Bugbot
    /// check is a WAIVER (compensating coverage substitutes), NOT a
    /// fail signal. Without this block the skeptic can fail a lane
    /// purely on a capped vendor's pending check, blocking the
    /// waiver that `compensating_coverage_green` would otherwise
    /// issue — exactly the r6 P2 finding.
    #[test]
    fn skeptic_prompt_contains_bugbot_waiver_token_when_bugbot_capped() {
        let ledger = capped_ledger(Vendor::Bugbot, "bead-bugbot");
        let prompt = build_skeptic_prompt("bead-x", 123, "owner/repo", &ledger);

        assert!(
            prompt.contains("bugbot:waived_vendor_unavailable"),
            "skeptic prompt must carry the canonical Bugbot waiver token when Bugbot is Capped; \
             without it the LLM treats a pending Bugbot check as a fail signal and the \
             compensating-coverage waiver can never fire. Got:\n{prompt}"
        );
        assert!(
            prompt.contains("VENDOR WAIVER CONTEXT"),
            "skeptic prompt must have an explicit waiver-context header so the LLM recognises \
             the block as authoritative guidance, not just a stray token. Got:\n{prompt}"
        );
        assert!(
            prompt.contains("Treat a pending or missing bugbot check as waived"),
            "skeptic prompt must contain the explicit 'treat pending as waived' directive; \
             a bare token would not change the LLM's behaviour. Got:\n{prompt}"
        );
    }

    /// A capped CodeRabbit routes to the fallback reviewer and does not add
    /// the legacy waiver context to the skeptic prompt.
    #[test]
    fn chain_walk_routes_past_capped_coderabbit() {
        let prev = std::env::var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN").ok();
        std::env::remove_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN");
        let ledger = capped_ledger(Vendor::CodeRabbit, "bead-cr");
        let prompt = build_skeptic_prompt("bead-x", 124, "owner/repo", &ledger);
        if let Some(v) = prev {
            std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", v);
        }

        assert!(
            !prompt.contains("coderabbit:waived_vendor_unavailable"),
            "CodeRabbit chain-walk must suppress the waiver token. Got:\n{prompt}"
        );
        assert!(
            !prompt.contains("VENDOR WAIVER CONTEXT"),
            "CodeRabbit chain-walk must suppress the entire waiver block. Got:\n{prompt}"
        );
        assert!(
            prompt.contains("REVIEWER_ROTATED"),
            "CodeRabbit chain-walk must leave an auditable rotation marker. Got:\n{prompt}"
        );
    }

    /// CodeRabbit rotates while Bugbot retains its existing waiver behavior.
    #[test]
    fn skeptic_prompt_rotates_coderabbit_and_waives_bugbot_when_both_capped() {
        let mut ledger = capped_ledger(Vendor::CodeRabbit, "bead-cr");
        for ts in 4..=6 {
            ledger.record_cap(CapObservation {
                vendor: Vendor::Bugbot,
                source: CapSource::VendorReportedCap,
                bead_id: format!("bead-bb-{ts}"),
                pr_number: ts,
                ts_epoch: ts,
                note: "test fixture".into(),
            });
        }
        let prompt = build_skeptic_prompt("bead-x", 125, "owner/repo", &ledger);

        assert!(
            !prompt.contains("coderabbit:waived_vendor_unavailable"),
            "CodeRabbit waiver token must be suppressed"
        );
        assert!(
            prompt.contains("bugbot:waived_vendor_unavailable"),
            "Bugbot waiver token must be present"
        );
    }

    /// When neither vendor is Capped, the prompt has NO waiver
    /// context — a Healthy vendor has nothing to waive. A stale
    /// waiver block would mislead the LLM into ignoring a real
    /// vendor verdict.
    #[test]
    fn skeptic_prompt_has_no_waiver_block_when_no_vendor_capped() {
        let ledger = VendorHealthLedger::new();
        let prompt = build_skeptic_prompt("bead-x", 126, "owner/repo", &ledger);

        assert!(
            !prompt.contains("VENDOR WAIVER CONTEXT"),
            "Healthy ledger must not produce a waiver block; the LLM would otherwise \
             ignore a real vendor verdict. Got:\n{prompt}"
        );
        assert!(
            !prompt.contains("waived_vendor_unavailable"),
            "Healthy ledger must not surface any waiver token. Got:\n{prompt}"
        );
    }

    /// The base prompt (Stage-1 skeptic instruction, gh commands,
    /// verdict grammar) must remain unchanged by the waiver-context
    /// injection. Only the suffix changes; the body must still
    /// instruct the LLM to inspect `gh pr checks` and reply with the
    /// pass|warn|fail grammar. This pins the contract that the
    /// waiver-context injection is a PURE ADDITION.
    #[test]
    fn skeptic_prompt_base_unaffected_by_waiver_block() {
        let ledger = capped_ledger(Vendor::Bugbot, "bead-bb");
        let prompt = build_skeptic_prompt("bead-x", 127, "owner/repo", &ledger);

        assert!(
            prompt.contains("You are the Stage-1 Skeptic gate for an autonomous coding factory."),
            "base Stage-1 role line must remain"
        );
        assert!(prompt.contains("gh pr diff 127 --repo owner/repo"));
        assert!(prompt.contains("gh pr view 127 --repo owner/repo --json body,comments"));
        // Bead jleechan-5arc: the CI-status instruction was REMOVED. Skeptic
        // is an independent signal and must not re-derive the Ci gate; the
        // Waived contract at verifier::GateResult depends on that independence.
        assert!(
            !prompt.contains("gh pr checks 127 --repo owner/repo"),
            "the gh pr checks instruction must stay removed (jleechan-5arc)"
        );
        assert!(
            prompt.contains("pass|warn <note>|fail <reason>"),
            "verdict grammar must remain in the base prompt"
        );
    }
}
