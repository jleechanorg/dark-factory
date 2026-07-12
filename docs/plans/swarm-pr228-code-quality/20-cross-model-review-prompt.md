# Cross-Model Cold Review Prompt (rule 12, MANDATORY)

Dispatch to **codex** (`codex exec --yolo --skip-git-repo-check`) — NOT same-model Claude. Same-model review shares blind spots (proven twice: 2026-07-06 design-retro docset, 2026-07-10 PR #7888 follow-up).

## Mandatory review behavior

1. **Execute code, don't just read.** For each candidate finding, attempt to reproduce by running the relevant command or test. Report what was actually observed, not what the diff appears to imply.

2. **Check scope creep.** Confirm the finding's severity matches what the PR actually does. Do NOT credit the PR for solving problems it didn't claim to solve.

3. **Check causation, not correlation.** Distinguish "the diff includes X" from "the diff causes X." For symptom-patches (e.g. coercing non-str at render site, dual-format read-back), verify whether the upstream invariant is named and addressed or just papered over.

4. **Spot-check.** Independently reproduce the most concrete finding. Single confirmed spot-check is strong evidence for the rest.

## Inputs

- Lane A doc: `docs/plans/swarm-pr228-code-quality/01-lane-a-runner.md`
- Lane B doc: `docs/plans/swarm-pr228-code-quality/02-lane-b-tests.md`
- Lane C doc: `docs/plans/swarm-pr228-code-quality/03-lane-c-config-and-working-tree.md`
- pr228 commits: `2a383a1` (fix), `658a715` (test). Both visible via `git show <sha>`.
- Working-tree changes: visible via `git status --short` + `git diff` / `cat`.

## Output format

```
# Cross-Model Review — pr228 findings

## Spot-check (the most concrete finding, executed)
- Finding F<id> from <lane>: <title>
- Reproduction: <command> → <observed output>
- Verdict: CONFIRMED / REFUTED / INCONCLUSIVE
- Notes: ...

## Per-finding adversarial verdict
For each candidate finding:
- **F<id>** (lane X, severity Y): CONFIRMED / REFUTED / DOWNGRADED-TO-NIT
  - Evidence: <what codex observed>
  - Reason: <why this verdict>

## Scope-creep checks
- Findings that credit the PR for fixes it didn't make: <list>
- Findings that mis-attribute origin/main drift as pr228 deletions: <list>

## Final disposition
- Total candidates: N
- CONFIRMED after spot-check: M
- REFUTED: K
- DOWNGRADED: J
```

Dispatch from a fresh tmux session to avoid same-process contamination. Pin the model so we know what reviewed.