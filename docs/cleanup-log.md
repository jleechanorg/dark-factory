# Cleanup log — untracked items removed 2026-06-13

This file documents untracked working-tree items that were removed as part of
bead `jleechan-4gx` (dark-factory-cleanup-untracked). The items were untracked
(not in any commit, not in any bead's working set) and had no on-disk recovery
path. Removal was a hygiene action, not a code change.

## Removed

| Item | Size | Origin | Why removed |
|------|------|--------|-------------|
| `=` (file at repo root) | 0 B | Shell redirection artifact (`cmd > =` creates a literal file named `=`) | Empty file, no content, no purpose |
| `pipelines/prompts/slim/er-evidence-fix.md` | 2,760 B | L3 subagent's abandoned prompt work from the prior `/claw` round (paired with the now-removed `.dot` file) | Orphaned: the .dot file that referenced this prompt was also untracked; no bead scope owns this work |
| `pipelines/prompts/slim/record-video.md` | 3,217 B | L3 subagent's abandoned prompt work, same as above | Orphaned: same reason as er-evidence-fix.md |
| `pipelines/slim/er-evidence-fix.dot` | ~1 KB | L3 subagent's abandoned pipeline, references a `claudeaf` backend not in the standard set (`runner.cli` doesn't define it) | Orphaned: untracked, no bead scope |
| `pipelines/slim/er-video-pass.dot` | ~1 KB | L3 subagent's abandoned pipeline, same as above | Orphaned: same reason as er-evidence-fix.dot |

## Data loss acknowledgment

The two `.md` prompt files contained real LLM-authored content (5,977 bytes
total) that is now gone. The files were untracked so they were not in any
commit, not in any git reflog, and not recoverable through git operations.
No Time Machine snapshot was available in the cleanup window.

The content was:
- L3's interpretation of a "fix evidence ceremony" prompt
- L3's interpretation of a "record terminal video" prompt
- Both paired with `.dot` pipelines that referenced the non-standard
  `claudeaf` backend

If either pipeline is wanted in the future, the work will be re-derived from
the current best-practice spec rather than recovered from this lost content.
The cost of regeneration is bounded (each prompt is a few hundred tokens).

## Bead

- `jleechan-4gx` — dark-factory-cleanup-untracked (P3)
