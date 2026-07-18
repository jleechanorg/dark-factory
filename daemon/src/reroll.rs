use crate::config::Config;
use crate::errors::DaemonError;
use crate::state::{
    set_human_hold_reason, BeadOverlay, HumanHoldReason, OverlayState, StateStore,
};
use crate::tools::{Llm, Scm, Sessions, SpawnSpec, Vcs};
use crate::telemetry::{self, TelemetryEvent};
use crate::constraints;
use std::path::Path;
use std::hash::{Hash, Hasher};

pub struct RerollDeps<'a> {
    pub scm: &'a dyn Scm,
    pub sessions: &'a dyn Sessions,
    pub vcs: &'a dyn Vcs,
    pub store: &'a dyn StateStore,
    pub llm: &'a dyn Llm,
    pub cfg: &'a Config,
    pub telemetry_log: &'a Path,
    pub reviewer: String,
    pub review_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RerollOutcome {
    Rerolled { new_branch: String },
    Aborted(String),
    Held(String),
}

/// Park reason recorded in `BeadOverlay::park_reason` when the circuit
/// breaker (bead jleechan-cq8r) trips. Deliberately prefixed with
/// `"circuit-breaker"` — `StateStore::recover_human_held` (bead
/// jleechan-4jn1) matches on that prefix to exclude circuit-breaker parks
/// from automatic requeue, since they exist specifically to STOP retrying
/// after the same reviewer rejects the same underlying issue twice in a
/// row. Shared as a constant so the park_reason write and the
/// `RerollOutcome::Held` message can never drift apart.
pub const CIRCUIT_BREAKER_PARK_REASON: &str = crate::state::CIRCUIT_BREAKER_PARK_REASON;

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

fn emit_telemetry(
    log_path: &Path,
    bead_id: &str,
    attempt_id: u32,
    lifecycle_state: &str,
    event_type: &str,
    metrics: serde_json::Value,
    context: serde_json::Value,
) -> Result<(), DaemonError> {
    telemetry::emit(
        log_path,
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

/// Circuit-Breaker Semantic Comparator (spec §4.2.6): judges whether two
/// consecutive same-reviewer rejection texts describe the SAME underlying
/// issue / root cause, even when reworded, paraphrased, reformatted, or
/// extended — as opposed to two genuinely different issues. This is a real
/// model judgment call (ZFC: semantic-similarity judgments must not be
/// hand-rolled as a scoring function) — mirrors the trailing-JSON-object
/// parsing contract `constraints::extract` already uses against the same
/// `Llm` trait.
fn same_underlying_issue(llm: &dyn Llm, prior_text: &str, new_text: &str) -> Result<bool, DaemonError> {
    let prompt = format!(
        "You are the Circuit-Breaker Semantic Comparator for an autonomous coding factory (spec §4.2.6).\n\
          Two consecutive rejection review comments were left by the SAME reviewer on re-roll attempts of \
          the same bead. Judge whether they describe the SAME underlying issue / root cause, even if \
          reworded, paraphrased, reformatted, or extended with extra commentary — as opposed to two \
          genuinely DIFFERENT issues.\n\n\
          PRIOR REJECTION:\n\"\"\"\n{prior_text}\n\"\"\"\n\n\
          NEW REJECTION:\n\"\"\"\n{new_text}\n\"\"\"\n\n\
          Respond with exactly one JSON object as the last thing in your reply, in this format:\n\
          {{\"sameUnderlyingIssue\": true|false}}"
    );

    let reply = llm.judge(&prompt)?;

    // jleechan-cq8r: a malformed/unparseable reply here must NOT construct
    // `DaemonError::Parse` -- that variant is fatal (`is_transient()` only
    // covers Tool|Timeout|Deferred) and crashes the whole daemon process
    // (main.rs calls `std::process::exit(1)` on any non-transient tick
    // error), reproducing the jleechan-5ia2 crash-loop pattern (PR #197)
    // through this call site. `ComparatorUnparseable` is transient by
    // design -- see its doc comment in errors.rs.
    let last_close = reply.rfind('}').ok_or_else(|| {
        DaemonError::ComparatorUnparseable(format!("no JSON object found in circuit-breaker comparator reply: {reply:?}"))
    })?;
    let prefix = &reply[..=last_close];
    let last_open = prefix.rfind('{').ok_or_else(|| {
        DaemonError::ComparatorUnparseable(format!("no JSON object found in circuit-breaker comparator reply: {reply:?}"))
    })?;
    let candidate = &prefix[last_open..=last_close];

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CmpResponse {
        same_underlying_issue: bool,
    }

    let parsed: CmpResponse = serde_json::from_str(candidate).map_err(|e| {
        DaemonError::ComparatorUnparseable(format!(
            "circuit-breaker comparator reply did not contain a valid response object: {e} (reply: {reply:?})"
        ))
    })?;

    Ok(parsed.same_underlying_issue)
}

pub fn execute(deps: &RerollDeps, bead: &mut BeadOverlay) -> Result<RerollOutcome, DaemonError> {
    // 1. Lock & Freshness Guard
    let latest = deps.store.load(&bead.bead_id)?;
    if let Some(ref lat) = latest {
        if lat.state != OverlayState::Attested && lat.state != OverlayState::ReRoll {
            return Ok(RerollOutcome::Aborted("bead state changed".into()));
        }
    } else {
        return Ok(RerollOutcome::Aborted("bead not found in store".into()));
    }

    bead.state = OverlayState::ReRoll;
    deps.store.save(bead)?;

    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "REROLL_START",
        serde_json::json!({}),
        serde_json::json!({}),
    )?;

    // 2. Circuit-Breaker Check
    let feedback_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        deps.review_text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };

    if bead.attempt > 1 {
        if let Some((prev_reviewer, _prev_hash)) = deps.store.load_rejection(&bead.bead_id, bead.attempt - 1)? {
            if prev_reviewer == deps.reviewer {
                let prev_text = deps.store.load_rejection_text(&bead.bead_id, bead.attempt - 1)?;
                let same_issue = match prev_text {
                    Some(ref prev) if *prev == deps.review_text => true,
                    Some(ref prev) => same_underlying_issue(deps.llm, prev, &deps.review_text)?,
                    None => false,
                };
                if same_issue {
                    bead.state = OverlayState::HumanHeld;
                    set_human_hold_reason(bead, HumanHoldReason::CircuitBreaker);
                    deps.store.save(bead)?;

                    // jleechan-9xrs Stage D: was `deps.cfg.target_repo` —
                    // the Healer scope must identify the bead's OWN
                    // resolved repo, not the daemon-global one, so a
                    // circuit-breaker trip on a non-default `[repos.*]` bead
                    // scopes correctly. This path is Stage-2-only (Stage 1
                    // never reaches `execute`'s "else" branch — see
                    // `run_fast_tier`'s `Stage 1: recorded, not executed`
                    // comment in tick.rs) but is fixed for consistency ahead
                    // of Stage 2 activation.
                    let bead_repo = bead.repo(deps.cfg).to_string();
                    let (owner, repo) = bead_repo
                        .split_once('/')
                        .unwrap_or(("unknown_owner", "unknown_repo"));
                    let healer_scope = format!("{}:{}:{}", owner, repo, bead.bead_id);

                    let healer_report = format!(
                        "# Healer Report\n\n                     Circuit-breaker triggered for bead {} (scope: {}).\n                     Consecutive re-roll rejections by the same reviewer ({}) citing the same semantic reason:\n\n                     \"\"\"\n                     {}\n                     \"\"\"\n",
                        bead.bead_id, healer_scope, deps.reviewer, deps.review_text
                    );

                    emit_telemetry(
                        deps.telemetry_log,
                        &bead.bead_id,
                        bead.attempt,
                        bead.state.as_str(),
                        "CIRCUIT_BREAKER_TRIGGERED",
                        serde_json::json!({}),
                        serde_json::json!({
                            "healerScope": healer_scope,
                            "healerReport": healer_report,
                            "reviewer": deps.reviewer,
                            "feedbackHash": feedback_hash,
                        }),
                    )?;

                    return Ok(RerollOutcome::Held(CIRCUIT_BREAKER_PARK_REASON.into()));
                }
            }
        }
    }

    // Save current rejection for future circuit-breaker checks
    deps.store.save_rejection(&bead.bead_id, bead.attempt, &deps.reviewer, &feedback_hash, &deps.review_text)?;

    // Adopted-PR remediation (bead jleechan-tfs1, Option A + hard safety
    // amendment): `bead.branch` for an adopted bead is the external
    // contributor's OWN branch (set at intake time in
    // `tick::run_slow_tier`), not a factory-fabricated one. Steps 3-8 below
    // are the factory-fabricated path — they stop/attach an AO session that
    // was never spawned for an adopted bead, fabricate a brand-new branch,
    // and close the contributor's PR. None of that is safe or applicable
    // here: diverge into the append-only remediation path instead and
    // return without touching steps 3-8.
    if bead.is_adopted {
        return execute_adopted(deps, bead);
    }

    // 3. Stop AO session and wait for quiescence (best-effort, short).
    //
    // Bead jleechan-zeij / issue #322: the previous implementation held a
    // hard 60-second quiescence-confirmation poll loop with a HEAD-SHA
    // stability requirement, originally added to guard against a worker
    // pushing a final commit during the confirmation window. In practice
    // that loop is reached only on the `ATTESTED -> RE_ROLL` path
    // (reroll.rs:147 enforces the gate at the top of `execute`), and on
    // that path the worker has ALREADY been promoted through the fast-tier
    // DISPATCHED -> ATTESTED transition (tick.rs:2092) — which requires
    // a PR to be open AND the verifier to have run a full
    // `GATE_ASSESSMENT` against the PR's current HEAD (which the reroll
    // caller just consumed). By the time reroll entry happens, the
    // worker's branch has been observably settled for many minutes.
    //
    // The bug observed live (2026-07-18, U4 lanes 9lvs / mh9o):
    // the AO session reports `status=spawning, activity=idle` (worker
    // finished and went back to idle without an explicit kill), which the
    // adapter's `is_terminal_ao_session` predicate (adapters.rs:3408)
    // classifies as NOT terminal. The 60s hardcoded loop then expires
    // and parks the bead HUMAN_HELD on every red gate_assessment,
    // defeating unattended end-to-end dispatch.
    //
    // The judo move: replace the 60s hard timeout with a SHORT
    // best-effort wait (a couple of polls with a small ceiling — ~3s) that
    // only blocks reroll when a worker is ACTIVELY in the middle of
    // pushing. If `is_quiescent` is already false (the typical
    // idle+spawning case) or never becomes true within the short window,
    // we fall through to branch fabrication: `stop()` has already been
    // issued (freeing the global worker slot), and the verifier has
    // already positively confirmed the worker's final SHA at gate
    // assessment time, so there is no race to guard against at reroll
    // entry. The genuine mid-push race is still covered by the
    // DISPATCHED -> ATTESTED transition (tick.rs:2092), not by the reroll
    // path.
    //
    // Track the per-branch last-observed HEAD SHA across this single
    // reroll call's wait window so we can detect a mid-window push even
    // within the short ceiling. Cleared on every reroll entry so stale
    // values never leak between beads.
    if let Some(ref branch) = bead.branch {
        emit_telemetry(
            deps.telemetry_log,
            &bead.bead_id,
            bead.attempt,
            bead.state.as_str(),
            "REROLL_QUIESCENCE_WAIT",
            serde_json::json!({}),
            serde_json::json!({"branch": branch}),
        )?;

        // attach() returns Err(SessionNotFound) when the previous worker
        // has already been fully reaped (no AO entry in `ao status`). In
        // that case there is nothing to stop and nothing to wait for —
        // clear the durable handle and fall through to branch fabrication.
        let session_id_result = deps.sessions.attach(branch, &bead.bead_id);
        let session_id = match session_id_result {
            Ok(id) => Some(id),
            Err(DaemonError::SessionNotFound { .. }) => {
                bead.session_id = None;
                deps.store.save(bead)?;
                emit_telemetry(
                    deps.telemetry_log,
                    &bead.bead_id,
                    bead.attempt,
                    bead.state.as_str(),
                    "REROLL_QUIESCENCE_SUCCESS",
                    serde_json::json!({}),
                    serde_json::json!({"reason": "no_live_session"}),
                )?;
                None
            }
            Err(e) => {
                bead.state = OverlayState::HumanHeld;
                set_human_hold_reason(bead, HumanHoldReason::RerollSessionAttachFailed);
                deps.store.save(bead)?;
                return Ok(RerollOutcome::Held(format!("failed to attach to session: {e}")));
            }
        };

        // Skip the entire stop-and-wait block when no live session exists.
        // This handles both "branch has no live session" (None) and
        // "bead has no branch at all" (the outer `if let Some` already
        // returned early for that case via the fall-through to step 4
        // below).
        if let Some(session_id) = session_id.as_ref() {
        // Best-effort stop — failure to stop is logged but does NOT park
        // the bead. The worker slot is the only thing we need to free; if
        // `stop` itself errors (e.g. transient `ao` failure), the next
        // tick's `active_count` sweep will still see an idle session
        // counted as active (a separate, pre-existing accounting
        // problem — NOT this fix's concern), and the reroll can safely
        // proceed to fabricate a fresh branch on a different attempt slot.
        if let Err(e) = deps.sessions.stop(session_id) {
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_QUIESCENCE_STOP_FAILED_BEST_EFFORT",
                serde_json::json!({}),
                serde_json::json!({"sessionId": session_id.0, "error": format!("{e}")}),
            )?;
        }

        // Short best-effort wait: a couple of polls with a small ceiling
        // (~3s). The previous 60s timeout with HEAD-SHA stability is
        // removed; see the rationale above for why it was unreachable for
        // the genuine ATTESTED -> RE_ROLL case in practice.
        //
        // We DO still observe HEAD SHA on each terminal poll: if the
        // session is reporting terminal but the branch HEAD is still
        // moving (the genuine mid-push race the original §4.2.6 spec was
        // written for), we keep polling until either two consecutive
        // terminal+matching reads (stable) OR the short ceiling expires.
        // We do NOT extend the ceiling to 60s — that was the bug.
        let start = std::time::Instant::now();
        let poll_interval = std::time::Duration::from_millis(500);
        let max_wait = std::time::Duration::from_secs(3);
        let mut confirmed = false;
        let mut observed_terminal = false;
        let mut last_terminal_head: Option<String> = None;
        while start.elapsed() < max_wait {
            let is_terminal = match deps.sessions.is_quiescent(session_id) {
                Ok(v) => v,
                Err(e) => {
                    // Transient AO status query failure — log but don't
                    // park. A transient `ao` failure should not block a
                    // fresh attempt branch when the underlying state is
                    // already known-good (ATTESTED -> RE_ROLL).
                    emit_telemetry(
                        deps.telemetry_log,
                        &bead.bead_id,
                        bead.attempt,
                        bead.state.as_str(),
                        "REROLL_QUIESCENCE_CHECK_TRANSIENT",
                        serde_json::json!({}),
                        serde_json::json!({"error": format!("{e}")}),
                    )?;
                    break;
                }
            };

            if is_terminal {
                observed_terminal = true;
                let head = match deps.vcs.head_sha(branch) {
                    Ok(h) => h,
                    Err(e) => {
                        // Same rationale as the transient check above: a
                        // transient `git` failure should not park the
                        // bead when the underlying state is already
                        // known-good.
                        emit_telemetry(
                            deps.telemetry_log,
                            &bead.bead_id,
                            bead.attempt,
                            bead.state.as_str(),
                            "REROLL_QUIESCENCE_HEAD_TRANSIENT",
                            serde_json::json!({}),
                            serde_json::json!({"error": format!("{e}")}),
                        )?;
                        break;
                    }
                };
                match &last_terminal_head {
                    Some(prev) if *prev == head => {
                        // Two consecutive terminal+matching reads — the
                        // worker's final commit has settled.
                        confirmed = true;
                        break;
                    }
                    _ => {
                        // First terminal observation, or the HEAD SHA
                        // moved since the last terminal observation (a
                        // push landed mid-window) — reset the streak and
                        // try again on the next poll.
                        last_terminal_head = Some(head);
                    }
                }
            } else {
                // Not yet terminal. Idle+spawning (the live bug) lives
                // here: we keep polling until `max_wait` expires, then
                // fall through. Any HEAD SHA stability streak observed
                // while it looked terminal is no longer trustworthy.
                last_terminal_head = None;
            }

            std::thread::sleep(poll_interval);
        }

        // No `HUMAN_HELD` park on timeout, on `stop` failure, or on
        // transient quiescence/head_sha errors. The bead is known-good at
        // entry (ATTESTED -> RE_ROLL); a transient wait failure should
        // never block the fresh attempt branch. The `confirmed` flag is
        // retained in telemetry so post-hoc debugging can distinguish
        // "waited and confirmed" from "waited and gave up".
        let reason = if confirmed {
            "confirmed_terminal_stable_head"
        } else if observed_terminal {
            "terminal_seen_head_unstable"
        } else {
            "best_effort_window_elapsed"
        };

        // Clear the durable session handle regardless of confirmation —
        // the previous worker is by definition replaced by the fresh
        // attempt we are about to fabricate.
        bead.session_id = None;
        deps.store.save(bead)?;

        emit_telemetry(
            deps.telemetry_log,
            &bead.bead_id,
            bead.attempt,
            bead.state.as_str(),
            "REROLL_QUIESCENCE_SUCCESS",
            serde_json::json!({}),
            serde_json::json!({"reason": reason}),
        )?;
        }
    }

    // 4. Compute baseline
    let base_sha = deps.vcs.base_head(&deps.cfg.base_branch)?;
    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "REROLL_BASELINE_COMPUTED",
        serde_json::json!({}),
        serde_json::json!({"baseCommit": base_sha}),
    )?;

    // 5. Fresh attempt branch
    let superseded_attempt = bead.attempt;
    bead.attempt += 1;
    bead.reroll_count += 1;
    let new_branch = format!("factory/{}-r{}", bead.bead_id, bead.attempt);
    deps.vcs.create_branch_at(&new_branch, &base_sha)?;

    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "REROLL_BRANCH_CREATED",
        serde_json::json!({}),
        serde_json::json!({"newBranch": new_branch, "baseCommit": base_sha}),
    )?;

    // 6. Record branch registry
    deps.store.register_branch(&bead.bead_id, &new_branch)?;
    bead.branch = Some(new_branch.clone());

    // 7. Old PR closure
    if let Some(pr_number) = bead.pr_number {
        let comment = format!("Superseded by new attempt branch {}", new_branch);
        deps.scm.close_pr(pr_number, &comment)?;
        bead.pr_number = None;

        emit_telemetry(
            deps.telemetry_log,
            &bead.bead_id,
            bead.attempt,
            bead.state.as_str(),
            "REROLL_PR_CLOSED",
            serde_json::json!({}),
            serde_json::json!({"prNumber": pr_number, "comment": comment}),
        )?;
    }

    // 8. Constraint Extraction & Spec Mutation
    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "CONSTRAINT_EXTRACTION_START",
        serde_json::json!({}),
        serde_json::json!({}),
    )?;

    let extracted = constraints::extract(deps.llm, &deps.review_text)?;

    // Format spec block append-only
    let mut inhibition_lines = String::new();
    for spec in &extracted.inhibition_specs {
        inhibition_lines.push_str(&format!("    \"{}\",\n", spec.replace('"', "\\\"")));
    }
    let mut positive_lines = String::new();
    for spec in &extracted.positive_assertions {
        positive_lines.push_str(&format!("    \"{}\",\n", spec.replace('"', "\\\"")));
    }

    let block = format!(
        "\n[[reroll]]\n         reviewer = \"{}\"\n         attempt = {}\n         inhibition_specs = [\n         {}         ]\n         positive_assertions = [\n         {}         ]\n         raw_feedback = \"\"\"\n         {}\n         \"\"\"\n",
        deps.reviewer,
        superseded_attempt,
        inhibition_lines,
        positive_lines,
        deps.review_text
    );

    let spec_path = Path::new(&deps.cfg.spec_dir).join(format!("{}.toml", bead.bead_id));
    constraints::append_mutation(&spec_path, &block)?;

    // Transition to RECOVERY
    bead.state = OverlayState::Recovery;
    deps.store.save(bead)?;

    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "CONSTRAINT_MUTATION_SUCCESS",
        serde_json::json!({
            "consumedUSD": 0.0,
            "elapsedAutonomySeconds": bead.autonomy_secs,
            "extractedConstraints": {
                "positiveAssertionsCount": extracted.positive_assertions.len(),
                "inhibitionSpecsCount": extracted.inhibition_specs.len(),
            }
        }),
        serde_json::json!({
            "targetBranch": new_branch,
            "baseCommit": base_sha,
            "activeModel": "fallback-chain",
        }),
    )?;

    Ok(RerollOutcome::Rerolled { new_branch })
}

/// Adopted-PR remediation dispatches a real coder session onto the EXISTING
/// contributor branch, briefed with the reviewer feedback that caused the red
/// gate. This function itself never fabricates commits, never creates a
/// replacement branch, and never closes the original PR — those three
/// invariants ARE structural: this code path contains no
/// `create_branch_at`, `close_pr`, or commit-authoring call at all, only
/// `Sessions::attach`/`Sessions::spawn`.
///
/// The "no force-push / no history-rewrite" constraint is DIFFERENT and is
/// NOT structural in that same sense. It is enforced at the PROMPT level
/// only (the coder session is instructed not to force-push, in the spawn
/// prompt built below) PLUS a post-hoc detection backstop added as a
/// required amendment (bead jleechan-tfs1): this function captures the
/// branch's pre-session HEAD SHA immediately before dispatch, and every
/// tick the resulting bead sits `DISPATCHED`, `tick::run_tick`'s
/// wedge-detection sweep verifies that SHA is still an ancestor of the
/// branch's current tip (`Vcs::is_ancestor`), parking the bead `HUMAN_HELD`
/// with an escalation comment naming both SHAs if not. This is detection,
/// not prevention — the coder session is an independent subprocess running
/// its own `git push` that the daemon does not control at the git layer,
/// so the daemon cannot structurally block a force-push at the moment it
/// happens the way it structurally blocks itself from calling
/// `create_branch_at`/`close_pr` here. Do not describe the force-push
/// constraint as "structural" — it is not.
fn execute_adopted(
    deps: &RerollDeps,
    bead: &mut BeadOverlay,
) -> Result<RerollOutcome, DaemonError> {
    let branch = match bead.branch.clone() {
        Some(b) => b,
        None => {
            // Should not happen: adoption always sets `branch` to the
            // contributor's head_ref_name. Park rather than guess.
            bead.state = OverlayState::HumanHeld;
            set_human_hold_reason(bead, HumanHoldReason::AdoptedMissingBranch);
            deps.store.save(bead)?;
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_MISSING_BRANCH",
                serde_json::json!({}),
                serde_json::json!({}),
            )?;
            return Ok(RerollOutcome::Held(
                "adopted bead has no branch on record; refusing to fabricate one".into(),
            ));
        }
    };

    emit_telemetry(
        deps.telemetry_log,
        &bead.bead_id,
        bead.attempt,
        bead.state.as_str(),
        "REROLL_ADOPTED_REMEDIATION_START",
        serde_json::json!({}),
        serde_json::json!({"branch": branch}),
    )?;

    // Duplicate-spawn guard: `execute_adopted` is normally only reachable
    // when the bead's freshest stored state is ATTESTED or RE_ROLL. This is
    // a second line of defense against two coder sessions racing commits
    // onto the same contributor-owned branch.
    match deps.sessions.attach(&branch, &bead.bead_id) {
        Ok(existing_session) => match deps.sessions.is_quiescent(&existing_session) {
            Ok(false) => {
                bead.state = OverlayState::HumanHeld;
                bead.session_id = Some(existing_session.0.clone());
                set_human_hold_reason(bead, HumanHoldReason::AdoptedSessionAlreadyActive);
                deps.store.save(bead)?;
                emit_telemetry(
                    deps.telemetry_log,
                    &bead.bead_id,
                    bead.attempt,
                    bead.state.as_str(),
                    "REROLL_ADOPTED_SESSION_ALREADY_ACTIVE",
                    serde_json::json!({}),
                    serde_json::json!({"branch": branch, "sessionId": existing_session.0}),
                )?;
                return Ok(RerollOutcome::Held(format!(
                    "an AO session is already active on adopted branch {branch}; retained session {} for reconciliation",
                    existing_session.0
                )));
            }
            Ok(true) => {}
            Err(e) => {
                bead.state = OverlayState::HumanHeld;
                bead.session_id = Some(existing_session.0.clone());
                set_human_hold_reason(bead, HumanHoldReason::AdoptedQuiescenceCheckFailed);
                deps.store.save(bead)?;
                emit_telemetry(
                    deps.telemetry_log,
                    &bead.bead_id,
                    bead.attempt,
                    bead.state.as_str(),
                    "REROLL_ADOPTED_QUIESCENCE_CHECK_FAILED",
                    serde_json::json!({}),
                    serde_json::json!({"branch": branch, "error": e.to_string()}),
                )?;
                return Ok(RerollOutcome::Held(format!(
                    "failed to check whether an existing remediation session on adopted branch {branch} is still active: {e}"
                )));
            }
        },
        Err(DaemonError::SessionNotFound { .. }) => {}
        Err(e) => {
            bead.state = OverlayState::HumanHeld;
            set_human_hold_reason(bead, HumanHoldReason::AdoptedSessionAttachFailed);
            deps.store.save(bead)?;
            return Ok(RerollOutcome::Held(format!(
                "could not uniquely reconcile AO sessions on adopted branch {branch}: {e}"
            )));
        }
    }

    let next_attempt = bead.attempt + 1;
    let prompt = format!(
        "Address the following code review feedback from {reviewer} on this pull \
         request (attempt {attempt}). Work ONLY on the existing branch `{branch}` - \
         make real code changes that resolve the issues described below, then commit \
         and push your changes to that same branch.\n\n\
         HARD CONSTRAINTS (do not violate under any circumstance):\n\
         - This is a branch owned by an external contributor. You MUST NOT force-push, \
         rewrite commits, or otherwise rewrite this branch's history.\n\
         - You MUST NOT close the existing pull request or open a new one.\n\
         - You MUST NOT create or push to any other branch.\n\
         - If resolving the feedback genuinely requires rewriting branch history (e.g. a \
         base conflict), STOP and leave the branch exactly as-is rather than doing \
         either - a human will handle it.\n\n\
         Review feedback:\n{review_text}",
        reviewer = deps.reviewer,
        attempt = next_attempt,
        branch = branch,
        review_text = deps.review_text,
    );
    // Capture the pre-session HEAD SHA for post-hoc force-push detection
    // (bead jleechan-tfs1 amendment).
    let pre_session_sha = match deps.vcs.remote_head_sha(&branch) {
        Ok(sha) => sha,
        Err(e) => {
            bead.state = OverlayState::HumanHeld;
            // Any existing attached remediation session was positively
            // quiescent above; persist that no-live-session proof with the
            // recoverable pre-spawn hold.
            bead.session_id = None;
            set_human_hold_reason(
                bead,
                HumanHoldReason::AdoptedPreSessionShaCaptureFailed,
            );
            deps.store.save(bead)?;
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_PRE_SESSION_SHA_CAPTURE_FAILED",
                serde_json::json!({}),
                serde_json::json!({"branch": branch, "error": e.to_string()}),
            )?;
            return Ok(RerollOutcome::Held(format!(
                "failed to capture the pre-session HEAD SHA for adopted branch {branch} \
                 before dispatching a remediation session (required for post-hoc \
                 force-push detection): {e}"
            )));
        }
    };

    // jleechan-35y4 Stage A/B: adopted-PR remediation is currently
    // restricted to same-repo PRs (`intake::same_repo_pr` rejects
    // fork/cross-repo PRs before adoption), so `bead.repo(cfg)` always
    // resolves to `deps.cfg.target_repo` today and `resolve_repo` is
    // therefore always `Some`. Falling back to the bead's raw repo string
    // with `deps.cfg.ao_project` unset (rather than panicking/unwrapping)
    // keeps this path inert if that restriction is ever lifted before the
    // Stage C/D call-site sweep reaches this function.
    let adopted_repo = bead.repo(deps.cfg).to_string();
    let adopted_routing = deps.cfg.resolve_repo(&adopted_repo).unwrap_or_else(|| {
        crate::config::RepoRouting {
            ao_project: deps
                .cfg
                .ao_project
                .clone()
                .unwrap_or_else(|| adopted_repo.clone()),
            push_remote: "origin".to_string(),
        }
    });
    let spec = SpawnSpec {
        bead_id: bead.bead_id.clone(),
        branch: branch.clone(),
        prompt,
        repo: adopted_repo,
        ao_project: adopted_routing.ao_project,
        remote: adopted_routing.push_remote,
    };

    // Persist an ambiguous pre-spawn intent before crossing the external AO
    // boundary. A process death after AO creates the worker but before its
    // session id is saved leaves DISPATCHING on disk; startup reconciliation
    // converts that to a permanent human hold instead of spawning a duplicate.
    bead.state = OverlayState::Dispatching;
    bead.session_id = None;
    bead.pre_session_head_sha = Some(pre_session_sha.clone());
    deps.store.save(bead)?;

    match deps.sessions.spawn(&spec) {
        Ok(session_id) => {
            bead.attempt = next_attempt;
            bead.reroll_count += 1;
            bead.session_id = Some(session_id.0.clone());
            // Reuse DISPATCHED: branch and pr_number are deliberately left
            // unchanged (same branch, same still-open PR). The fast-tier
            // quiescence-gated DISPATCHED -> ATTESTED promotion moves this
            // back to verification after the coder session finishes.
            bead.state = OverlayState::Dispatched;
            bead.pre_session_head_sha = Some(pre_session_sha);
            if let Err(save_error) = deps.store.save(bead) {
                if let Err(cleanup_error) = deps.sessions.stop(&session_id) {
                    bead.state = OverlayState::HumanHeld;
                    bead.session_id = Some(session_id.0.clone());
                    set_human_hold_reason(bead, HumanHoldReason::SpawnCleanupFailed);
                    let cleanup_error = match deps.store.save(bead) {
                        Ok(()) => cleanup_error,
                        Err(state_error) => DaemonError::Config(format!(
                            "session cleanup failed: {cleanup_error}; additionally failed to persist the HUMAN_HELD cleanup record: {state_error}"
                        )),
                    };
                    return Err(DaemonError::SpawnCleanupFailed {
                        session: session_id.0,
                        spawn_error: Box::new(save_error),
                        cleanup_error: Box::new(cleanup_error),
                    });
                }

                bead.state = OverlayState::HumanHeld;
                bead.session_id = None;
                set_human_hold_reason(bead, HumanHoldReason::AdoptedSpawnFailed);
                deps.store.save(bead)?;
                return Err(save_error);
            }
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_SESSION_SPAWNED",
                serde_json::json!({
                    "elapsedAutonomySeconds": bead.autonomy_secs,
                }),
                serde_json::json!({"branch": branch, "sessionId": session_id.0}),
            )?;
            Ok(RerollOutcome::Rerolled { new_branch: branch })
        }
        Err(error @ DaemonError::SpawnCleanupFailed { .. }) => {
            let session = match &error {
                DaemonError::SpawnCleanupFailed { session, .. } => session.clone(),
                _ => unreachable!(),
            };
            bead.state = OverlayState::HumanHeld;
            bead.session_id = Some(session.clone());
            set_human_hold_reason(bead, HumanHoldReason::SpawnCleanupFailed);
            if let Err(state_error) = deps.store.save(bead) {
                return Err(DaemonError::SpawnCleanupFailed {
                    session,
                    spawn_error: Box::new(error),
                    cleanup_error: Box::new(DaemonError::Config(format!(
                        "failed to persist HUMAN_HELD cleanup record: {state_error}"
                    ))),
                });
            }
            Err(error)
        }
        Err(e) => {
            bead.state = OverlayState::HumanHeld;
            bead.session_id = None;
            set_human_hold_reason(bead, HumanHoldReason::AdoptedSpawnFailed);
            deps.store.save(bead)?;
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_SPAWN_FAILED",
                serde_json::json!({}),
                serde_json::json!({"branch": branch, "error": e.to_string()}),
            )?;
            Ok(RerollOutcome::Held(format!(
                "failed to spawn a remediation coder session on adopted branch {branch}: {e}"
            )))
        }
    }
}
