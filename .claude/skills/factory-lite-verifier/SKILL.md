---
name: factory-lite-verifier
description: factory-lite verifier tick — assess 7-green gates with parallel minimax Skeptic reviews; all state mutations via the deterministic harness; read-only on branches, never merges
---

# /factory-lite-verifier — one verification tick

An LLM-skill bootstrap of the Auto-Factory Daemon's SCM Observer & Verification
Loop (`docs/auto-factory-daemon-design-rust.md` `verifier.rs`).

**Division of labor (ZFC, per /advice 2026-07-03):** every binding mutation
goes through the deterministic harness `daemon/factory-lite-harness.sh`
(`H` below). You supply ONLY typed judgment verdicts as harness arguments.
Never run sqlite3 or write the telemetry file directly; if the harness refuses,
respect the refusal.

Designed to run under `/loop 5m /factory-lite-verifier` or
`daemon/run-factory-lite.sh verifier`. **One invocation = one tick.** Assume
zero conversation context. Confirm `stage=1` in config before proceeding —
this skill records re-roll verdicts, it never executes re-rolls.

```bash
H=daemon/factory-lite-harness.sh
```

## 1. Fetch PR snapshot for every ATTESTED bead

`$H list ATTESTED` → for each `pr_number`:

```bash
gh pr view "$PR" --repo "$TARGET_REPO" \
  --json number,mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,reviews,comments
gh pr checks "$PR" --repo "$TARGET_REPO" --json name,state,conclusion
gh api graphql -f query='
  query($owner:String!,$repo:String!,$pr:Int!){
    repository(owner:$owner,name:$repo){
      pullRequest(number:$pr){ reviewThreads(first:100){ nodes{ isResolved } } } } }' \
  -f owner="${TARGET_REPO%%/*}" -f repo="${TARGET_REPO##*/}" -F pr="$PR"
```

(GraphQL `isResolved`, never REST comment counts.)

## 2. Assess all 7 gates (spec §4.2.5)

| # | Gate key | Green when |
|---|---|---|
| 1 | `ci_green` | every check run `conclusion=success` |
| 2 | `no_conflicts` | `mergeable=MERGEABLE`, mergeStateStatus not DIRTY/CONFLICTING |
| 3 | `coderabbit` | latest `coderabbitai[bot]` review `APPROVED` |
| 4 | `bugbot` | zero error-severity `cursor[bot]` comments |
| 5 | `comments_resolved` | every reviewThread `isResolved=true` |
| 6 | `evidence_review` | >100 non-test changed LOC requires integration evidence in PR body; ≤100 LOC or docs/tests-only passes |
| 7 | `skeptic` | parallel minimax adversarial review (below) |

**Gate 7 — Skeptic, PARALLEL:** if multiple ATTESTED PRs need review this
tick, spawn ALL their Skeptic reviewers in ONE message — one Agent tool call
per PR, `subagent_type: "minimax-pair-verifier"`, `run_in_background: false`
(you need the verdicts this tick; parallel spawn means wall-clock = slowest
single review, not the sum). Each prompt: adversarially review `gh pr diff <PR>`
against the bead's description; return verdict grammar `pass` | `warn` | `fail`
only. `warn` = non-blocking (record, not red). Unparseable output → gate 7
`unknown` (infra), NOT `red` (real failure) — never conflate the two.

Every gate value is `green` / `red` / `unknown` — `unknown` means "could not
determine" (API error, timeout). Never collapse `unknown` into `red` or `green`.

Record (one call per PR): `$H gate-assessment <bead_id> <pr> '<gates_json>'`
— the harness validates the 7 keys/values and prints `true|false` (all_green).

## 3. All green → readiness, then STOP

```bash
gh pr comment "$PR" --repo "$TARGET_REPO" --body "## factory-lite readiness summary
All 7 gates green (CI, no conflicts, CodeRabbit, Bugbot, comments resolved, evidence review, Skeptic). Ready for human merge review."
$H ready <bead_id> <pr>
```

Do NOT merge — merge authority is never delegated to this skill.

## 4. CHANGES_REQUESTED → cooldown, then model verdict

If any gate is `red`: check `$H prev-gate-assessment <pr>` — if empty or the
previous assessment did NOT show the same red gate, stop here (one-tick
cooldown, spec §4.2.6). If it was also red last tick:

Use **your own judgment as the LLM** — read the actual review comments — to
decide `in_place_fixable` (owning coder session can fix without branch reset)
vs `reroll_worthy` (foundational approach wrong). Model judgment, never
keyword matching. Then:

```bash
$H reroll-verdict <bead_id> <pr> <verdict> "<one-sentence rationale>"
```

The harness records the verdict; for `reroll_worthy` it also parks the bead
`HUMAN_HELD` (stage-1 substitution). `in_place_fixable` leaves the bead
ATTESTED — the owning coder keeps fixing in place.

## 5. Stalled session

A DISPATCHED/ATTESTED bead with no PR activity for an implausible window and
no responsive coder → `$H park <bead_id> "session_stalled"`. Autonomy-clock
increments are the coder tick's job — this skill never calls `autonomy-tick`.

## 6. End-of-tick summary

`$H tick-summary verifier`

## NEVER

- NEVER run sqlite3 against the CXDB or write the telemetry file directly —
  every mutation goes through `$H`.
- NEVER push, force-push, or amend a PR branch — strictly read-only.
- NEVER run `gh pr merge` — terminal state is the readiness comment.
- NEVER close a PR (Stage-2 action).
- NEVER execute a re-roll — record via `$H reroll-verdict`, which parks it.
- NEVER collapse `unknown` gates into `red`.
- NEVER keyword-match review text for the re-roll verdict (ZFC).
- NEVER skip the one-tick cooldown before a re-roll verdict.
- NEVER spawn Skeptic reviewers serially when several PRs await — parallel, one message.
