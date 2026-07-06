# Nextsteps — dark-factory — 2026-07-06

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Default Coder Alignment & git conflicts resolved:** The default coder settings were updated to use the Agent Orchestrator with the `antigravity` agent backend by default, and a fallback cascading chain was established to try `agy` first and then `claude`. Git main rebase conflicts were resolved and changes pushed cleanly to `origin/main`.
- **CodeRabbit status parser bug resolved:** Ignored non-decision `COMMENTED` reviews from CodeRabbit in the verifier (`daemon/src/adapters.rs`) and the `gates-compute` utility (`daemon/src/gates_compute.rs`). Now the verifier parses the actual `APPROVED` or `CHANGES_REQUESTED` state properly even if CodeRabbit leaves follow-up comments.
- **PR #7888 beads ID duplicate resolved:** Solved `beads-jsonl-validation` failures on `worldarchitect.ai` PR #7888 by removing the duplicate `rev-zzw3` bead ID from `.beads/issues.jsonl` in the PR worktree. Committed and pushed the fix to the remote branch, successfully passing local validation.
- **PR Merge Successes:** Five candidates listed in the previous roadmap have successfully merged or progressed:
  - `worldarchitect.ai` candidates PR #8058, PR #8116, PR #8064, and PR #8061 have successfully **MERGED**.
  - `dark-factory` PR #133 has **MERGED**.
  - `worldarchitect.ai` PR #7888 and PR #8060 remain **OPEN** and need to be driven through their remaining failing checks.

## Context

This work block (Session 2026-07-06) addressed git main conflicts, default coder configuration, and verifier/GHA failures blocking `worldarchitect.ai` PR #7888 from hitting green. The duplicate bead ID in the PR branch was resolved, and CodeRabbit comment states were filtered out of the daemon verifier's status check.

## Bead index

| Bead | Title | Priority | Link |
|------|-------|----------|------|
| [jleechan-93ft](https://github.com/jleechanorg/worldarchitect.ai/issues/20) | [worldai] Drive PR #7888 (cc-finish-level-commit) to /green — rebase + verifier pass | P1 | [jleechan-93ft](https://github.com/jleechanorg/worldarchitect.ai/issues/20) |
| [jleechan-sniw](https://github.com/jleechanorg/dark-factory/issues/138) | [daemon/intake] Sweep external PRs labeled factory and drive to /green | P1 | [jleechan-sniw](https://github.com/jleechanorg/dark-factory/issues/138) |
| [jleechan-7wud](https://github.com/jleechanorg/dark-factory/issues/139) | [daemon/router] ZFC: replace verdict enum with LLM-classified tier (no regex) | P1 | [jleechan-7wud](https://github.com/jleechanorg/dark-factory/issues/139) |
| [jleechan-x1bq](https://github.com/jleechanorg/dark-factory/issues/140) | [daemon/verifier] Wire tracker-style multi-vendor consensus | P1 | [jleechan-x1bq](https://github.com/jleechanorg/dark-factory/issues/140) |
| [jleechan-nmll](https://github.com/jleechanorg/dark-factory/issues/141) | [daemon] Replace operator-invoked scripts with daemon auto-tick | P1 | [jleechan-nmll](https://github.com/jleechanorg/dark-factory/issues/141) |
| [jleechan-3uiz](https://github.com/jleechanorg/dark-factory/issues/142) | [daemon] Mirror tracker Go consensus-task harness for /er | P2 | [jleechan-3uiz](https://github.com/jleechanorg/dark-factory/issues/142) |
| [jleechan-9xb2](https://github.com/jleechanorg/dark-factory/issues/143) | [daemon/router] LOCAL_PATH verdict for PR-less + direct-to-main work | P2 | [jleechan-9xb2](https://github.com/jleechanorg/dark-factory/issues/143) |
| [jleechan-j3ob](https://github.com/jleechanorg/dark-factory/issues/144) | [daemon/verifier] EVIDENCE_FLOOR by tier + zero-LOC exemptions | P2 | [jleechan-j3ob](https://github.com/jleechanorg/dark-factory/issues/144) |
| [jleechan-herp](https://github.com/jleechanorg/dark-factory/issues/145) | [daemon/dispatch] Replace host-shell ao with self-contained AOW worker spawning | P3 | [jleechan-herp](https://github.com/jleechanorg/dark-factory/issues/145) |
| [jleechan-2ka](https://github.com/jleechanorg/dark-factory/issues/146) | [daemon] Stage 2: reroll.rs + constraints.rs (re-roll writer plane) | P2 | [jleechan-2ka](https://github.com/jleechanorg/dark-factory/issues/146) |
| [jleechan-xrdx](https://github.com/jleechanorg/dark-factory/issues/147) | [daemon/decommission] decommission legacy loop | P2 | [jleechan-xrdx](https://github.com/jleechanorg/dark-factory/issues/147) |

## Work queue

1. **Drive PR #7888 to /green:** Address the remaining runner environmental failures on the `worldarchitect.ai` PR branch `fix/7887-cc-finish-level-commit` (missing `pip` on the self-hosted GHA runner and the `libavif16` APT package install failure). Tracks [jleechan-93ft](https://github.com/jleechanorg/worldarchitect.ai/issues/20).
2. **Drive PR #8060 to /green:** Check the GHA check runs for `worldarchitect.ai` PR #8060 (`fix/rewards-box-not-showing-8020-v2`) and fix any failures to get it green.
3. **ZFC Router Alignment:** Implement LLM-based ZFC tier classification in `daemon/src/router.rs` rather than string matching. Tracks [jleechan-7wud](https://github.com/jleechanorg/dark-factory/issues/139).
4. **Wire Multi-Vendor Consensus:** Implement parallel Opus+GPT+Gemini review consensus in `daemon/src/verifier.rs`. Tracks [jleechan-x1bq](https://github.com/jleechanorg/dark-factory/issues/140).
5. **Intake and Auto-Tick Automation:** Automate the PR labels sweep and auto-tick polling to remove human invocation steps. Tracks [jleechan-sniw](https://github.com/jleechanorg/dark-factory/issues/138) and [jleechan-nmll](https://github.com/jleechanorg/dark-factory/issues/141).

## PR / merge state

- https://github.com/jleechanorg/worldarchitect.ai/pull/7888 — OPEN (mergeable=true, unstable)
- https://github.com/jleechanorg/worldarchitect.ai/pull/8060 — OPEN (mergeable=true, unstable)
- https://github.com/jleechanorg/worldarchitect.ai/pull/8058 — MERGED
- https://github.com/jleechanorg/worldarchitect.ai/pull/8116 — MERGED
- https://github.com/jleechanorg/worldarchitect.ai/pull/8064 — MERGED
- https://github.com/jleechanorg/worldarchitect.ai/pull/8061 — MERGED
- https://github.com/jleechanorg/dark-factory/pull/133 — MERGED
- https://github.com/jleechanorg/dark-factory/pull/161 — CLOSED

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — section `2026-07-06 — verifier CodeRabbit COMMENTED review parser fix + beads duplicate ID`

## Roadmap pointer

- Appended `roadmap/activity/2026-07-06.md` — Recent activity (per-day file)
