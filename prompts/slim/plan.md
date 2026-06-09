Create `spec.md` for the requested feature.

Goal:
${goal}

Read `.dark-factory/explore-findings.md` from the explore phase. The spec must
align with its authorities map and centralization proposal. If the explore
artifact is missing, stop and report that — do not invent a design from scratch.

Include:
- acceptance criteria
- non-goals
- implementation plan
- deterministic test command
- public behavioral expectations from the visible spec
- evidence expected before merge

Do not write hidden holdout scenarios or evaluator details into the spec. The
runner will execute sealed validation separately.
Do not implement yet.

**Hard requirement — lane independence (parallel or stacked work only):**
If the spec proposes parallel lanes, stacked PRs, or multiple concurrent
worktrees, it MUST include a **file-ownership matrix**: every file the work
will touch maps to exactly ONE owning lane/PR (single-writer rule). Any file
listed under two or more lanes forces a choice: serialize those lanes, or
restructure the split so each lane owns distinct files.

The spec must also state the overlap pre-flight commands used to verify
independence before spawning workers:

    git diff --name-only <base>...<branch>   # per lane
    # or pairwise:
    git merge-tree --write-tree <base> <branch-a> <branch-b>

If the work is single-lane, a one-line statement suffices:
"Single lane — no ownership matrix needed."

Background: a 6-PR stacked plan that placed the same new module in every PR
produced 7 divergent blobs and a fully serialized merge train (jleechan-hv1).
