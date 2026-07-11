// Automated `/er` runner (bead jleechan-qqq). Wired into the daemon fast tier
// to close the gate-6 = permanently `Unknown` gap documented in
// `docs/factory-goal-gap-review-2026-07-06.md` (blocker #4): without a
// reviewer invocation, `er_verdict` is `Absent`, gate 6 is `Unknown`, and
// every ATTESTED bead dead-ends in `HUMAN_HELD`.
//
// Design (see `docs/er-runner-design-2026-07-07.md`): the runner is a fresh
// subprocess (`claude --print` / `codex exec`) — independent process tree,
// no conversation memory of the implementing agent — whose reply is posted
// verbatim as a PR comment so the existing `parse_er_verdict` picks it up
// on the next tick. Idempotent (already-posted verdict ⇒ no-op), bounded
// (max 3 attempts per PR), and cooldown-throttled (default 300s) so a
// transient comment-fetch delay can't trigger duplicate spawns.
//
// ZFC: the verdict is decided by an LLM (claude/codex) via prompt, NOT by
// any keyword/heuristic router in daemon code. The only "routing" decisions
// the runner makes — "should we spawn this tick?" — are pure state
// (existing verdict, cooldown elapsed, attempt cap), matching the existing
// `skeptic_evidence` "is_real() then subprocess" split.

use crate::errors::DaemonError;
use crate::state::{BeadOverlay, OverlayState};
#[cfg(test)]
use crate::state::StateStore;
use crate::tick::TickDeps;
use crate::tools::{run_tool, PrComment, PrSnapshot};
use crate::verifier::{self, ErVerdict};

/// Maximum `/er` runner attempts per (bead, pr). After this many spawns
/// the runner stops retrying and gate 6 stays `Unknown` — the same
/// discipline `recover-held` uses (max_attempt=10).
pub const MAX_ER_RUNNER_ATTEMPTS: u32 = 3;

/// Cooldown between `/er` runner spawns for the same PR (seconds). Prevents
/// comment-fetch latency from triggering duplicate reviewer spawns within
/// the same PR-comment-post round-trip.
pub const ER_RUNNER_COOLDOWN_SECS: u64 = 300;

/// Wall-clock timeout for the spawned reviewer subprocess.
///
/// jleechan-hhmb: raised from 120s in lockstep with tick.rs's
/// `REVIEWER_TIMEOUT_SECS` — the /er reviewer does the same class of
/// `gh`-backed end-to-end PR investigation as the gate-7 skeptic, which
/// was live-measured at 2m27s on a 50-file PR (the old 120s cap killed it
/// every cycle).
pub const ER_RUNNER_TIMEOUT_SECS: u64 = 300;

/// Telemetry event names (mirrored in `factory-overlay.sh` and the CXDB
/// consumers). Kept as `&'static str` so the `emit` call in tick.rs needs
/// no allocation.
pub const EVT_DISPATCHED: &str = "ER_RUNNER_DISPATCHED";
pub const EVT_NOOP: &str = "ER_RUNNER_NOOP";
pub const EVT_CAPPED: &str = "ER_RUNNER_CAPPED";
pub const EVT_POSTED: &str = "ER_RUNNER_POSTED";

/// Outcome of one `maybe_run` call. Returned (not just internally logged)
/// so the call site can emit the right telemetry event AND so unit tests
/// can assert behavior without poking the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A `/er` verdict was already present in PR comments — nothing to do.
    AlreadyPosted(ErVerdict),
    /// Within the cooldown window after a prior spawn — suppress duplicate.
    Cooldown { elapsed_secs: u64, count: u32 },
    /// Attempt cap reached — runner stops retrying, gate 6 stays Unknown.
    Capped { count: u32 },
    /// Reviewer spawned, reply captured, PR comment posted.
    Posted { verdict: ErVerdict, count: u32 },
    /// Bead not in Attested state, or PR number missing — not applicable.
    NotApplicable,
}

/// Build the prompt sent to the spawned reviewer. Public for unit-test
/// pinning (so a regression that mutates the prompt is caught immediately).
pub fn build_er_prompt(bead_id: &str, pr: u64, target_repo: &str) -> String {
    format!(
        "You are the /er (evidence review) gate for an autonomous coding factory.\n\
         You are NOT the implementing agent; you are an INDEPENDENT reviewer.\n\
         \n\
         Review PR #{pr} in repo {target_repo}:\n\
           gh pr diff {pr} --repo {target_repo}\n\
           gh pr view {pr} --repo {target_repo} --json body,comments\n\
           gh pr checks {pr} --repo {target_repo}\n\
         \n\
         Decide whether the PR has REAL evidence (tests run, integration checks,\n\
         screenshots, videos, evidence bundles) supporting its claims.\n\
         \n\
         Reply with EXACTLY ONE LINE, no preamble, no markdown:\n\
           /er PASS\n\
         or\n\
           /er FAIL <one short reason>\n\
         or\n\
           /er PARTIAL <one short reason>\n\
         or\n\
           /er INCONCLUSIVE <one short reason>\n\
         \n\
         Bead id: {bead_id}. PR: {pr}.",
    )
}

/// Decide whether the runner should fire this tick, capture the verdict,
/// and post it as a PR comment. Idempotent: if `/er` is already in the
/// PR comments, this is a no-op.
///
/// `now_epoch` is the current unix epoch in seconds (injected for tests).
pub fn maybe_run(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    now_epoch: u64,
) -> Result<Outcome, DaemonError> {
    // 1. Bead + PR must be valid (overlay is loaded to validate state + PR
    //    consistency; the actual fetch is done in step 2). Captured (not
    //    discarded) so its resolved repo (jleechan-9xrs Stage D —
    //    `overlay.repo(cfg)`) can be threaded through the snapshot fetch,
    //    reviewer prompt, and posted comment's ext_ref below instead of
    //    `deps.cfg.target_repo`.
    let overlay = match deps.store.load(bead_id)? {
        Some(o) if o.state == OverlayState::Attested && o.pr_number == Some(pr) => o,
        _ => return Ok(Outcome::NotApplicable),
    };
    let repo = overlay.repo(&deps.cfg).to_string();

    // 2. Already posted? (idempotence) — jleechan-nplh: only a verdict
    //    posted at/after the CURRENT head commit counts. A verdict that
    //    predates the head verified code that no longer exists; treating it
    //    as valid would short-circuit re-verification forever (live
    //    incident: worldarchitect.ai#7888, a 2026-07-08 PASS suppressed
    //    re-review of a 2026-07-10 head ~100 commits later).
    let snapshot = deps.scm.pr_snapshot_for_repo(&repo, pr)?;
    let existing =
        verifier::parse_er_verdict_since(&snapshot.comments, snapshot.head_committed_epoch);
    if existing != ErVerdict::Absent {
        return Ok(Outcome::AlreadyPosted(existing));
    }

    // 3. Capped?
    let (count, last_at) = deps.store.er_runner_attempt(bead_id)?;
    if count >= MAX_ER_RUNNER_ATTEMPTS {
        return Ok(Outcome::Capped { count });
    }

    // 4. Cooldown?
    if let Some(last) = last_at {
        let elapsed = now_epoch.saturating_sub(last);
        if elapsed < ER_RUNNER_COOLDOWN_SECS {
            return Ok(Outcome::Cooldown {
                elapsed_secs: elapsed,
                count,
            });
        }
    }

    // 5. Spawn reviewer
    let prompt = build_er_prompt(bead_id, pr, &repo);
    let reply = if !deps.llm.is_real() {
        deps.llm.judge(&prompt)?
    } else {
        spawn_reviewer(&prompt)?
    };

    // 6. Post verbatim reply as a PR comment. If the post fails (network
    //    blip, 403, etc.), do NOT consume an attempt — propagate the error
    //    so the next tick can retry without burning one of the 3 slots.
    let body = format!(
        "🤖 **[dark-factory /er]** Evidence review verdict:\n\n```\n{reply}\n```"
    );
    let ext_ref = format!("{}#{}", repo, pr);
    deps.tracker.comment_external(&ext_ref, &body)?;

    // 7. Increment attempt counter — only AFTER the comment landed, so a
    //    transient `comment_external` failure doesn't burn a retry slot.
    let new_count = deps.store.incr_er_runner_attempt(bead_id, now_epoch)?;

    // 8. Parse verdict from the reply (NOT just from the now-posted comment —
    //    parse_er_verdict would find /er PASS etc. inside the formatted body,
    //    but parsing the raw reply is more honest and isolates the verdict
    //    grammar from any future comment-format changes).
    let verdict = parse_reviewer_reply(&reply);

    Ok(Outcome::Posted {
        verdict,
        count: new_count,
    })
}

/// Spawn the actual reviewer subprocess (production path). Mirrors the
/// `skeptic_evidence` discipline: try `claude` from the nvm install first,
/// fall back to `claude` on `$PATH`. Uses `run_tool` for bounded execution
/// (timeout + concurrent pipe draining, see tools.rs::run_tool).
fn spawn_reviewer(prompt: &str) -> Result<String, DaemonError> {
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
        ER_RUNNER_TIMEOUT_SECS,
    )
}

/// Parse the reviewer's raw reply text into an `ErVerdict`. Re-uses the
/// same grammar `parse_er_verdict` applies to PR comments so the runner's
/// output format and the comment-scanner stay in lockstep.
pub fn parse_reviewer_reply(raw: &str) -> ErVerdict {
    let lower = raw.to_ascii_lowercase();
    let mut start = 0;
    while let Some(idx) = lower[start..].find("/er") {
        let abs = start + idx;
        let valid_start = abs == 0
            || !lower.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let valid_end = abs + 3 == lower.len()
            || {
                let next = lower.as_bytes()[abs + 3] as char;
                !next.is_ascii_alphanumeric() && next != '-' && next != '_'
            };
        if valid_start && valid_end {
            let after = &lower[abs + 3..];
            for token in after
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|s| !s.is_empty())
            {
                match token {
                    "pass" | "passed" => return ErVerdict::Pass,
                    "partial" | "partially" => return ErVerdict::Partial,
                    "fail" | "failed" => return ErVerdict::Fail,
                    "inconclusive" => return ErVerdict::Inconclusive,
                    _ => {}
                }
            }
        }
        start = abs + 3;
    }
    ErVerdict::Absent
}

/// Convenience wrapper around `parse_reviewer_reply` used in tests to build
/// scripted `PrComment` lists.
pub fn comments_with_verdict(text: &str) -> Vec<PrComment> {
    vec![PrComment {
        author: "dark-factory-er".into(),
        body: text.into(),
        created_at_epoch: 0,
    }]
}

/// Helper for unit tests / examples: a `PrSnapshot` with the given
/// comments and an otherwise all-green profile.
pub fn snapshot_with_comments(pr: u64, comments: Vec<PrComment>) -> PrSnapshot {
    PrSnapshot {
        pr_number: pr,
        ci_success: true,
        mergeable: true,
        coderabbit_approved: true,
        bugbot_error_count: 0,
        unresolved_thread_count: Some(0),
        head_sha: "deadbeef".into(),
        body: String::new(),
        comments,
        files: vec![],
        updated_at_epoch: 0,
        ci_status: "green".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
        head_committed_epoch: 0,
    }
}

/// Suppress unused-import warnings for items only referenced in tests.
#[allow(dead_code)]
fn _force_use(_: &BeadOverlay) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Issue, Permission, PrSnapshot};

    // ----- Local test scaffolding (mirrors tests/common/mod.rs but lives
    // inside the unit-test module so it doesn't widen the public surface
    // of `er_runner.rs`). -----

    #[derive(Default)]
    struct L {
        response: std::cell::RefCell<Option<Result<String, String>>>,
        real: bool,
        calls: std::cell::RefCell<Vec<String>>,
    }
    impl crate::tools::Llm for L {
        fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
            self.calls.borrow_mut().push(format!("judge({prompt})"));
            match self.response.borrow().as_ref() {
                Some(Ok(t)) => Ok(t.clone()),
                Some(Err(e)) => Err(DaemonError::Parse(e.clone())),
                None => Ok(String::new()),
            }
        }
        fn is_real(&self) -> bool {
            self.real
        }
    }

    #[derive(Default)]
    struct S {
        snapshots: std::cell::RefCell<std::collections::HashMap<u64, PrSnapshot>>,
        issues: Vec<Issue>,
        perms: std::collections::HashMap<String, Permission>,
        calls: std::cell::RefCell<Vec<String>>,
    }
    impl crate::tools::Scm for S {
        fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("labeled_issues({label})"));
            Ok(self.issues.clone())
        }
        fn labeled_prs(&self, label: &str) -> Result<Vec<crate::tools::LabeledPr>, DaemonError> {
            self.calls.borrow_mut().push(format!("labeled_prs({label})"));
            Ok(Vec::new())
        }
        fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("collaborator_permission({login})"));
            Ok(self.perms.get(login).copied().unwrap_or(Permission::None))
        }
        fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
            self.calls.borrow_mut().push(format!("pr_snapshot({pr})"));
            self.snapshots
                .borrow()
                .get(&pr)
                .cloned()
                .ok_or_else(|| DaemonError::Tool {
                    tool: "gh".into(),
                    rc: 1,
                    stderr: format!("no scripted snapshot for pr {pr}"),
                })
        }
        /// jleechan-9xrs Stage D regression coverage: records the `repo`
        /// argument distinctly so tests can assert `maybe_run` fetched the
        /// bead's OWN resolved repo, not `cfg.target_repo`.
        fn pr_snapshot_for_repo(&self, repo: &str, pr: u64) -> Result<PrSnapshot, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("pr_snapshot_for_repo({repo},{pr})"));
            self.snapshots
                .borrow()
                .get(&pr)
                .cloned()
                .ok_or_else(|| DaemonError::Tool {
                    tool: "gh".into(),
                    rc: 1,
                    stderr: format!("no scripted snapshot for pr {pr}"),
                })
        }
        fn close_pr(&self, pr: u64, _c: &str) -> Result<(), DaemonError> {
            self.calls.borrow_mut().push(format!("close_pr({pr})"));
            Ok(())
        }
        fn remote_branch_last_commit(&self, _b: &str) -> Result<Option<u64>, DaemonError> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct T {
        comment_calls: std::cell::RefCell<Vec<(String, String)>>,
    }
    impl crate::tools::Tracker for T {
        fn fetch_candidates(&self) -> Result<Vec<crate::tools::Bead>, DaemonError> {
            Ok(Vec::new())
        }
        fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
            Ok(std::collections::HashSet::new())
        }
        fn create_bead(
            &self,
            _t: &str,
            _b: &str,
            _e: &str,
        ) -> Result<String, DaemonError> {
            Ok("fake".into())
        }
        fn comment_external(&self, ext_ref: &str, body: &str) -> Result<(), DaemonError> {
            self.comment_calls
                .borrow_mut()
                .push((ext_ref.to_string(), body.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct Ss;
    impl crate::tools::Sessions for Ss {
        fn active_count(&self) -> Result<usize, DaemonError> { Ok(0) }
        fn spawn(&self, _s: &crate::tools::SpawnSpec) -> Result<crate::tools::SessionId, DaemonError> {
            Ok(crate::tools::SessionId("fake".into()))
        }
        fn attach(&self, _b: &str, _i: &str) -> Result<crate::tools::SessionId, DaemonError> {
            Ok(crate::tools::SessionId("fake".into()))
        }
        fn stop(&self, _i: &crate::tools::SessionId) -> Result<(), DaemonError> { Ok(()) }
        fn is_quiescent(&self, _i: &crate::tools::SessionId) -> Result<bool, DaemonError> { Ok(true) }
    }

    #[derive(Default)]
    struct V;
    impl crate::tools::Vcs for V {
        fn base_head(&self, _b: &str) -> Result<String, DaemonError> { Ok("deadbeef".into()) }
        fn create_branch_at(&self, _n: &str, _s: &str) -> Result<(), DaemonError> { Ok(()) }
        fn head_sha(&self, _b: &str) -> Result<String, DaemonError> { Ok("deadbeef".into()) }
        fn is_remote_ahead(&self, _b: &str, _r: &str) -> Result<bool, DaemonError> { Ok(false) }
        fn push_fix_commit(&self, _branch: &str, _message: &str) -> Result<(), DaemonError> { Ok(()) }
        fn remote_head_sha(&self, _branch: &str) -> Result<String, DaemonError> { Ok("deadbeef".into()) }
        fn is_ancestor(&self, _ancestor_sha: &str, _descendant_sha: &str) -> Result<bool, DaemonError> { Ok(true) }
    }

    // Local in-memory StateStore that records the er_runner_attempt counter.
    #[derive(Default)]
    struct St {
        overlays: std::cell::RefCell<std::collections::HashMap<String, BeadOverlay>>,
        er_counts: std::cell::RefCell<std::collections::HashMap<String, (u32, Option<u64>)>>,
        calls: std::cell::RefCell<Vec<String>>,
    }
    impl StateStore for St {
        fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
            self.calls.borrow_mut().push(format!("load({bead_id})"));
            Ok(self.overlays.borrow().get(bead_id).cloned())
        }
        fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("save({})", overlay.bead_id));
            self.overlays
                .borrow_mut()
                .insert(overlay.bead_id.clone(), overlay.clone());
            Ok(())
        }
        fn register_branch(&self, _b: &str, _br: &str) -> Result<(), DaemonError> { Ok(()) }
        fn owned_branches(&self) -> Result<Vec<String>, DaemonError> { Ok(Vec::new()) }
        fn bead_id_for_branch(&self, _b: &str) -> Result<Option<String>, DaemonError> { Ok(None) }
        fn increment_active_autonomy(&self, _e: u64) -> Result<Vec<BeadOverlay>, DaemonError> {
            Ok(Vec::new())
        }
        fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
            Ok(Vec::new())
        }
        fn bump_autonomy_secs(&self, _bead_id: &str, _delta_secs: u64) -> Result<(), DaemonError> {
            Ok(())
        }
        fn human_held_at_or_above_attempt(
            &self,
            _max_attempt: u32,
        ) -> Result<Vec<BeadOverlay>, DaemonError> {
            Ok(Vec::new())
        }
        fn save_rejection(
            &self,
            _b: &str,
            _a: u32,
            _r: &str,
            _h: &str,
            _t: &str,
        ) -> Result<(), DaemonError> {
            Ok(())
        }
        fn load_rejection(&self, _b: &str, _a: u32) -> Result<Option<(String, String)>, DaemonError> {
            Ok(None)
        }
        fn incr_er_runner_attempt(
            &self,
            bead_id: &str,
            now_epoch: u64,
        ) -> Result<u32, DaemonError> {
            let mut counts = self.er_counts.borrow_mut();
            let entry = counts.entry(bead_id.to_string()).or_insert((0, None));
            entry.0 += 1;
            entry.1 = Some(now_epoch);
            Ok(entry.0)
        }
        fn er_runner_attempt(
            &self,
            bead_id: &str,
        ) -> Result<(u32, Option<u64>), DaemonError> {
            Ok(self
                .er_counts
                .borrow()
                .get(bead_id)
                .copied()
                .unwrap_or((0, None)))
        }
    }

    fn test_cfg() -> crate::config::Config {
        crate::config::Config {
            target_repo: "owner/repo".into(),
            ao_project: None,
            base_branch: "main".into(),
            stage: 1,
            max_workers: 30,
            max_batch: 15,
            fast_tick_secs: 60,
            slow_tick_secs: 60,
            autonomy_timebox_secs: 10_800,
            budget_warn_usd: 20.0,
            spec_dir: ".factory/specs/".into(),
            repos: std::collections::HashMap::new(),
        }
    }

    fn attested_overlay(bead_id: &str, pr: u64) -> BeadOverlay {
        BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(format!("factory/{bead_id}-r1")),
            session_id: Some("s1".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        }
    }

    fn make_deps<'a>(
        scm: &'a S,
        tracker: &'a T,
        sessions: &'a Ss,
        llm: &'a L,
        store: &'a St,
        cfg: &'a crate::config::Config,
    ) -> TickDeps<'a> {
        TickDeps {
            scm,
            tracker,
            sessions,
            llm,
            store,
            vcs: &V,
            cfg,
            telemetry_log: std::path::Path::new("/tmp/afd_er_runner_unit_test.jsonl"),
        }
    }

    fn telemetry_cleanup() {
        let _ = std::fs::remove_file("/tmp/afd_er_runner_unit_test.jsonl");
    }

    // =========================================================
    // TDD red phase: tests must FAIL until implementation lands.
    // Each test names the gap it pins down.
    // =========================================================

    #[test]
    fn noop_when_pass_comment_already_present() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default(); // mock
        let store = St::default();
        let cfg = test_cfg();

        let pr = 101;
        let bead = "b1";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, comments_with_verdict("/er PASS")));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 1_000_000).unwrap();
        assert_eq!(outcome, Outcome::AlreadyPosted(ErVerdict::Pass));
        assert!(llm.calls.borrow().is_empty(), "no reviewer spawn expected");
        assert!(tracker.comment_calls.borrow().is_empty(), "no PR comment expected");
    }

    #[test]
    fn noop_when_fail_comment_already_present() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        let store = St::default();
        let cfg = test_cfg();

        let pr = 102;
        let bead = "b2";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots.borrow_mut().insert(
            pr,
            snapshot_with_comments(pr, comments_with_verdict("/er FAIL no integration tests")),
        );

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 1_000_000).unwrap();
        assert_eq!(outcome, Outcome::AlreadyPosted(ErVerdict::Fail));
        assert!(llm.calls.borrow().is_empty());
    }

    #[test]
    fn spawns_reviewer_when_no_verdict_and_posts_comment() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS — saw integration test output".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 103;
        let bead = "b3";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 1_000_000).unwrap();
        assert_eq!(
            outcome,
            Outcome::Posted {
                verdict: ErVerdict::Pass,
                count: 1,
            }
        );
        assert_eq!(llm.calls.borrow().len(), 1, "reviewer spawn expected");
        let comment_calls = tracker.comment_calls.borrow();
        assert_eq!(comment_calls.len(), 1, "exactly one PR comment expected");
        let (ext_ref, body) = &comment_calls[0];
        assert_eq!(ext_ref, "owner/repo#103");
        assert!(body.contains("/er PASS"), "verbatim verdict in comment: {body:?}");
    }

    /// jleechan-9xrs Stage D: when the bead has an explicit `target_repo`
    /// DIFFERENT from `cfg.target_repo`, `maybe_run` must fetch the PR
    /// snapshot, build the reviewer prompt, and post the verdict comment
    /// against the BEAD's OWN repo — not the daemon-global
    /// `cfg.target_repo`. Regression for the multi-repo dispatch fix
    /// (docs/multirepo-dispatch-investigation-2026-07-11.md); this is the
    /// "er_runner prompts/ext_ref" half of Stage D's acceptance criteria.
    /// The companion `spawns_reviewer_when_no_verdict_and_posts_comment`
    /// test above (bead `target_repo: None`) pins the legacy-fallback half.
    #[test]
    fn cross_repo_bead_uses_its_own_repo_not_cfg_target_repo() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS — saw integration test output".into()));
        let store = St::default();
        let cfg = test_cfg(); // cfg.target_repo == "owner/repo"

        let pr = 999;
        let bead = "b-cross-repo";
        let mut overlay = attested_overlay(bead, pr);
        overlay.target_repo = Some("otherorg/other-repo".to_string());
        store.overlays.borrow_mut().insert(bead.into(), overlay);
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 1_000_000).unwrap();
        assert_eq!(
            outcome,
            Outcome::Posted {
                verdict: ErVerdict::Pass,
                count: 1,
            }
        );

        // The posted comment's ext_ref must target the bead's own repo.
        let comment_calls = tracker.comment_calls.borrow();
        assert_eq!(comment_calls.len(), 1, "exactly one PR comment expected");
        let (ext_ref, _body) = &comment_calls[0];
        assert_eq!(
            ext_ref, "otherorg/other-repo#999",
            "escalation/er comment must target the bead's own repo, not cfg.target_repo"
        );

        // The snapshot fetch must have gone through pr_snapshot_for_repo
        // with the bead's own repo, not the plain (cfg-bound) pr_snapshot.
        let scm_calls = scm.calls.borrow();
        assert!(
            scm_calls
                .iter()
                .any(|c| c == "pr_snapshot_for_repo(otherorg/other-repo,999)"),
            "expected pr_snapshot_for_repo with the bead's own repo, got: {scm_calls:?}"
        );
        assert!(
            !scm_calls.iter().any(|c| c.contains("owner/repo")),
            "must never fall back to cfg.target_repo for a bead with an \
             explicit target_repo, got: {scm_calls:?}"
        );

        // The reviewer prompt (captured via the mock LLM's judge() call log)
        // must reference the bead's own repo, not cfg.target_repo.
        let llm_calls = llm.calls.borrow();
        assert_eq!(llm_calls.len(), 1, "reviewer spawn expected");
        assert!(
            llm_calls[0].contains("otherorg/other-repo"),
            "reviewer prompt must embed the bead's own repo, got: {:?}",
            llm_calls[0]
        );
        assert!(
            !llm_calls[0].contains("owner/repo"),
            "reviewer prompt must not leak cfg.target_repo, got: {:?}",
            llm_calls[0]
        );
    }

    #[test]
    fn cooldown_suppresses_duplicate_spawn_within_window() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 104;
        let bead = "b4";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);

        let t0 = 2_000_000;
        let first = maybe_run(&deps, bead, pr, t0).unwrap();
        assert!(matches!(first, Outcome::Posted { count: 1, .. }));

        // Second call 60s later — within the 300s cooldown
        let t1 = t0 + 60;
        let second = maybe_run(&deps, bead, pr, t1).unwrap();
        match second {
            Outcome::Cooldown { elapsed_secs: 60, count: 1 } => {}
            other => panic!("expected Cooldown(60, 1), got {other:?}"),
        }
        assert_eq!(llm.calls.borrow().len(), 1, "second call must NOT spawn reviewer");
        assert_eq!(
            tracker.comment_calls.borrow().len(),
            1,
            "second call must NOT post comment"
        );
    }

    #[test]
    fn outside_cooldown_resumes_spawning() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er FAIL broken tests".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 105;
        let bead = "b5";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let t0 = 3_000_000;
        let _ = maybe_run(&deps, bead, pr, t0).unwrap();

        // Second call 400s later — past the 300s cooldown
        let t1 = t0 + ER_RUNNER_COOLDOWN_SECS + 100;
        let second = maybe_run(&deps, bead, pr, t1).unwrap();
        assert!(matches!(second, Outcome::Posted { count: 2, verdict: ErVerdict::Fail, .. }));
        assert_eq!(llm.calls.borrow().len(), 2);
        assert_eq!(tracker.comment_calls.borrow().len(), 2);
    }

    #[test]
    fn caps_at_max_attempts() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er FAIL x".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 106;
        let bead = "b6";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        // Fire MAX_ER_RUNNER_ATTEMPTS times, spacing each past the cooldown
        let mut now = 4_000_000;
        for i in 1..=MAX_ER_RUNNER_ATTEMPTS {
            let outcome = maybe_run(&deps, bead, pr, now).unwrap();
            assert_eq!(
                outcome,
                Outcome::Posted {
                    verdict: ErVerdict::Fail,
                    count: i,
                },
                "attempt {i} should be Posted"
            );
            now += ER_RUNNER_COOLDOWN_SECS + 10;
        }
        // Fourth attempt must be Capped
        let capped = maybe_run(&deps, bead, pr, now).unwrap();
        assert_eq!(
            capped,
            Outcome::Capped {
                count: MAX_ER_RUNNER_ATTEMPTS
            }
        );
        assert_eq!(llm.calls.borrow().len() as u32, MAX_ER_RUNNER_ATTEMPTS);
        assert_eq!(tracker.comment_calls.borrow().len() as u32, MAX_ER_RUNNER_ATTEMPTS);
    }

    #[test]
    fn not_applicable_when_overlay_state_is_not_attested() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        let store = St::default();
        let cfg = test_cfg();

        let pr = 107;
        let bead = "b7";
        let mut overlay = attested_overlay(bead, pr);
        overlay.state = OverlayState::Dispatched;
        store.overlays.borrow_mut().insert(bead.into(), overlay);
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 5_000_000).unwrap();
        assert_eq!(outcome, Outcome::NotApplicable);
        assert!(llm.calls.borrow().is_empty());
    }

    #[test]
    fn not_applicable_when_pr_number_missing_on_overlay() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        let store = St::default();
        let cfg = test_cfg();

        let bead = "b8";
        let mut overlay = attested_overlay(bead, 999);
        overlay.pr_number = None;
        store.overlays.borrow_mut().insert(bead.into(), overlay);

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, 999, 6_000_000).unwrap();
        assert_eq!(outcome, Outcome::NotApplicable);
        assert!(llm.calls.borrow().is_empty());
    }

    #[test]
    fn parse_reviewer_reply_handles_all_grammar_tokens() {
        assert_eq!(parse_reviewer_reply("/er PASS"), ErVerdict::Pass);
        assert_eq!(parse_reviewer_reply("/er PASS — saw CI run"), ErVerdict::Pass);
        assert_eq!(parse_reviewer_reply("/er FAIL broken"), ErVerdict::Fail);
        assert_eq!(parse_reviewer_reply("/er PARTIAL missing coverage"), ErVerdict::Partial);
        assert_eq!(parse_reviewer_reply("/er INCONCLUSIVE flaky ci"), ErVerdict::Inconclusive);
        // Boundary: word-fragment must NOT match (e.g. "superpass")
        assert_eq!(parse_reviewer_reply("supersede"), ErVerdict::Absent);
        // Boundary: no `/er` token at all
        assert_eq!(parse_reviewer_reply("looks good"), ErVerdict::Absent);
    }

    #[test]
    fn build_er_prompt_contains_pr_and_repo() {
        let p = build_er_prompt("b9", 42, "owner/repo");
        assert!(p.contains("#42"));
        assert!(p.contains("owner/repo"));
        assert!(p.contains("b9"));
        assert!(p.contains("/er PASS"));
    }

    #[test]
    fn posted_outcome_increments_attempt_counter() {
        telemetry_cleanup();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 110;
        let bead = "b10";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 7_000_000).unwrap();
        assert_eq!(
            outcome,
            Outcome::Posted {
                verdict: ErVerdict::Pass,
                count: 1,
            }
        );
        let (count, last_at) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(count, 1);
        assert_eq!(last_at, Some(7_000_000));
    }
}
