# Success Criteria — /af E2E proof

Strict mode: all criteria require concrete, independently-verifiable evidence.

## C1 — PR #190 merged fully green (jleechan-sniw.1) — DONE 2026-07-07T~21:12
- [x] Merge/close state checked FIRST (`gh api pulls/190 --jq '{state,merged}'`)
- [x] All checks SUCCESS at final head SHA (test, daemon-tests, Evidence Gate,
      skeptic, notify, CodeRabbit); Bugbot skip documented
- [x] No unresolved inline review threads (0 confirmed via GraphQL before merge)
- [x] Squashed to single commit before merge (merge_commit ed52665 has 1 parent, verified via `git log --format=%P`)
- [x] PR #190 shows `merged: true` (merge_commit_sha ed5266582802c7f3f8ca493f10e2d2412beebdd6)
Closed jleechan-sniw.1 with evidence. Full 3-pass adversarial verdict chain in
/tmp/dark-factory/sidekick/af-e2e/STATE.md.

## C2 — Live /af labeled-PR E2E proven (jleechan-sniw.2) — SUBSTANTIALLY PROVEN, not fully closed
- [x] Daemon running as durable service with live `systemctl --user status` (active/running) evidence
- [x] ≥2 watchdog-fed tick intervals visible (9+ consecutive tick attempts logged; daemon has run
      continuously since 2026-07-07T15:53 PDT install)
- [x] A real factory-labeled canary issue (#8227, jleechanorg/worldarchitect.ai) adopted by daemon
      intake -> bead jleechan-vj89 created, confirmed via bead_overlay sqlite row + `br show`
- [x] AO worker sessions dispatched by the daemon (not operator command) — 15+ beads reached DISPATCHED
      state with real AO session_ids (wa-2986+) and real branches; daemon observed running real
      `skeptic verify --pr 8075/8050` + multiple live `codex exec` sessions against worldarchitect.ai
- [~] Gates run and recorded — inferred from the skeptic-verify processes observed running, not yet
      directly confirmed per-bead via telemetry/GitHub checks
- [ ] PR reaches READY/merge state without operator coding intervention — NOT yet directly confirmed
      for any specific bead (queue was still draining, my own canary jleechan-vj89 still QUEUED behind
      a backlog as of last check)
- [x] Evidence bundle written under /tmp/dark-factory/sidekick/af-e2e/evidence/ + full narrative in
      STATE.md; [ ] independent skeptic/evidence-review pass on the bundle NOT yet done
Along the way, found + fixed 2 real production bugs live: jleechan-u4gb (intake dedup TOCTOU race,
fixed via PR #192, independently adversarially verified PASS) and an AO-not-running operational gap
(worked around live via `ao start`, no code fix filed yet). This is strong, honest partial proof —
recommend keeping jleechan-sniw.2 open until at least one specific bead is directly observed reaching
READY/gates-recorded state.

## C3 — Sessions::attach remediation (jleechan-tfs1, P1) — RE-SCOPED, decision pending
- [ ] Adopted PRs with an existing branch get a real Sessions::attach remediation path (code + test)
      NOT YET IMPLEMENTED. Bead re-scoped with explicit reason: implementation was blocked on (a) PR
      #190 merging first (now done) and (b) an explicit behavioral decision on remediation strategy.
      Two concrete options (in-place remediation on existing branch vs. new-branch-without-closing-PR)
      drafted and sent to main/operator for decision; not started pending that call — this is a genuine
      infra-spec decision per this repo's own "confirm before implementing" rule, not an oversight.

## C4 — Remaining open PR reconciliation — DONE, all 7 accounted for
- [x] #163: 3-round adversarial saga (gate-7 deadlock found + fixed + residual instance found + fixed
      again), 3rd independent pass VERDICT PASS on head f982dfd — merge-ready, recommended to main.
- [x] #164: rebased clean, all checks green, flagged merge-ready to human by main.
- [x] #165: not present in open PR list at any point this session (resolved before this session started).
- [x] #172: MERGED (installer relocation + 7 bug fixes, light adversarial pass PASS).
- [x] #173: rebased clean, all checks green, flagged merge-ready to human by main.
- [x] #174: MERGED (structured exit codes + ZFC hardcoded-ID removal; independently verified — the
      scoped reviewer's 2 "unresolved" findings were actually a false positive on direct code inspection).
- [x] #179: CLOSED as superseded by human (jleechan2015), matching the sidekick's evidence-backed
      recommendation (shell READY-writer was a genuine correctness regression vs. verifier.rs's
      stricter all_green contract, not just redundant).
- [x] No open PR left in CONFLICTING/dirty state without a rebase bead or active fix (#163 fixed
      in-place; #192 is new, opened clean).

## C5 — Method fidelity + tracking
- [x] /sidekick orchestrator spawned and owns the mission durably (this session; respawned once after
      a /tmp sweep destroyed the first STATE.md, reconstructed from transcript, continued without
      losing mission context).
- [x] Adversarial verification fan-out used extensively throughout (codex-pair-verifier/coder agents
      dispatched for every PR decision, PR #190's 3-pass chain, PR #163's 3-round saga, PR #192's
      review) — functionally the /swarm pattern, though not invoked as the literal named skill.
- [x] Beads updated at each transition (jleechan-sniw.1 closed with evidence, jleechan-u4gb/jleechan-uinw/
      jleechan-f4id/jleechan-7q88 filed with full repro+acceptance criteria, jleechan-lkq1 logged+closed
      for PR163 round-3 traceability, jleechan-fk9q annotated with repro notes).
- [ ] Roadmap activity + nextsteps update — NOT yet done, still pending at end of this work block.
