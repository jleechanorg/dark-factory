# Goal: /af factory truly proven working end-to-end

**Defined**: 2026-07-07 19:15
**Method requirement (user-specified)**: iterate using `/sidekick` (persistent orchestrator) and `/swarm` (multi-agent fan-out with adversarial verification). Method fidelity applies — do not silently substitute.

## Goal statement

Iterate until all relevant PR work is done and the dark-factory /af path is
truly proven working end-to-end: a factory-labeled PR/bead flows through
daemon intake → AO worker → gates → READY/merge **without operator coding
intervention**, with independent evidence.

## Work items (from operator handoff)

| Item | Bead | Priority |
|---|---|---|
| Finish PR #190 gate audit; merge only if fully green | jleechan-sniw.1 | P0 |
| Prove live /af labeled-PR E2E after #190 lands | jleechan-sniw.2 | P0 |
| Existing-branch Sessions::attach remediation for adopted PRs | jleechan-tfs1 | P1 |
| Reconcile remaining open PRs (#163, #164, #165, #172, #173, #174, #179) | jleechan-seey/ebe1/v2wv etc. | P1 |

## Resume state

- PR #190: https://github.com/jleechanorg/dark-factory/pull/190
- Head: 3b4b4e0fcfd69f0c0e560a757e7f72f2ad6f3d0c
- Last verified (19:10): test, daemon-tests, Evidence Gate, skeptic, notify,
  CodeRabbit all SUCCESS; Cursor Bugbot NEUTRAL (usage-limit skip);
  reviewDecision APPROVED; MERGEABLE.

## Hard constraints (from memory)

- jleechan-1m4 safety gate: `Restart=always` HARD-BLOCKED; WatchdogSec is
  liveness-only, not tick isolation.
- Gate self-certification anti-pattern: E2E proof must come from independent
  ground truth (journal logs, GitHub state, live systemctl output), never
  from the factory's own claims.
- PR #188 was blocker progress, NOT E2E proof — do not repeat the overclaim.
- Merge discipline: squash before final merge; check merged/closed state
  FIRST on every PR status check.
- Local main checkout is behind origin and dirty — reconcile before local
  daemon work; do not clobber uncommitted tick.rs recovery changes silently.
