## Summary

<!-- What does this PR do? 1-3 sentences. Reference the .dot file(s) or runner code touched. -->

## Beads

<!-- REQUIRED: list the bead IDs this PR closes, fixes, or refs.
     Format: `Beads: <id>` (one line, comma-separated if multiple).
     For jleechanorg/dark-factory the prefix is `jleechan-` (e.g. jleechan-0qy, jleechan-n3m).

     Examples:
       Beads: jleechan-0qy
       Beads: jleechan-0qy, jleechan-n3m
       Beads: none   (explicit opt-out if no bead applies)

     Run `br list --status open --json` to find open beads. Open a bead FIRST
     via `br create "<title>" --type task|bug|chore --priority 0..4` so this PR
     has something to close; do not silently drop discovered work.
-->

Beads: jleechan-xxxx

## Tenets

<!-- Guiding principles this PR upholds. Common dark-factory tenets:
     - `.dot` files are the durable artifact; runner code is dorodango.
     - Holdouts stay sealed; implementing agents never see holdout paths or content.
     - Every code-producing graph routes reviewer `outcome!=success` to a bounded `fix` loop.
     - Merge confidence comes from outcome artifacts, not human code review.
     Tie back to the governing design doc or `specs/<feature>.md`. If none apply, write `N/A`. -->

- N/A

## Test plan

<!-- How was this tested?
     - Pipeline smoke: `dark-factory --pipeline <dot> --goal "<goal>" --backend echo`
     - Conformance: `bin/conformance validate`
     - Unit / integration: `.venv/bin/python -m pytest tests/`
     - /er review (evidence), /es (evidence-standards), or /code_standards output
     - Holdout result (never paste holdout content; cite `run_id` + verdict line) -->

## Risk

<!-- What could break?
     - Pipeline-graph regressions (which .dot files reference this code?)
     - CXDB schema changes (existing CXDB consumers break?)
     - Holdout-isolation boundary (does any new code leak holdout paths?)
     - WIP pollution on shared branches (factory's WIP-exhausted commits)
     Blast radius + rollback plan. -->

## Out of scope

<!-- What this PR intentionally does NOT do.
     E.g. "Does not add a new node type" / "Does not change the conformance contract".
     Helps reviewers focus and prevents scope creep. -->
