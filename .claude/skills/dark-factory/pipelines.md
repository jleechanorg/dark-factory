<!-- On-demand detail for .claude/skills/dark-factory/SKILL.md (/f). -->

# `/f` pipelines

The skill maps each short name to its path; the runner does not expand short
names. With no `--pipeline` the runner runs `two_node`.

| Short name | Path | Flow | When to use |
|------------|------|------|-------------|
| **`two_node`** ⭐ default | `pipelines/slim/two_node.dot` | worker → fresh Codex reviewer → exit | Default when no `--pipeline` is passed. The reviewer is a fresh fully tooled `codex exec --ephemeral --yolo` process in a detached worktree at the pin minted after the worker's last visit (agent-config files quarantined, never the live workdir); on failure only typed `{path, claim, required_fix}` findings reach the worker, never the raw response. |
| `two_node_holdout` | `pipelines/slim/two_node_holdout.dot` | worker → fresh Codex reviewer → holdout → exit | Two-node plus behavioral holdouts (opt-in) |
| `hello` | `pipelines/factory/hello.dot` | explore → plan → implement → holdout → fix-loop → exit | New feature with a holdout scenario; wiring smoke with `--backend echo` |
| `gates` | `pipelines/factory/gates.dot` | holdout → skeptic → adversarial reviewer → /es → /er → /code_standards → web advice → exit; failures loop fix → holdout | Validate an already-implemented diff (holdout defaults to `hello`; `--feature` overrides) |
| `pr_gates` | `pipelines/factory/pr_gates.dot` | same flow as `gates` | Validate an in-flight PR diff (holdout from `--feature <name>`) |
| `minimal_feature` | `pipelines/slim/minimal_feature.dot` | explore → plan → implement → test → review → holdout → /es → /er → exit; failures loop fix → test | Full production pipeline from scratch with a holdout |
| `minimal_pr` | `pipelines/slim/minimal_pr.dot` | explore → research → plan → implement → test → review → holdout → /es → /er → web advice → exit; failures loop fix → test | In-flight PR iteration with `--state slim.test_command=...` (requires `--feature <name>`) |
| `ready` | `pipelines/slim/ready.dot` | test → /es → /er → /advice → holdout → fix (max 3) | PR `/ready` enforcement + fix iteration |
| `bug_fix` | `pipelines/bug_fix.dot` | reproduce → red gate → fix → green gate → holdout → evidence | Bug fix with red/green discipline |
| `spec_gen` | `pipelines/slim/spec_gen.dot` | explore → plan_main → review_main (fix loop) → plan_attractor → review_attractor (fix loop) → exit | Reviewed main + attractor specs (the `/fs` pipeline) |
| `minimal_research` | `pipelines/slim/minimal_research.dot` | explore fanout → research → /er → exit; failures loop fix → research | Research report only |
| `review_slim` | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` | acceptance + codex reviewer | Spec review (slim) |
| `review_full` | `benchmarks/attractor-spec-review/pipelines/review_full.dot` | review_slim + fullstack smoke | Spec review with stack smoke |
| `level5_feature` | `pipelines/factory/level5_feature.dot` | full reference pipeline | Level-5 reference pipeline with hard-tier gates |
| **no pipeline** | — | — | No pipeline fits the inspected PR: see SKILL.md **Honesty rules**; do not force an inapplicable holdout pipeline |
