---
description: Build a developer repro bundle with raw agent transcripts and repo evidence.
type: evidence
execution_mode: immediate
---

## Execution Instructions

Load and execute the repo-local `repro-developer` skill:

```
Skill("repro-developer", args="$ARGUMENTS")
```

Use this when an Anthropic/OpenAI engineer needs a faithful reproduction bundle with raw Claude/Codex JSONL transcripts, subagent state, repo evidence, hashes, and a replay guide.

If the user asks about the July 6-7, 2026 Claude/fable vs Codex plan-miss incident, use the committed LFS artifact unless they explicitly request a fresh collection:

```
artifacts/repro-developer/claude-fable-adversarial-review-codex-plan-miss/
```

For a fresh collection, run `.claude/skills/repro-developer/scripts/collect_repro.py` with `--sanitize --require-gitleaks-clean --encrypt-raw --publish-dir ...`, verify LFS pointers with `git lfs status`, and never commit the raw passphrase.
