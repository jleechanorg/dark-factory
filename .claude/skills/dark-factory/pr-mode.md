<!-- On-demand detail for .claude/skills/dark-factory/SKILL.md (/f). -->

# `/f-pr` — explicit PR-mode entry point

`/f-pr` skips Step 0a and always runs PR-mode. Use the LLM to **read the
PR's actual context and pick the right pipeline** — this is a reasoning
task, not a deterministic rule table (no `if is_draft then X`, no
`if labels contains 'bug' then bug_fix.dot`, no keyword-routing).

1. **Resolve the PR number**: from `$ARGUMENTS` if present, else
   `gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" --json number --jq '.[0].number'`,
   else ask the user.
2. **Read PR context as facts, not routing rules**:
   ```bash
   gh pr view <N> --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,body,files,labels
   gh pr view <N> --json statusCheckRollup
   gh pr diff <N> --name-only
   gh pr view <N> --comments   # optional
   ls specs/ roadmap*/ docs/design/ 2>/dev/null   # optional
   ```
3. **Reason about**: what kind of work this is (new feature / bug fix /
   refactor / docs / test-only / infra — from diff+body+files, never
   pre-bucketed by label alone); whether a spec already covers the
   change (if not, see SKILL.md **Honesty rules** on `/fs`); holdout eligibility
   (see SKILL.md **Honesty rules** on `--feature`); and what evidence mix (`/es` + `/er` +
   `/code_standards` minimum, `holdout_eval` for behavior-grade) the
   pipeline needs to deliver without over-running.
4. Execute only after needed preparation is complete and a pipeline
   actually fits: pick the pipeline from SKILL.md **Pipelines**
   using that reasoning, pick the backend (`echo` for wiring smoke;
   `claude` unless the PR's reviewer queue or `gate_er` priority queue
   says otherwise), then construct, show, and run the command — same
   shape as SKILL.md Step 2, run from the PR's target repo.
5. Report the outcome per SKILL.md **Output contract**.

`/f-pr` honesty rules (in addition to the shared ones under SKILL.md **Honesty
rules**): the `gate_er` priority queue (default `codex > minimax > agy >
claude-sonnet`; a node's `backend_priority` attribute, then
`DARK_FACTORY_ADVERSARIAL_PRIORITY`, overrides it) still resolves the reviewer even when `--backend claude`
was passed for the run itself — passing `--backend claude` does not mean
`gate_er` uses claude by default.
