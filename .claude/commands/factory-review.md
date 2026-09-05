---
description: "/factory-review — review-only Dark Factory entry point. NO coding node: a fresh, static-prompt Codex reviewer evaluates a target you already changed in ANY repo, and returns a verdict + findings. Never writes/commits/pushes to the reviewed workspace."
type: skill
execution_mode: immediate
aliases: [fr]
---

# /factory-review — Review-Only Dark Factory Entry Point

Single writer for the `/factory-review`/`/fr` contract. Read
`.claude/skills/factory-review/SKILL.md` (this repo) and execute it as the
source of truth.

Unlike `/f`/`/factory` (worker + fresh reviewer — codes AND reviews),
`/factory-review` has **no coding node at all**. It exists so a calling LLM
session that already wrote its own code — in dark-factory, or in any other
repo — can get one fresh, adversarial review pass without dark-factory
touching, committing, or pushing anything in the reviewed workspace.

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via
`~/.local/bin`.

## Usage

```
/factory-review [--target <locator-or-freeform>] [--target-intent "<what changed>"] [--workdir <path>]
```

- `--target` — what to review: a `factory.review-target.v1` locator
  (`git-range://…@base..head`, `git-commit://…@sha`, `gh-pr://owner/repo/N`,
  `file://…@sha256:…`) or freeform text (a bare SHA, `PR 811`, an absolute
  path) resolved mechanically before the run starts. Defaults to the calling
  repo's own committed diff against its upstream default branch when
  omitted (see SKILL.md Step 1).
- `--target-intent` — free text describing what you changed and want
  reviewed. Optional; without it the reviewer sees the default placeholder
  intent text.
- `--workdir` — the repo being reviewed. Defaults to cwd. This is **not**
  dark-factory's own working tree unless you are reviewing dark-factory
  itself.

## See also

- `/fr` — short alias, identical behavior.
- `/f` / `/factory` — full worker+reviewer loop (codes AND reviews); use
  `/factory-review` instead when you already wrote the code yourself and
  only want the adversarial review pass.
- `.claude/skills/factory-review/SKILL.md` — full contract: target
  resolution, the `pipelines/slim/review_only.dot` graph, and how to parse
  the returned verdict + findings.
