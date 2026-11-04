<!-- On-demand detail for .claude/skills/dark-factory/SKILL.md (/f). -->

# `/f` arguments

Every runner flag below appears in `dark-factory --help`; rows marked *skill
convention* are parsed by the skill and never passed to the runner. Whatever is
left after flag parsing is the goal description.

| Flag | Meaning | Skill convention |
|------|---------|------------------|
| `--pipeline <name>` | Graph to run | Short name from [pipelines.md](pipelines.md) (mapped to its path) or a `.dot` path; omit for `two_node` |
| `--goal <text>` | Goal text | Whatever is left after flag parsing; mutually exclusive with `--target` |
| `--target <locator>` / `--target-intent <text>` | Review a resolved target (`PR 811`, path, SHA) instead of a goal | Pins the resolved locator into run state; the run refuses to start if it does not resolve. The default `two_node` graph does not yet route reviewer-first on it |
| `--feature <name>` | Holdout feature for `holdout_eval` nodes | Holdout name; see SKILL.md **Honesty rules** |
| `--backend echo\|claude\|codex\|ao\|agy` | LLM backend for codergen nodes | Skill default: always passed explicitly (SKILL.md Step 0b). `echo` is a cost-free wiring smoke |
| `--cxdb <path>` | CXDB SQLite step log | Skill default: always pass `~/.dark-factory/cxdb.sqlite` so the Healer clusters across runs |
| `--max-steps <N>` | Step budget (default 100) | — |
| `--state KEY=VALUE` | Pre-seed `ctx.state`; repeatable | e.g. `--state slim.test_command="pytest tests/..."` in PR loops |
| `--preflight` | Validate the pipeline, print diagnostics, exit | — |
| `--resume <checkpoint>` | Resume from a checkpoint file | — |
| `--evidence-bundle <dir>` | Per-run evidence bundle (default `evidence/<run-id>`) | — |
| `--events <path>` | JSONL structured event log | — |
| `--workdir <dir>` | Implementation workdir (default cwd) | — |
| `--require-holdouts` | Fail fast when the feature has no `scenarios.yaml` | — |
| `--allow-live-pr-target` / `--allow-push` | Run on a branch with an open PR / let codergen nodes `git push` | Both default off; pass only when the task authorizes it |
| `--reviewer-calibration=true\|false` | — | *Skill convention*, default on (SKILL.md **Reviewer calibration**) |
