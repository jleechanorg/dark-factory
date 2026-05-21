---
description: "Dark Factory — the coding agent NEVER sees this repo. Holdouts and evaluator only."
type: quality
execution_mode: none
---

# Dark Factory Agent Policy

## CRITICAL: Agent Isolation

This repo contains **holdout scenarios** and an **evaluator** that the coding agent
must NEVER see during implementation. Violating this destroys the adversarial guarantee.

### What the implementing agent sees:
- `specs/<feature>.md` — the feature specification
- `prompts/` — prompt templates referenced by pipeline nodes
- `pipelines/` — pipeline graph definitions (optional, for context)

### What the implementing agent NEVER sees:
- `holdouts/` — blind evaluation scenarios
- `runner/evaluator.py` — the evaluator that runs holdouts
- `tests/` — any test infrastructure that reveals holdout structure

### Evaluator Agent (separate context):
- Sees `holdouts/` + `specs/` + the implementation diff
- Does NOT see the implementing agent's reasoning, context, or optimism
- Returns normalized verdicts: PASS / WARN / FAIL

## Pipeline Authoring Rules

1. `.dot` files are first-class artifacts — version them, review them in PRs
2. Node handlers are pluggable — new types via `handlers.py` registration
3. Backends are swappable — AO, Claude Code, Codex, Gemini CLI
4. Every pipeline must have a `start` and `exit` node
5. Condition expressions use simple `key=value` or `key!=value` syntax
