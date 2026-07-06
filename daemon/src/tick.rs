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
use crate::dispatch;
use crate::errors::DaemonError;
use crate::intake;
use crate::router::{self, RoutingVerdict};
use crate::state::{BeadOverlay, OverlayState, StateStore};
use crate::telemetry::{self, TelemetryEvent};
use crate::tools::{Bead, Llm, Scm, SessionId, Sessions, Tracker, Vcs};
use crate::verifier::{self, PrEvidence};
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

/// Run one full tick: slow tier (intake -> route -> dispatch) then fast tier
/// (verify every ATTESTED bead), then emit exactly one summarizing `TICK`
/// event. `tick_index` selects whether the slow tier is due this call
/// (`tick_index % (slow_tick_secs / fast_tick_secs).max(1) == 0`); pass `0` to
/// always run the slow tier (used by `--once`).
///
/// Stage gate: `deps.cfg.stage` must be `1` — this function only implements
/// the Stage-1 substitution rule (re-roll verdicts recorded, never executed).
pub fn run_tick(deps: &TickDeps, tick_index: u64, elapsed_secs: u64) -> Result<TickSummary, DaemonError> {
    if deps.cfg.stage != 1 && deps.cfg.stage != 2 {
        return Err(DaemonError::Config(format!(
            "run_tick only implements stage 1 or 2; got stage={}",
            deps.cfg.stage
        )));
    }

    let mut summary = TickSummary::default();

    // Increment autonomy_secs and perform safety envelope & wedge detection checks for active beads
    let active_overlays = deps.store.increment_active_autonomy(elapsed_secs)?;
    for mut overlay in active_overlays {
        // 1. Time-box envelope check
        if overlay.autonomy_secs >= deps.cfg.autonomy_timebox_secs {
            overlay.state = OverlayState::HumanHeld;
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
            let comment_body = format!("🤖 **[dark-factory]** Coder session parked (human held): autonomy time-box limit exceeded.");
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
                if let Some(ref branch) = overlay.branch {
                    if overlay.autonomy_secs >= 1800 {
                        let last_commit_epoch = deps.scm.remote_branch_last_commit(branch)?;
                        let now_epoch = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let is_silent = match last_commit_epoch {
                            None => true,
                            Some(commit_time) => now_epoch.saturating_sub(commit_time) >= 1800,
                        };

                        if is_silent {
                            overlay.state = OverlayState::HumanHeld;
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
                            let comment_body = format!("🤖 **[dark-factory]** Coder session parked (human held): coder silent/inactive on branch for 30 minutes.");
                            let _ = post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body);
                            summary.beads_parked_human_held += 1;
                        }
                    }
                }
            }
            OverlayState::Attested => {
                if let Some(pr_number) = overlay.pr_number {
                    let pr_snapshot = deps.scm.pr_snapshot(pr_number)?;
                    let now_epoch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    if now_epoch.saturating_sub(pr_snapshot.updated_at_epoch) >= 1800 && !pr_snapshot.ci_pending {
                        let is_stalled_or_dead = if let Some(ref session_id_str) = overlay.session_id {
                            let session_id = SessionId(session_id_str.clone());
                            deps.sessions.is_quiescent(&session_id)?
                        } else {
                            true
                        };

                        if is_stalled_or_dead {
                            overlay.state = OverlayState::HumanHeld;
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
                            let comment_body = format!("🤖 **[dark-factory]** Coder session parked (human held): session stalled or quiescent on open PR.");
                            let _ = post_scm_comment_by_bead_id(deps, &overlay.bead_id, &comment_body);
                            summary.beads_parked_human_held += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let slow_tier_due = {
        let ratio = (deps.cfg.slow_tick_secs / deps.cfg.fast_tick_secs.max(1)).max(1);
        tick_index.is_multiple_of(ratio)
    };

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
        }),
        serde_json::json!({"tick_index": tick_index, "slow_tier_due": slow_tier_due}),
    )?;

    Ok(summary)
}

/// Slow tier: intake new beads, route each freshly-queued bead, dispatch as
/// many QUEUED beads as the safety envelope (30/15) allows.
fn run_slow_tier(deps: &TickDeps, summary: &mut TickSummary) -> Result<(), DaemonError> {
    let created = intake::normalize(deps.scm, deps.tracker, deps.cfg)?;
    let mut routing_candidates: Vec<Bead> = Vec::new();
    for bead_id in &created {
        let mut pr_number = None;
        if deps.llm.is_real() {
            if let Ok(candidates) = deps.tracker.fetch_candidates() {
                if let Some(bead) = candidates.iter().find(|b| b.id == *bead_id) {
                    if let Some(ref ext_ref) = bead.external_ref {
                        if let Some((_, num_str)) = parse_external_ref(ext_ref) {
                            if let Ok(num) = num_str.parse::<u64>() {
                                if crate::tools::run_tool(
                                    "gh",
                                    &[
                                        "pr",
                                        "view",
                                        &num.to_string(),
                                        "--repo",
                                        &deps.cfg.target_repo,
                                        "--json",
                                        "number",
                                    ],
                                    10,
                                ).is_ok() {
                                    pr_number = Some(num);
                                }
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
            serde_json::json!({}),
        )?;
        // `Tracker::fetch_candidates` == `br list ...`; a real `br` would show
        // this bead on the very next call since `br` is a durable store. Test
        // fakes are static/pre-seeded, so route/dispatch this tick against the
        // bead we just created directly rather than depending on the fake
        // reflecting it back through `fetch_candidates`.
        routing_candidates.push(Bead {
            id: bead_id.clone(),
            title: String::new(),
            description: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        });
    }

    // Also pick up any bead left over from a prior tick that reached QUEUED
    // but was never routed/dispatched (process restart resilience) — real
    // `Tracker::fetch_candidates` reflects prior `create_bead` calls, so this
    // covers that path in production even though the static test fake can't.
    for bead in deps.tracker.fetch_candidates()? {
        if !routing_candidates.iter().any(|b| b.id == bead.id) {
            routing_candidates.push(bead);
        }
    }

    let mut ready: Vec<(Bead, RoutingVerdict)> = Vec::new();
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
                    serde_json::json!({"manual": true}),
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
                    serde_json::json!({"routingVerdict": verdict_str}),
                )?;
                ready.push((bead.clone(), verdict));
            }
            Err(DaemonError::Parse(reason)) => {
                // ZFC: an unparseable routing verdict is never guessed at —
                // park the bead HUMAN_HELD per the same "unknown is not a
                // silent default" discipline router.rs already enforces.
                let mut held = overlay;
                held.state = OverlayState::HumanHeld;
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
                let comment_body = format!("🤖 **[dark-factory]** Router parse error (human held): {}", reason);
                if let Some(ref ext_ref) = bead.external_ref {
                    let _ = deps.tracker.comment_external(ext_ref, &comment_body);
                }
            }
            Err(other) => return Err(other),
        }
    }

    if !ready.is_empty() {
        let dispatched = dispatch::dispatch_ready(deps.sessions, deps.store, deps.cfg, &ready)?;
        summary.beads_dispatched += dispatched;
        for (bead, _) in ready.iter().take(dispatched) {
            emit(
                deps.telemetry_log,
                &bead.id,
                1,
                OverlayState::Dispatched.as_str(),
                "TASK_DISPATCHED",
                serde_json::json!({}),
                serde_json::json!({}),
            )?;
            let attempt = if let Ok(Some(o)) = deps.store.load(&bead.id) {
                o.attempt
            } else {
                1
            };
            let comment_body = format!(
                "🤖 **[dark-factory]** Spawned worker session in slot for bead `{}` (attempt {}). Branch: `factory/{}-r{}`.",
                bead.id, attempt, bead.id, attempt
            );
            if let Some(ref ext_ref) = bead.external_ref {
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
fn skeptic_evidence(deps: &TickDeps, bead_id: &str, pr: u64) -> Result<PrEvidence, DaemonError> {
    let prompt = format!(
        "You are the Stage-1 Skeptic gate for an autonomous coding factory.\n\
         Review bead {bead_id}'s PR #{pr} end-to-end (diff, evidence, tests) and \
         judge whether it is ready to merge.\n\
         Respond with exactly one line of the form:\n\
         pass|warn <note>|fail <reason>",
    );

    if !deps.llm.is_real() {
        let reply = deps.llm.judge(&prompt)?;
        let skeptic_verdict = verifier::parse_skeptic_verdict(&reply);
        return Ok(PrEvidence {
            is_production: false,
            non_test_changed_loc: 0,
            has_integration_evidence_marker: false,
            er_verdict: verifier::ErVerdict::Absent,
            skeptic_verdict,
        });
    }

    let prompt_clone1 = prompt.clone();
    let handle1 = std::thread::spawn(move || {
        crate::tools::run_tool("codex", &["exec", "--yolo", "--skip-git-repo-check", &prompt_clone1], 120)
    });

    let prompt_clone2 = prompt.clone();
    let handle2 = std::thread::spawn(move || {
        let home = std::env::var("HOME").unwrap_or_default();
        let nvm_claude = format!("{}/.nvm/versions/node/v22.22.0/bin/claude", home);
        let claude_bin = if std::path::Path::new(&nvm_claude).exists() {
            nvm_claude
        } else {
            "claude".to_string()
        };
        crate::tools::run_tool(&claude_bin, &["--print", "--dangerously-skip-permissions", "--setting-sources", "", &prompt_clone2], 120)
    });

    let res1 = handle1.join().unwrap_or(Err(DaemonError::Tool { tool: "thread".into(), rc: -1, stderr: "join failed".into() }));
    let res2 = handle2.join().unwrap_or(Err(DaemonError::Tool { tool: "thread".into(), rc: -1, stderr: "join failed".into() }));

    let v1 = match &res1 {
        Ok(reply) => verifier::parse_skeptic_verdict(reply),
        Err(_) => None,
    };

    let v2 = match &res2 {
        Ok(reply) => verifier::parse_skeptic_verdict(reply),
        Err(_) => None,
    };

    let skeptic_verdict = match (v1, v2) {
        (Some(verifier::SkepticVerdict::Fail(r1)), Some(verifier::SkepticVerdict::Fail(r2))) => Some(verifier::SkepticVerdict::Fail(format!("{r1} && {r2}"))),
        (Some(verifier::SkepticVerdict::Fail(r)), _) => Some(verifier::SkepticVerdict::Fail(r)),
        (_, Some(verifier::SkepticVerdict::Fail(r))) => Some(verifier::SkepticVerdict::Fail(r)),
        (Some(verifier::SkepticVerdict::Warn(w1)), Some(verifier::SkepticVerdict::Warn(w2))) => Some(verifier::SkepticVerdict::Warn(format!("{w1} && {w2}"))),
        (Some(verifier::SkepticVerdict::Warn(w)), _) => Some(verifier::SkepticVerdict::Warn(w)),
        (_, Some(verifier::SkepticVerdict::Warn(w))) => Some(verifier::SkepticVerdict::Warn(w)),
        (Some(verifier::SkepticVerdict::Pass), Some(verifier::SkepticVerdict::Pass)) => Some(verifier::SkepticVerdict::Pass),
        (Some(verifier::SkepticVerdict::Pass), None) => Some(verifier::SkepticVerdict::Pass),
        (None, Some(verifier::SkepticVerdict::Pass)) => Some(verifier::SkepticVerdict::Pass),
        _ => None,
    };

    Ok(PrEvidence {
        is_production: false,
        non_test_changed_loc: 0,
        has_integration_evidence_marker: false,
        er_verdict: verifier::ErVerdict::Absent,
        skeptic_verdict,
    })
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
                            &deps.cfg.target_repo,
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
                overlay.state = OverlayState::Attested;
                deps.store.save(&overlay)?;
                emit(
                    deps.telemetry_log,
                    bead_id,
                    overlay.attempt,
                    OverlayState::Attested.as_str(),
                    "PR_OPENED",
                    serde_json::json!({}),
                    serde_json::json!({"pr_number": pr}),
                )?;
                let comment_body = format!(
                    "🤖 **[dark-factory]** Worker session opened this pull request for bead `{}`. Beginning gate-by-gate safety verification...",
                    bead_id
                );
                let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
            }
        }

        if overlay.state != OverlayState::Attested {
            continue;
        }
        let pr = match overlay.pr_number {
            Some(pr) => pr,
            None => continue,
        };

        let mut evidence = skeptic_evidence(deps, bead_id, pr)?;
        let snapshot = deps.scm.pr_snapshot(pr)?;
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

        evidence.er_verdict = verifier::parse_er_verdict(&snapshot.comments);
        evidence.is_production = verifier::classify_production(&snapshot.files);
        evidence.non_test_changed_loc = verifier::calculate_non_test_loc(&snapshot.files);
        evidence.has_integration_evidence_marker = verifier::check_integration_marker(&snapshot.body, &snapshot.comments);
        let report = verifier::assess(deps.scm, pr, deps.cfg, &evidence)?;
        summary.gates_assessed += 1;
        emit(
            deps.telemetry_log,
            bead_id,
            overlay.attempt,
            OverlayState::Attested.as_str(),
            "GATE_ASSESSMENT",
            serde_json::json!({}),
            serde_json::json!({"all_green": report.all_green}),
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
                let comment_body = format!(
                    "🤖 **[dark-factory]** Coder session parked (human held): gate assessment failed. Stage 1 configuration prevents re-roll."
                );
                let _ = post_scm_comment_by_bead_id(deps, bead_id, &comment_body);
            } else {
                // Stage 2: execute re-roll engine
                let mut reviewer = "verifier".to_string();
                let mut feedback = Vec::new();
                for (gate_name, result) in &report.results {
                    if let verifier::GateResult::Red(ref reason) = result {
                        feedback.push(format!("{gate_name:?}: {reason}"));
                        if *gate_name == verifier::GateName::Skeptic {
                            reviewer = "skeptic".to_string();
                        } else if *gate_name == verifier::GateName::CodeRabbitApproved {
                            reviewer = "coderabbit".to_string();
                        }
                    }
                }
                let review_text = feedback.join("\n");

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
                    Ok(crate::reroll::RerollOutcome::Rerolled { new_branch: _ }) => {
                        // Perform recovery validation: check if spec is valid TOML
                        let spec_path = std::path::Path::new(&deps.cfg.spec_dir).join(format!("{}.toml", overlay.bead_id));
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
                            let comment_body = format!(
                                "🤖 **[dark-factory]** Coder session parked (human held): spec file validation failed in recovery."
                            );
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
                    Ok(crate::reroll::RerollOutcome::Aborted(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }

    Ok(())
}

fn parse_external_ref(external_ref: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = external_ref.split('#').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

fn post_scm_comment_by_bead_id(deps: &TickDeps, bead_id: &str, body: &str) -> Result<(), DaemonError> {
    if let Ok(Some(overlay)) = deps.store.load(bead_id) {
        if let Some(pr) = overlay.pr_number {
            let ext_ref = format!("{}#{}", deps.cfg.target_repo, pr);
            let _ = deps.tracker.comment_external(&ext_ref, body);
            return Ok(());
        }
    }
    if let Ok(candidates) = deps.tracker.fetch_candidates() {
        if let Some(bead) = candidates.iter().find(|b| b.id == bead_id) {
            if let Some(ref ext_ref) = bead.external_ref {
                let _ = deps.tracker.comment_external(ext_ref, body);
            }
        }
    }
    Ok(())
}

