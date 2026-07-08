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

        // Spec §4.2.6: quiescence requires BOTH a terminal process state AND a
        // stable branch HEAD SHA before the daemon proceeds — this is the
        // guard against the race where an AO worker pushes a final commit
        // during the confirmation window, leaving a half-pushed branch. A
        // process-only check (the old implementation here) cannot detect
        // that race at all: `is_quiescent()` can report a terminal AO
        // process while a `git push` from that same worker is still landing
        // on the remote, or lands moments later. HEAD SHA stability is
        // therefore tracked independently: on each poll where the process IS
        // terminal, read `head_sha(branch)` and require it to match the
        // previous terminal-poll's reading (two consecutive terminal+matching
        // reads) before declaring quiescence confirmed. Any non-terminal poll
        // OR any HEAD SHA change resets the stability streak — a mid-window
        // push is treated as "still not settled", never as a false success.
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(60);
        let poll_interval = std::time::Duration::from_millis(500);
        let mut confirmed = false;
        let mut last_stable_head: Option<String> = None;
        while start.elapsed() < timeout {
            let is_terminal = match deps.sessions.is_quiescent(&session_id) {
                Ok(v) => v,
                Err(e) => {
                    bead.state = OverlayState::HumanHeld;
                    deps.store.save(bead)?;
                    return Ok(RerollOutcome::Held(format!("quiescence check failed: {e}")));
                }
            };

            if is_terminal {
                let head = match deps.vcs.head_sha(branch) {
                    Ok(h) => h,
                    Err(e) => {
                        bead.state = OverlayState::HumanHeld;
                        deps.store.save(bead)?;
                        return Ok(RerollOutcome::Held(format!("quiescence check failed: {e}")));
                    }
                };
                match &last_stable_head {
                    Some(prev) if *prev == head => {
                        confirmed = true;
                        break;
                    }
                    _ => {
                        // First terminal+readable observation, or the HEAD
                        // SHA moved since the last terminal observation (a
                        // push landed mid-window) — (re)start the stability
                        // streak rather than trusting a single sample.
                        last_stable_head = Some(head);
                    }
                }
            } else {
                // Process left/never reached terminal state — any HEAD SHA
                // stability streak observed while it looked terminal is no
                // longer trustworthy (e.g. it resumed and could push again).
                last_stable_head = None;
            }

            std::thread::sleep(poll_interval);
        }

        if !confirmed {
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

/// Adopted-PR remediation (bead jleechan-tfs1, Option A + hard safety
/// amendment): append-only fix commit on the EXISTING contributor branch.
/// Never fabricates a replacement branch, never closes the original PR,
/// never force-pushes or rewrites history. `deps.vcs.push_fix_commit`
/// enforces the append-only/non-force contract at the git layer (fetch +
/// checkout onto `origin/<branch>` + `--allow-empty` commit + a *non*-force
/// push); a non-fast-forward rejection there (remote diverged, or a genuine
/// conflict with base that would require a rebase) is surfaced here as a
/// `Held` outcome — this function does NOT retry with a force-push or a
/// rebase under any circumstance. The caller (`tick::run_fast_tier`) posts
/// the escalation comment on the PR for a `Held` outcome via the same
/// generic path it already uses for every other `Held` reason.
fn execute_adopted(deps: &RerollDeps, bead: &mut BeadOverlay) -> Result<RerollOutcome, DaemonError> {
    let branch = match bead.branch.clone() {
        Some(b) => b,
        None => {
            // Should not happen: adoption always sets `branch` to the
            // contributor's head_ref_name. Park rather than guess.
            bead.state = OverlayState::HumanHeld;
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
        "REROLL_ADOPTED_APPEND_ONLY_START",
        serde_json::json!({}),
        serde_json::json!({"branch": branch}),
    )?;

    let next_attempt = bead.attempt + 1;
    let commit_message = format!(
        "fix: address {} review feedback (attempt {})\n\n{}",
        deps.reviewer, next_attempt, deps.review_text
    );

    match deps.vcs.push_fix_commit(&branch, &commit_message) {
        Ok(()) => {
            bead.attempt = next_attempt;
            bead.reroll_count += 1;
            // Branch and pr_number are deliberately left unchanged: same
            // branch, same PR, still open. There is no factory session to
            // redispatch to, so the bead goes straight back to ATTESTED —
            // the next tick's verifier pass re-assesses the PR once CI
            // catches up to the new commit, exactly like any other push a
            // contributor makes to their own open PR.
            bead.state = OverlayState::Attested;
            deps.store.save(bead)?;
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_FIX_PUSHED",
                serde_json::json!({
                    "elapsedAutonomySeconds": bead.autonomy_secs,
                }),
                serde_json::json!({"branch": branch}),
            )?;
            Ok(RerollOutcome::Rerolled { new_branch: branch })
        }
        Err(e) => {
            // Append-only push failed — most likely a non-fast-forward
            // rejection, meaning genuine remediation would require a rebase
            // or force-push. The daemon has no authority to rewrite a
            // contributor's history, so this parks HUMAN_HELD rather than
            // ever falling back to `--force`.
            bead.state = OverlayState::HumanHeld;
            deps.store.save(bead)?;
            emit_telemetry(
                deps.telemetry_log,
                &bead.bead_id,
                bead.attempt,
                bead.state.as_str(),
                "REROLL_ADOPTED_APPEND_ONLY_FAILED",
                serde_json::json!({}),
                serde_json::json!({"branch": branch, "error": e.to_string()}),
            )?;
            Ok(RerollOutcome::Held(format!(
                "append-only fix push to adopted branch {branch} failed and needs a human \
                 (possible remote divergence or a base conflict requiring a rebase/force-push, \
                 which automation will not perform on a contributor-owned branch): {e}"
            )))
        }
    }
}
