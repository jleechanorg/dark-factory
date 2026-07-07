# Automated /er Runner — Design (jleechan-qqq)

> Companion to `docs/factory-goal-gap-review-2026-07-06.md` blocker #4.
> Closes the gap where Stage-1 daemon's gate 6 is permanently `Unknown`
> because `er_verdict = Absent` until a human posts `/er PASS`/`/er FAIL`.

## Goal

When a bead is in `ATTESTED` state with an open PR and no `/er` verdict
recorded in PR comments, the daemon MUST automatically spawn an independent
reviewer (a fresh subprocess invocation that is NOT the implementing agent's
session), capture the verdict, and post it back as a PR comment in the
format `parse_er_verdict` recognizes. The next tick then reads the comment
and gate 6 flips from `Absent` → `Pass` / `Fail` / `Partial` / `Inconclusive`.

## Non-goals

- Running the full evidence-standards 5-criterion rubric inline (the
  dispatched reviewer just emits a verdict line; the rubric still lives in
  the running reviewer's prompt).
- Replacing the existing parse_er_verdict gate logic.
- Posting anything other than a single verdict line per PR.
- Auto-merge (bead jleechan-s3c).

## Architecture

```
+---------------------+        +-----------------------+
|  run_fast_tier      |        |  /er runner           |
|  (attested bead)    |        |                       |
|                     |        |  1. check PR comments |
|  if er_verdict ==   | -----> |     skip if /er seen  |
|  Absent and no      |        |  2. spawn `claude      |
|  /er comment:       |        |     --print` fresh    |
|  invoke runner      |        |  3. capture reply     |
|                     |        |  4. post as PR comment|
+---------------------+        +-----------------------+
                                          |
                                          v
                                 PR comments now contain
                                 "/er PASS" (or FAIL/PARTIAL/INCONCLUSIVE)
                                          |
                                          v
                                 Next tick: parse_er_verdict -> Pass
                                 Gate 6 -> Green
```

## Independence guarantee

- The reviewer is a fresh subprocess: `claude --print --dangerously-skip-permissions`
  (or `codex exec --yolo --skip-git-repo-check`), independent process tree,
  no conversation memory of the implementing agent's session.
- The `Llm::is_real()` flag distinguishes mock (used in tests) vs. real
  (used in production), matching the existing skeptic_evidence gate
  (gate 7) — same discipline, no new pattern.
- Prompt explicitly forbids self-grading: "You are NOT the implementing
  agent; you are an independent reviewer. Read the PR diff + comments +
  body via `gh` CLI; do not trust claims without seeing the evidence."

## Idempotence

- Step 1 checks the current `PrSnapshot.comments` via `parse_er_verdict`.
  If it returns anything other than `Absent`, the runner is a no-op.
- A new field `last_er_runner_attempt_at: Option<u64>` (unix epoch secs)
  on `BeadOverlay` provides a cooldown (default 300s) so transient
  comment-fetch delays don't trigger duplicate reviewer spawns.
- `attempt_er_runner_count: u32` on `BeadOverlay` caps total retries at
  `MAX_ER_RUNNER_ATTEMPTS = 3`. After 3 failures the runner stops
  retrying; gate 6 stays `Unknown` and the bead parks `HUMAN_HELD` with
  a clear reason — same discipline as `recover-held` (max_attempt=10).

## Prompt contract

```
You are the /er (evidence review) gate for an autonomous coding factory.
You are NOT the implementing agent; you are an INDEPENDENT reviewer.

Review PR #<N> in repo <owner>/<name>:
  gh pr diff <N> --repo <repo>
  gh pr view <N> --repo <repo> --json body,comments
  gh pr checks <N> --repo <repo>

Decide whether the PR has REAL evidence (tests run, integration checks,
screenshots, videos) supporting its claims. Reply with EXACTLY ONE LINE,
no preamble, no markdown:

  /er PASS
or
  /er FAIL <one short reason>
or
  /er PARTIAL <one short reason>
or
  /er INCONCLUSIVE <one short reason>
```

## Data model additions

`daemon/src/state.rs::BeadOverlay`:
- `attempt_er_runner_count: u32` (default 0; clamped to MAX_ER_RUNNER_ATTEMPTS)
- `last_er_runner_attempt_at: Option<u64>` (unix epoch secs; cooldown gating)

`daemon/contracts/schema.sql`:
- ALTER bead_overlay ADD COLUMN attempt_er_runner_count INTEGER NOT NULL DEFAULT 0
- ALTER bead_overlay ADD COLUMN last_er_runner_attempt_at INTEGER

The existing StateStore trait gets two new methods with default impls
(no-op for fakes that don't override) so existing test fakes don't need
to change:
- `incr_er_runner_attempt(&self, bead_id: &str, now_epoch: u64) -> Result<u32, DaemonError>`
- `er_runner_attempt(&self, bead_id: &str) -> Result<(u32, Option<u64>), DaemonError>`

## Tick integration

In `run_fast_tier`, BEFORE the gate assessment (currently tick.rs:647-666),
add a single call:

```rust
let snapshot = deps.scm.pr_snapshot(pr)?;
if verifier::parse_er_verdict(&snapshot.comments) == ErVerdict::Absent {
    crate::er_runner::maybe_run(deps, bead_id, pr)?;
    // re-fetch so the just-posted comment is visible
    let snapshot = deps.scm.pr_snapshot(pr)?;
}
```

(Or fold into `skeptic_evidence` — see implementation TODO.)

The runner is a single function in a new `daemon/src/er_runner.rs`:

```rust
pub fn maybe_run(deps: &TickDeps, bead_id: &str, pr: u64) -> Result<Option<ErVerdict>, DaemonError> {
    let snapshot = deps.scm.pr_snapshot(pr)?;
    let existing = verifier::parse_er_verdict(&snapshot.comments);
    if existing != ErVerdict::Absent { return Ok(Some(existing)); }

    let overlay = match deps.store.load(bead_id)? { Some(o) => o, None => return Ok(None) };
    let (count, last_at) = deps.store.er_runner_attempt(bead_id).unwrap_or((0, None));
    let now = now_epoch();
    if count >= MAX_ER_RUNNER_ATTEMPTS { return Ok(None); } // capped
    if let Some(last) = last_at { if now.saturating_sub(last) < ER_RUNNER_COOLDOWN_SECS {
        return Ok(None); // cooldown
    }}

    let prompt = build_er_prompt(bead_id, pr, &deps.cfg.target_repo);
    let reply = if !deps.llm.is_real() {
        deps.llm.judge(&prompt)?
    } else {
        spawn_claude_reviewer(&prompt, ER_RUNNER_TIMEOUT_SECS)?
    };

    // post the verbatim reply as a PR comment
    let body = format!("🤖 **[dark-factory /er]** Evidence review verdict:\n\n```\n{reply}\n```");
    let ext_ref = format!("{}#{}", deps.cfg.target_repo, pr);
    let _ = deps.tracker.comment_external(&ext_ref, &body);

    deps.store.incr_er_runner_attempt(bead_id, now)?;

    let verdict = verifier::parse_er_verdict_text(&reply); // new helper
    Ok(Some(verdict))
}
```

## Telemetry events

- `ER_RUNNER_DISPATCHED` — fired once per spawned reviewer (counts and cooldowns tracked).
- `ER_RUNNER_NOOP` — fired when existing verdict or cooldown suppresses dispatch.
- `ER_RUNNER_CAPPED` — fired when `attempt_er_runner_count >= 3`.
- `ER_RUNNER_POSTED` — fired after the PR comment is posted (incl. verdict).

## ZFC compliance

- The reviewer prompt is a model-judgment call (claude/codex LLM). Verdict
  parsing uses the same `parse_er_verdict` regex-free grammar used
  elsewhere — no keyword routing added in daemon code.
- Cooldown / cap / idempotence are arithmetic, not judgment.
- The only "routing" decision is "should we spawn this tick?" — driven by
  pure state (existing verdict, cooldown elapsed, attempt cap), matching
  the existing `skeptic_evidence` "is_real() then subprocess" split.

## Test plan (TDD)

### Unit tests in `daemon/src/er_runner.rs`

1. `maybe_run_is_noop_when_pass_comment_already_present` — snapshot has
   a comment containing `/er PASS`; runner returns `Some(Pass)` without
   touching `Llm` or `Tracker`.
2. `maybe_run_is_noop_when_fail_comment_already_present` — same for `/er FAIL`.
3. `maybe_run_is_noop_within_cooldown_window` — second call within
   300s of the first does NOT spawn another reviewer.
4. `maybe_run_caps_at_max_attempts` — once
   `attempt_er_runner_count = MAX_ER_RUNNER_ATTEMPTS (3)`, runner stops
   spawning and emits `ER_RUNNER_CAPPED`.
5. `maybe_run_spawns_reviewer_when_no_verdict` — no `/er` in comments,
   fresh attempt; mock `Llm::judge` is called and its reply is captured.
6. `maybe_run_posts_reply_as_pr_comment` — captured reply is posted via
   `Tracker::comment_external`; PR-comment call log records it.
7. `maybe_run_increments_attempt_counter` — after spawn, the overlay's
   `attempt_er_runner_count` increases by 1 and `last_er_runner_attempt_at`
   is set to the current epoch.

### Integration tests in `daemon/tests/`

8. `gate_6_flips_pass_after_er_runner_posts_pass` — full tick:
   bead `ATTESTED`, no `/er` comment, mock LLM returns `/er PASS`,
   end-of-tick `evidence.er_verdict = Pass`, `GateResult::Green`.
9. `gate_6_flips_fail_after_er_runner_posts_fail` — same with
   `/er FAIL <reason>` → `GateResult::Red("/er verdict is FAIL")`.
10. `gate_6_stays_unknown_when_runner_capped` — overlay has 3 prior
    failed attempts; runner is a no-op; gate 6 stays `Unknown`; bead
    parks `HUMAN_HELD` with reason "evidence floor: /er runner capped".

### Real-adapter smoke (manual / CI marker)

11. (Manual, gated on `#[ignore]`) Run the daemon end-to-end against a
    fixture PR; confirm `/er PASS` or `/er FAIL` lands as a comment.

## Migration / rollback

- Additive: schema migration is two `ALTER TABLE ... DEFAULT 0` columns;
  no existing rows are invalidated.
- Idempotent: `parse_er_verdict` already gracefully handles `Absent`,
  so deploying without spawning the reviewer (e.g. feature-flag off) is
  a no-op regression to the pre-fix behavior — exactly today's
  "permanently Unknown" state, which is the documented failure mode.
- Rollback = revert the commit. No data loss.

## Open questions / follow-ups

- Should the reviewer post the rubric summary alongside the verdict?
  Current design: just the verdict line; rubric evidence lives in the
  reviewer's transcript (CXDB-style log) if/when we add it.
- Should we share a single reviewer session across multiple ATTESTED
  beads in the same tick? Current design: one reviewer per bead per
  cooldown window — simpler, but slower under burst load. Defer.