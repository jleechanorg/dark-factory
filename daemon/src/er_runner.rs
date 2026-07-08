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
pub const ER_RUNNER_TIMEOUT_SECS: u64 = 120;

/// Generous upper bound on total forked-child wall-clock time: the child's
/// own reviewer subprocess is already bounded by `ER_RUNNER_TIMEOUT_SECS`
/// (120s) via `run_tool` inside `spawn_reviewer`, plus ~30s for the `gh`
/// comment post (also `run_tool`-bounded). 180s leaves headroom without
/// letting a hung child live indefinitely.
pub const ER_RUNNER_CHILD_REAP_TIMEOUT_SECS: u64 = 180;

/// Telemetry event names (mirrored in `factory-overlay.sh` and the CXDB
/// consumers). Kept as `&'static str` so the `emit` call in tick.rs needs
/// no allocation.
pub const EVT_DISPATCHED: &str = "ER_RUNNER_DISPATCHED";
pub const EVT_NOOP: &str = "ER_RUNNER_NOOP";
pub const EVT_CAPPED: &str = "ER_RUNNER_CAPPED";
pub const EVT_POSTED: &str = "ER_RUNNER_POSTED";

/// Env-var name. When set to "1", `maybe_run` calls `child_run_er` inline
/// instead of forking a separate process. Used by:
///   - integration tests (so a single `run_tick` still drives the comment
///     post through the same in-memory deps; production never sets this)
///   - manual debugging (run `DARK_FACTORY_ER_INLINE=1 daemon --tick-once`
///     to see the full pipeline in one process)
///
/// Default (unset / anything else) → production uses `Command::new`
/// fork so a 60–120s `claude --print` review does NOT block the
/// slow-tier tick (bead jleechan-bpb6).
pub const ER_RUNNER_INLINE_ENV: &str = "DARK_FACTORY_ER_INLINE";

/// Test seam. Production never writes to this; tests flip it to `true`
/// for the duration of an assertion to force inline child execution
/// without spawning a real subprocess. Atomic so parallel `cargo test`
/// threads don't race. Default `false`.
pub static ER_RUNNER_FORCE_INLINE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Outcome of one `maybe_run` call. Returned (not just internally logged)
/// so the call site can emit the right telemetry event AND so unit tests
/// can assert behavior without poking the event log.
///
/// jleechan-bpb6: the parent (slow-tier tick) only does the synchronous
/// preflight + dispatch reservation. The actual reviewer spawn + PR
/// comment post + verdict parse happens in a child process via
/// `child_run_er`. The parent sees `Dispatched` and treats it as no-op
/// for that tick — gate 6 stays `Unknown` until a future tick re-fetches
/// the snapshot and the just-posted comment flips it to `Pass`/`Fail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A `/er` verdict was already present in PR comments — nothing to do.
    AlreadyPosted(ErVerdict),
    /// Within the cooldown window after a prior spawn — suppress duplicate.
    Cooldown { elapsed_secs: u64, count: u32 },
    /// Attempt cap reached — runner stops retrying, gate 6 stays Unknown.
    Capped { count: u32 },
    /// Synchronous preflight passed; slot reserved; child process forked
    /// (or inline-mode ran `child_run_er`). Parent must NOT try to use a
    /// verdict this tick — child is still running.
    Dispatched { count: u32 },
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

/// Decide whether the runner should fire this tick. jleechan-bpb6:
/// synchronous preflight + dispatch reservation only; the actual reviewer
/// spawn + PR comment post happens in a child process via `child_run_er`.
/// Returns immediately on success (`Outcome::Dispatched`); the slow-tier
/// tick treats `Dispatched` as no-op and moves on. The child writes the
/// verdict as a PR comment; a future tick picks it up via
/// `parse_er_verdict` and flips gate 6 from `Unknown` to `Pass`/`Fail`.
///
/// Idempotent: if `/er` is already in the PR comments, this is a no-op.
///
/// `now_epoch` is the current unix epoch in seconds (injected for tests).
pub fn maybe_run(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    now_epoch: u64,
) -> Result<Outcome, DaemonError> {
    // 1. Bead + PR must be valid (overlay is loaded to validate state + PR
    //    consistency; the actual fetch is done in step 2)
    match deps.store.load(bead_id)? {
        Some(o) if o.state == OverlayState::Attested && o.pr_number == Some(pr) => {}
        _ => return Ok(Outcome::NotApplicable),
    };

    // 2. Already posted? (idempotence)
    let snapshot = deps.scm.pr_snapshot(pr)?;
    let existing = verifier::parse_er_verdict(&snapshot.comments);
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

    // 5. Reserve dispatch slot — increment the counter BEFORE the child
    //    forks so a duplicate parent tick can't race and burn the slot.
    //    The child does NOT re-increment; the reservation is the parent's
    //    responsibility (single-writer invariant on attempt count).
    let count = deps.store.incr_er_runner_attempt(bead_id, now_epoch)?;

    // 6. Either run inline (tests / debug) or fork a child (production).
    //    Both paths return `Outcome::Dispatched`; the parent's contract is
    //    "the work is in flight — don't try to read a verdict this tick."
    dispatch_to_child(deps, bead_id, pr, now_epoch)?;

    Ok(Outcome::Dispatched { count })
}

/// Run the async part of the `/er` runner in a child process (production)
/// or inline in the parent (when `DARK_FACTORY_ER_INLINE=1`). The parent
/// has already incremented the attempt counter and committed to
/// `Outcome::Dispatched`; this function must not double-count.
fn dispatch_to_child(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    now_epoch: u64,
) -> Result<(), DaemonError> {
    let inline = ER_RUNNER_FORCE_INLINE.load(std::sync::atomic::Ordering::SeqCst)
        || std::env::var(ER_RUNNER_INLINE_ENV).as_deref() == Ok("1");

    if inline {
        // Inline mode: run child work synchronously with the shared deps.
        // Used by integration tests so a single `run_tick` still drives
        // the comment post through the same in-memory fakes. NOT used in
        // production — defeats the whole point of jleechan-bpb6.
        child_run_er_inline(deps, bead_id, pr, now_epoch);
        return Ok(());
    }

    // Production: fork a separate process so the slow-tier tick returns
    // immediately and the 60–120s `claude --print` review does not block
    // other beads' intake / verifier ticks. The child receives the
    // minimum context it needs (bead_id + pr + now_epoch + target_repo)
    // and re-creates its own production adapters from config.
    //
    // Test binaries can't safely fork themselves (the child would re-run
    // the test runner). `is_test_binary` detects `target/debug/deps/...`
    // (cargo test layout) and the `--list` / `--exact` flags cargo test
    // passes, returning a no-op so unit tests asserting parent behavior
    // can run without side effects.
    if is_test_binary() {
        return Ok(());
    }

    // SAFETY: any error from the fork (e.g. current_exe unreadable) is
    // swallowed — the reservation has already been recorded, so the next
    // tick will retry within the cooldown window. Returning Ok(()) keeps
    // the parent in the same `Dispatched` state.
    match fork_er_child(bead_id, pr, now_epoch, &deps.cfg.target_repo) {
        Ok(_pid) => {}
        Err(e) => {
            // Reservation was made; child fork failed. Log to stderr so
            // operators can debug; next tick will retry past cooldown.
            eprintln!(
                "er_runner: child fork failed for bead={bead_id} pr={pr}: {e:?}"
            );
        }
    }
    Ok(())
}

/// Returns true if the current process looks like a `cargo test` binary.
/// `target/debug/deps/daemon-<hash>` is cargo's per-test layout; the
/// `--list` / `--exact` flags only appear when libtest is driving the
/// process. We bail out of forking in that case so the test runner
/// isn't re-invoked as a child (would run the whole suite again).
fn is_test_binary() -> bool {
    let exe = std::env::current_exe().ok();
    let path_is_test = exe
        .map(|p| {
            let s = p.to_string_lossy();
            s.contains("/target/debug/deps/") || s.contains("/target/release/deps/")
        })
        .unwrap_or(false);
    let args_have_test_flag = std::env::args().any(|a| {
        a == "--list" || a == "--exact" || a == "--include-ignored" || a.starts_with("--test-threads")
    });
    path_is_test || args_have_test_flag
}

/// Poll a forked `/er` child to completion, killing it if it exceeds
/// `ER_RUNNER_CHILD_REAP_TIMEOUT_SECS`, and ALWAYS `wait()`s so the OS can
/// reap it (an un-`wait()`-ed exited child is a zombie process; an
/// un-bounded still-running child is an orphan that outlives its purpose).
/// Runs on a detached background thread so the parent tick loop returns
/// immediately after `fork_er_child` — this function must never be awaited
/// synchronously from `dispatch_to_child`.
fn reap_er_child(mut child: std::process::Child, bead_id: String, pr: u64) {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(ER_RUNNER_CHILD_REAP_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    eprintln!(
                        "er_runner: forked child exited non-zero for bead={bead_id} pr={pr}: {status:?}"
                    );
                }
                return;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "er_runner: forked child exceeded {ER_RUNNER_CHILD_REAP_TIMEOUT_SECS}s \
                         timeout for bead={bead_id} pr={pr}; killing"
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!(
                    "er_runner: try_wait failed while reaping forked child for bead={bead_id} pr={pr}: {e}"
                );
                return;
            }
        }
    }
}

/// Spawn the daemon binary as a child process with `--child-run-er`. The
/// child invocation is handled by `main.rs` which reconstructs production
/// adapters from config and runs `child_run_er_owned`. PID is returned
/// for observability; we spawn a detached reaper thread to bound and reap
/// the child so it doesn't become a zombie/orphan.
fn fork_er_child(
    bead_id: &str,
    pr: u64,
    now_epoch: u64,
    target_repo: &str,
) -> Result<u32, DaemonError> {
    let exe = std::env::current_exe().map_err(|e| DaemonError::Tool {
        tool: "std::env::current_exe".into(),
        rc: 1,
        stderr: e.to_string(),
    })?;
    let child = std::process::Command::new(exe)
        .args([
            "--child-run-er",
            "--bead-id",
            bead_id,
            "--pr",
            &pr.to_string(),
            "--now-epoch",
            &now_epoch.to_string(),
            "--target-repo",
            target_repo,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| DaemonError::Tool {
            tool: "std::process::Command::spawn".into(),
            rc: 1,
            stderr: e.to_string(),
        })?;
    let pid = child.id();
    let bead_id_owned = bead_id.to_string();
    std::thread::spawn(move || reap_er_child(child, bead_id_owned, pr));
    Ok(pid)
}

/// Shared child work: spawn reviewer, post PR comment, parse verdict.
/// Used by BOTH the inline test/debug path (`child_run_er_inline`, via
/// `TickDeps` trait objects) and the real forked-child path
/// (`run_child_main`, via owned production adapters) so the two paths
/// cannot drift out of sync.
fn run_er_child_work(
    llm: &dyn crate::tools::Llm,
    tracker: &dyn crate::tools::Tracker,
    bead_id: &str,
    pr: u64,
    target_repo: &str,
) -> ErVerdict {
    // 1. Spawn reviewer (mock or real per llm.is_real()).
    let prompt = build_er_prompt(bead_id, pr, target_repo);
    let reply = match if !llm.is_real() {
        llm.judge(&prompt)
    } else {
        spawn_reviewer(&prompt)
    } {
        Ok(s) => s,
        Err(e) => {
            eprintln!("er_runner: child spawn failed for bead={bead_id} pr={pr}: {e:?}");
            return ErVerdict::Absent;
        }
    };

    // 2. Post verbatim reply as a PR comment. If post fails (network blip,
    //    403, etc.) the reservation slot is already counted; the next tick
    //    will burn another attempt after cooldown.
    let body = format!(
        "🤖 **[dark-factory /er]** Evidence review verdict:\n\n```\n{reply}\n```"
    );
    let ext_ref = format!("{}#{}", target_repo, pr);
    if let Err(e) = tracker.comment_external(&ext_ref, &body) {
        eprintln!("er_runner: child post failed for bead={bead_id} pr={pr}: {e:?}");
        return ErVerdict::Absent;
    }

    // 3. Parse verdict from the raw reply (NOT from the just-posted
    //    comment — isolates the verdict grammar from any future comment-
    //    format changes).
    parse_reviewer_reply(&reply)
}

/// Inline-mode child work: spawn the reviewer, post the verbatim reply as
/// a PR comment. Counter is NOT incremented (parent reserved the slot).
/// Returns the parsed verdict so callers / tests can assert on it.
fn child_run_er_inline(
    deps: &TickDeps,
    bead_id: &str,
    pr: u64,
    _now_epoch: u64,
) -> ErVerdict {
    run_er_child_work(deps.llm, deps.tracker, bead_id, pr, &deps.cfg.target_repo)
}

/// Owned-deps entry point invoked by the child process (production path).
/// Called from `main.rs`'s `--child-run-er` handler with argv already
/// parsed. Builds REAL production adapters (`CliTracker`, `ChainLlm` — both
/// zero-field unit structs, no config needed) and runs the same child work
/// `child_run_er_inline` runs in tests, but as a genuinely separate forked
/// process with no shared memory with the parent.
///
/// Reporting mechanism: the child's ONLY side effect is posting the
/// reviewer's verbatim reply as a PR comment via `Tracker::comment_external`
/// (same as the inline path). GitHub is the durable, externally-visible
/// state store — there is no separate file/CXDB write-back needed, because
/// the PARENT's next tick already re-fetches `pr_snapshot(pr).comments` and
/// runs `parse_er_verdict` over them (see `maybe_run` step 2). This is the
/// existing idempotent hand-off contract; the forked child doesn't need to
/// invent a new one.
pub fn run_child_main(
    bead_id: &str,
    pr: u64,
    now_epoch: u64,
    target_repo: &str,
) -> Result<(), DaemonError> {
    eprintln!(
        "er_runner[child]: start bead={bead_id} pr={pr} now_epoch={now_epoch} repo={target_repo}"
    );
    let llm = crate::adapters::ChainLlm;
    let tracker = crate::adapters::CliTracker;
    let verdict = run_er_child_work(&llm, &tracker, bead_id, pr, target_repo);
    eprintln!(
        "er_runner[child]: done bead={bead_id} pr={pr} verdict={verdict:?}"
    );
    Ok(())
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
        unresolved_thread_count: 0,
        head_sha: "deadbeef".into(),
        body: String::new(),
        comments,
        files: vec![],
        updated_at_epoch: 0,
        ci_status: "green".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
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

    /// Process-global lock that serializes tests touching
    /// `ER_RUNNER_FORCE_INLINE`. The atomic itself is process-global, so
    /// two parallel tests setting different values would race; this
    /// mutex makes the inline-vs-production decision strictly serialized.
    /// `unwrap_or_else(|e| e.into_inner())` recovers from a poisoned mutex
    /// (a test panic while holding the lock shouldn't deadlock the suite).
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Lock the global test mutex and set inline-mode. Returns the
    /// guard; tests that hold it run with inline child execution.
    fn force_inline() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ER_RUNNER_FORCE_INLINE.store(true, std::sync::atomic::Ordering::SeqCst);
        guard
    }

    /// Lock the global test mutex and force production-mode (fork). For
    /// tests that need to assert the parent does NOT spawn / post inline.
    fn force_production() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ER_RUNNER_FORCE_INLINE.store(false, std::sync::atomic::Ordering::SeqCst);
        guard
    }

    /// Reset the inline flag at end of test (defensive — the next test's
    /// `force_inline`/`force_production` will set it explicitly anyway,
    /// but this makes accidental leaks obvious).
    fn reset_inline() {
        ER_RUNNER_FORCE_INLINE.store(false, std::sync::atomic::Ordering::SeqCst);
    }

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
    fn parent_dispatches_to_inline_child_which_spawns_and_posts() {
        telemetry_cleanup();
        let _lock = force_inline();
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
        // jleechan-bpb6: parent returns Dispatched (not Posted). The
        // inline child did the spawn + post, but the parent must NOT
        // try to use a verdict this tick.
        assert_eq!(outcome, Outcome::Dispatched { count: 1 });
        assert_eq!(llm.calls.borrow().len(), 1, "inline child spawned reviewer");
        let comment_calls = tracker.comment_calls.borrow();
        assert_eq!(comment_calls.len(), 1, "inline child posted exactly one PR comment");
        let (ext_ref, body) = &comment_calls[0];
        assert_eq!(ext_ref, "owner/repo#103");
        assert!(body.contains("/er PASS"), "verbatim verdict in comment: {body:?}");
        // Counter reserved by parent (single-writer invariant).
        let (count, _last) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(count, 1, "parent reserved the slot");
        reset_inline();
    }

    #[test]
    fn cooldown_suppresses_duplicate_dispatch_within_window() {
        telemetry_cleanup();
        let _lock = force_inline();
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
        assert!(matches!(first, Outcome::Dispatched { count: 1 }));

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
        reset_inline();
    }

    #[test]
    fn outside_cooldown_resumes_dispatch() {
        telemetry_cleanup();
        let _lock = force_inline();
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
        // jleechan-bpb6: parent returns Dispatched on second dispatch,
        // not Posted. The verdict is owned by the child process; parent
        // does NOT see it this tick.
        assert!(matches!(second, Outcome::Dispatched { count: 2 }));
        assert_eq!(llm.calls.borrow().len(), 2);
        assert_eq!(tracker.comment_calls.borrow().len(), 2);
        reset_inline();
    }

    #[test]
    fn caps_at_max_attempts() {
        telemetry_cleanup();
        let _lock = force_inline();
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
            // jleechan-bpb6: parent returns Dispatched; verdict is owned
            // by child process and never reaches parent.
            assert_eq!(
                outcome,
                Outcome::Dispatched { count: i },
                "attempt {i} should be Dispatched"
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
        reset_inline();
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
    fn dispatched_outcome_reserves_attempt_counter() {
        telemetry_cleanup();
        let _lock = force_inline();
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
        // jleechan-bpb6: parent returns Dispatched { count: 1 } — the
        // counter is reserved by the parent BEFORE the child forks so
        // a duplicate parent tick can't race and burn the slot.
        assert_eq!(outcome, Outcome::Dispatched { count: 1 });
        let (count, last_at) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(count, 1, "parent reserved the slot");
        assert_eq!(last_at, Some(7_000_000));
        reset_inline();
    }

    // =========================================================
    // jleechan-bpb6: NEW tests for the async process-child pattern.
    // These pin the behavioral contract that distinguishes this
    // refactor from the synchronous pre-bpb6 implementation.
    // =========================================================

    /// Production mode (no inline flag, test-binary path): parent must
    /// return Dispatched immediately WITHOUT spawning the reviewer
    /// and WITHOUT posting a comment. The child does the actual work
    /// in a forked process.
    #[test]
    fn parent_dispatches_without_inline_work_in_production_mode() {
        telemetry_cleanup();
        let _lock = force_production();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        // Even if the LLM would have a response ready, the parent must
        // not call it — production fork path doesn't touch the mock.
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 200;
        let bead = "b_prod_1";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let outcome = maybe_run(&deps, bead, pr, 8_000_000).unwrap();

        // Parent returns Dispatched with the reserved counter slot.
        assert_eq!(
            outcome,
            Outcome::Dispatched { count: 1 },
            "production parent must return Dispatched (not Posted) so the tick stays no-op"
        );
        // Parent did NOT spawn the reviewer inline.
        assert!(
            llm.calls.borrow().is_empty(),
            "production parent must NOT call llm.judge (that's the child's job); got calls: {:?}",
            llm.calls.borrow()
        );
        // Parent did NOT post a comment inline.
        assert!(
            tracker.comment_calls.borrow().is_empty(),
            "production parent must NOT post comment_external inline; got calls: {:?}",
            tracker.comment_calls.borrow()
        );
        // Counter was still incremented by the parent (reservation).
        let (count, last_at) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(count, 1, "parent reserved the slot even in production mode");
        assert_eq!(last_at, Some(8_000_000));
        reset_inline();
    }

    /// Production mode parent must return FAST — it must NOT block on
    /// a 60s `claude --print` review. We simulate a slow reviewer
    /// indirectly: even if the LLM were real (slow), the parent must
    /// not touch it. The test asserts wallclock is far below any
    /// realistic reviewer spawn latency.
    #[test]
    fn production_parent_is_fast_even_if_reviewer_would_block() {
        telemetry_cleanup();
        let _lock = force_production();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L { real: true, ..Default::default() };
        // Real LLM means the parent's `if !deps.llm.is_real()` branch
        // would normally call `spawn_reviewer` (60s+ wallclock). The
        // refactor removes that branch from the parent entirely.
        let store = St::default();
        let cfg = test_cfg();

        let pr = 201;
        let bead = "b_prod_2";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let t0 = std::time::Instant::now();
        let outcome = maybe_run(&deps, bead, pr, 9_000_000).unwrap();
        let elapsed = t0.elapsed();

        assert_eq!(outcome, Outcome::Dispatched { count: 1 });
        // 2 seconds is generous — pre-bpb6 the parent blocked 60s+.
        // Anything well under the reviewer's 60s budget proves the
        // async refactor worked.
        assert!(
            elapsed.as_secs() < 2,
            "parent must return fast (under 2s); took {elapsed:?} — \
             this means the synchronous reviewer spawn leaked back \
             into the parent path"
        );
        assert!(
            llm.calls.borrow().is_empty(),
            "parent must not touch real LLM (child's job)"
        );
        reset_inline();
    }

    /// Two consecutive `maybe_run` calls in production mode each
    /// reserve a slot — the counter increments monotonically. The
    /// cooldown gate prevents a third call within the window.
    #[test]
    fn production_parent_increments_counter_on_each_dispatch() {
        telemetry_cleanup();
        let _lock = force_production();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        let store = St::default();
        let cfg = test_cfg();

        let pr = 202;
        let bead = "b_prod_3";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let t0 = 10_000_000;
        let first = maybe_run(&deps, bead, pr, t0).unwrap();
        assert_eq!(first, Outcome::Dispatched { count: 1 });
        let (c, _) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(c, 1);

        // After cooldown elapses, dispatch again — count goes to 2.
        let t1 = t0 + ER_RUNNER_COOLDOWN_SECS + 10;
        let second = maybe_run(&deps, bead, pr, t1).unwrap();
        assert_eq!(second, Outcome::Dispatched { count: 2 });
        let (c, _) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(c, 2);
        reset_inline();
    }

    /// `child_run_er_inline` must NOT increment the counter — the
    /// parent reserved the slot, the child only does spawn + post +
    /// parse. This protects against accidental double-counting if a
    /// future maintainer adds `incr_er_runner_attempt` back into the
    /// child path thinking it was missed.
    #[test]
    fn child_run_er_inline_does_not_re_increment_counter() {
        telemetry_cleanup();
        let _lock = force_inline();
        let scm = S::default();
        let tracker = T::default();
        let sessions = Ss;
        let llm = L::default();
        *llm.response.borrow_mut() = Some(Ok("/er PASS".into()));
        let store = St::default();
        let cfg = test_cfg();

        let pr = 203;
        let bead = "b_child_1";
        store
            .overlays
            .borrow_mut()
            .insert(bead.into(), attested_overlay(bead, pr));
        scm.snapshots
            .borrow_mut()
            .insert(pr, snapshot_with_comments(pr, vec![]));

        // Pre-set the counter to a known value, simulating a parent
        // that already reserved slot N.
        let reserved_count = 7;
        store
            .er_counts
            .borrow_mut()
            .insert(bead.into(), (reserved_count, Some(1_000_000)));

        let deps = make_deps(&scm, &tracker, &sessions, &llm, &store, &cfg);
        let verdict = child_run_er_inline(&deps, bead, pr, 1_000_000);
        assert_eq!(verdict, ErVerdict::Pass, "child must parse verdict");

        // Child did NOT change the counter.
        let (count, _) = store.er_runner_attempt(bead).unwrap();
        assert_eq!(
            count, reserved_count,
            "child must not re-increment; parent is the single writer"
        );
        // Child DID spawn the reviewer and post the comment.
        assert_eq!(llm.calls.borrow().len(), 1);
        assert_eq!(tracker.comment_calls.borrow().len(), 1);
        reset_inline();
    }
}
