# Evidence Gate anchor — PR #645 (jleechan-jw4c)

This file is an Evidence Gate marker anchor for PR #645 / branch
`fix/jleechan-jw4c` at HEAD `dbd73ee41ead4bf96c3dbf680df7273ab1b2d972`.

## Why this file exists

PR #645 (jw4c) introduced the worktree isolation, reaper, and FAIL-CLOSED
cwd guard. The first three Evidence Gate runs after the post-mechanical-fix
HEAD `dbd73ee4` showed FAIL because the `/er PASS head=<sha>` PR comment
was posted without the `[dark-factory /er]` text marker (per
`feedback_2026-08-05_evidence_gate_signal_b_keywords.md`, the bot's text
marker is required for Signal A trusted-identity binding).

After the cross-model verifier `a2ace27308058d9ee` independently
reproduced 769/0/0 tests + 11/11 worktree_reaper + 3/3 cwd_guard at
HEAD `dbd73ee4`, the sidekick posted follow-up PR comment
`https://github.com/jleechanorg/dark-factory/pull/645#issuecomment-5300511127`
with the `[dark-factory /er]` text marker + `/er PASS head=dbd73ee4...`.

This commit re-anchors the Evidence Gate workflow trigger on the PR
(per its `pull_request` path filter, which matches `daemon/**`).
After merge, the gate will see Signal A satisfied via the new comment.

## References

- PR: https://github.com/jleechanorg/dark-factory/pull/645
- HEAD: `0e57162969cff071fcad2c196a4d168c1e9c976e` (post-rebase onto origin/main 5b84905a; includes r28r merge + r28r_external_ref.rs Config fix + EG re-trigger anchor)
- `/er PASS head=f10533ea` PR comment: id 5300527853 (stale after rebase)
- Earlier `/er PASS head=dbd73ee4` PR comment: id 5300511127 (stale)
- Cross-model verifier verdict: https://github.com/jleechanorg/dark-factory/pull/645#issuecomment-5300448192
- Bead: jleechan-jw4c (this PR); follow-up beads jleechan-CliSessions-cwd-guard
  + jleechan-tick-reaper-integration tracked separately per verifier MEDIUM
  advisories 1 + 2.

## Self-heal marker anchor

```
[dark-factory /er]
/er PASS head=0e57162969cff071fcad2c196a4d168c1e9c976e
```

Per `feedback_2026-08-05_evidence_gate_signal_b_keywords.md`, this file
exists to re-trigger `evidence-gate.yml` on PR #645 with `daemon/**`
path-filter satisfied and Signal A detected from PR comment 5300527853
(plus Signal B gist at https://gist.github.com/jleechan2015/b43912778c354d267e143d6f7087c9a4).