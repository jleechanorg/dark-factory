---
name: factory-lite-verifier
description: factory-lite verifier tick — assess 7-green gates, dispatch adversarial Skeptic review, record re-roll verdicts; honors auto-factory daemon contracts (read-only on branches, never merges)
---

# /factory-lite-verifier — one verification tick

An LLM-skill bootstrap of the Auto-Factory Daemon's SCM Observer & Verification
Loop (`docs/auto-factory-daemon-design-rust.md` `verifier.rs`), built to run
BEFORE the Rust daemon exists, against the exact same
`~/.dark-factory/daemon-cxdb.sqlite` and telemetry contract.

Designed to run under `/loop 5m /factory-lite-verifier`. **One invocation =
one tick.** Assume zero conversation context.

## 0. Load contract + config

Read `.claude/skills/factory-lite/CONTRACT.md` in full first. Load config per
CONTRACT.md §0. Confirm `stage=1` before proceeding — this skill's re-roll
handling (step 4) assumes Stage 1 (record, never execute) and will refuse to
run the Re-Roll Engine even if `stage=2` is set (that engine does not exist in
these LLM skills; report the mismatch instead of improvising).

## 1. Fetch PR snapshot for every ATTESTED bead

```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite \
  "SELECT bead_id, pr_number, branch, attempt FROM bead_overlay WHERE state='ATTESTED';"
```

For each `pr_number`, gather:
```bash
gh pr view "$PR" --repo "$TARGET_REPO" \
  --json number,mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,reviews,comments
gh pr checks "$PR" --repo "$TARGET_REPO" --json name,state,conclusion
```
For thread resolution (GraphQL `isResolved`, not REST comment count — REST
undercounts):
```bash
gh api graphql -f query='
  query($owner:String!,$repo:String!,$pr:Int!){
    repository(owner:$owner,name:$repo){
      pullRequest(number:$pr){
        reviewThreads(first:100){ nodes{ isResolved } }
      }
    }
  }' -f owner="${TARGET_REPO%%/*}" -f repo="${TARGET_REPO##*/}" -F pr="$PR"
```

## 2. Assess all 7 gates (spec §4.2.5)

| # | Gate | Source |
|---|---|---|
| 1 | CI Green | every `statusCheckRollup`/`gh pr checks` entry has `conclusion=success` |
| 2 | No Conflicts | `mergeable=MERGEABLE` / `mergeStateStatus` not `DIRTY`/`CONFLICTING` |
| 3 | CodeRabbit APPROVED | latest review by `coderabbitai[bot]` has `state=APPROVED` |
| 4 | Bugbot Clean | zero error-severity comments from `cursor[bot]` in `reviews`/`comments` |
| 5 | Comments Resolved | every `reviewThreads` node has `isResolved=true` |
| 6 | Evidence Review (`/er`) | most recent `/er`-triggered comment/check reports `PASS` |
| 7 | Skeptic PASS | **dispatch a minimax reviewer** (below) |

**Gate 7 — Skeptic:** spawn via the Agent tool, `subagent_type:
"minimax-pair-verifier"`. Prompt it to adversarially review the PR's diff
(`gh pr diff "$PR"`) independently of any in-repo reviewer nodes, and return a
verdict using exactly this grammar: `pass` | `warn` | `fail` (map `warn` to a
non-blocking gate — record it, don't treat as red). Do not accept free text as
the verdict; if the subagent's output doesn't parse to one of the three
tokens, treat gate 7 as `Unknown` (infra failure), not `Red` (real failure) —
these are different failure classes, don't conflate them.

Each gate result is one of `Green` / `Red(reason)` / `Unknown(reason)` —
`Unknown` means "couldn't determine" (API error, timeout), `Red` means
"determined and failing." Never collapse `Unknown` into `Red` or `Green`.

Emit **one** `GATE_ASSESSMENT` event per PR per tick:
```
context = {
  "pr_number": N,
  "gates": {
    "ci_green": "green|red|unknown", "no_conflicts": "...", "coderabbit": "...",
    "bugbot": "...", "comments_resolved": "...", "evidence_review": "...", "skeptic": "..."
  },
  "all_green": true|false
}
```

## 3. All green → readiness, then STOP

If `all_green=true`:
```bash
gh pr comment "$PR" --repo "$TARGET_REPO" --body "$(cat <<'EOF'
## factory-lite readiness summary
All 7 gates green (CI, no conflicts, CodeRabbit, Bugbot, comments resolved,
evidence review, Skeptic). Ready for human merge review.
EOF
)"
```
Emit `READY_FOR_MERGE`. Do **not** change `bead_overlay.state` further this
tick beyond what step 5 (autonomy check) requires, and do **not** merge —
merge authority is never delegated to this skill (CONTRACT.md §5.2).

## 4. CHANGES_REQUESTED → cooldown, then model verdict

If any gate is `Red` (i.e. `reviewDecision=CHANGES_REQUESTED` or an
equivalent failing signal), apply the spec's one-tick cooldown before acting:
check whether the **previous** `GATE_ASSESSMENT` for this `pr_number` in
`~/Library/Logs/dark-factory/daemon.jsonl` (grep for
`"event_type":"GATE_ASSESSMENT"` + this `pr_number`, take the second-to-last
match) already showed the same red gate. If this is the FIRST tick observing
the failure, stop here (no verdict yet — that's the cooldown). If it was
ALSO red last tick, proceed:

Use **your own judgment as the LLM** — read the actual review comments — to
decide `in_place_fixable` vs `reroll_worthy`. This is a model judgment call
(ZFC): never pattern-match on comment text keywords. In-place-fixable means
the owning coder session's native remediation loop can address it without a
branch reset; re-roll-worthy means the PR's foundational approach is wrong.

Emit `REROLL_VERDICT_RECORDED`: `context={"verdict":"in_place_fixable"|"reroll_worthy","rationale":"..."}`.

- **`in_place_fixable`:** stop here. Do not change `bead_overlay.state` — the
  owning coder session continues in-place. This is exactly what the daemon
  spec calls "verdict recorded, session continues."
- **`reroll_worthy`:** because `stage=1` disables the Re-Roll Engine, park it:
  ```bash
  sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
    "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
     WHERE bead_id='<bead_id>';"
  ```
  Emit `PARKED_HUMAN_HELD`: `context={"reason":"reroll_worthy_stage1_disabled"}`.

## 5. Stalled/dead session or time-box exceeded

If `gh pr view` shows no activity for an implausibly long window, or a
dispatched coder subagent never returned, treat this the same as
re-roll-worthy in step 4 (park `HUMAN_HELD`, `context.reason="session_stalled"`).
Autonomy-clock parking itself (crossing `autonomy_timebox_secs`) is owned by
`factory-lite-coder` step 7 — this skill does not double-increment
`autonomy_secs`, it only reads the current value and defers to whatever state
the coder skill already set.

## 6. End-of-tick summary

Emit one `TICK` event: `metrics={"attested_checked":N,"ready_for_merge":N,"reroll_recorded":N,"human_held":N}`.

## NEVER

- NEVER push a commit, force-push, or amend the PR branch — this skill is
  strictly read-only on branches (CONTRACT.md §5.8).
- NEVER run `gh pr merge` or any merge-equivalent — terminal state is
  `READY_FOR_MERGE` + posted comment only.
- NEVER close a PR — old-PR closure is a Stage-2 Re-Roll Engine step, out of
  scope while `stage=1`.
- NEVER execute a re-roll (branch reset, spec mutation, re-dispatch) — record
  the verdict via `REROLL_VERDICT_RECORDED` and park `HUMAN_HELD` instead.
- NEVER collapse `Unknown` gate results into `Red` — an API timeout is not a
  failing review.
- NEVER keyword-match review text to decide in-place vs re-roll — model
  judgment only (ZFC).
- NEVER skip the one-tick cooldown before recording a re-roll verdict.
