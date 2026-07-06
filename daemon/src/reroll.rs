use crate::config::Config;
use crate::errors::DaemonError;
use crate::state::{BeadOverlay, OverlayState, StateStore};
use crate::tools::{Llm, Scm, Sessions, Vcs};
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
        if let Some((prev_reviewer, prev_hash)) = deps.store.load_rejection(&bead.bead_id, bead.attempt - 1)? {
            if prev_reviewer == deps.reviewer && prev_hash == feedback_hash {
                bead.state = OverlayState::HumanHeld;
                deps.store.save(bead)?;

                let (owner, repo) = deps.cfg.target_repo.split_once('/').unwrap_or(("unknown_owner", "unknown_repo"));
                let healer_scope = format!("{}:{}:{}", owner, repo, &bead.bead_id);

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

                return Ok(RerollOutcome::Held("circuit-breaker triggered: same reviewer and feedback hash as prior attempt".into()));
            }
        }
    }

    // Save current rejection for future circuit-breaker checks
    deps.store.save_rejection(&bead.bead_id, bead.attempt, &deps.reviewer, &feedback_hash, &deps.review_text)?;

    // 3. Stop AO session and wait for quiescence (60s timeout)
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

        let session_id = match deps.sessions.attach(branch, &bead.bead_id) {
            Ok(id) => id,
            Err(e) => {
                bead.state = OverlayState::HumanHeld;
                deps.store.save(bead)?;
                return Ok(RerollOutcome::Held(format!("failed to attach to session: {e}")));
            }
        };

        if let Err(e) = deps.sessions.stop(&session_id) {
            bead.state = OverlayState::HumanHeld;
            deps.store.save(bead)?;
            return Ok(RerollOutcome::Held(format!("failed to stop session: {e}")));
        }

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(60);
        let mut quiescent = false;
        while start.elapsed() < timeout {
            match deps.sessions.is_quiescent(&session_id) {
                Ok(true) => {
                    quiescent = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    bead.state = OverlayState::HumanHeld;
                    deps.store.save(bead)?;
                    return Ok(RerollOutcome::Held(format!("quiescence check failed: {e}")));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        if !quiescent {
            bead.state = OverlayState::HumanHeld;
            deps.store.save(bead)?;
            return Ok(RerollOutcome::Held("quiescence timeout exceeded (60s)".into()));
        }

        emit_telemetry(
            deps.telemetry_log,
            &bead.bead_id,
            bead.attempt,
            bead.state.as_str(),
            "REROLL_QUIESCENCE_SUCCESS",
            serde_json::json!({}),
            serde_json::json!({}),
        )?;
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
        inhibition_lines.push_str(&format!("    \"{}\",\n", spec.replace('"', "\"")));
    }
    let mut positive_lines = String::new();
    for spec in &extracted.positive_assertions {
        positive_lines.push_str(&format!("    \"{}\",\n", spec.replace('"', "\"")));
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
