# Diagram Color Semantics

Every color in a dark-factory diagram carries a **semantic meaning**. Color is not decoration — it is the *third axis* of a node's identity alongside its name and its shape. A reader should be able to tell, at a glance, what role a node plays in the system **without reading its label**.

This doc is the canonical reference for all dark-factory diagrams: Graphviz `.dot` pipeline previews (`dot -Tpng ...`), Excalidraw diagrams in the README, the spec-validation benchmark outputs, and any future renderer. **The `.dot` colors and the Excalidraw colors must agree** — they are defined as a single vocabulary, split by medium, and the table at the bottom is the binding contract.

---

## The three layers

A dark-factory diagram is a story about three things plus the human:

1. **Component layer** — *what* the node is in the system (engine / agent / LLM / gate / holdout / human / decision / start-exit).
2. **Verdict layer** — *how a gate/evaluator decided* (pass / warn / fail / error). Lives on edges and badges, not on the node body.
3. **Decision layer** — *where the graph branches on a verdict* (the diamond, the conditional edge). Often overlaps with verdict color on the outgoing edge.

If you find yourself wanting a fourth layer, you are conflating two of these. Stop.

---

## Component layer (node bodies)

The pipeline engine runs `.dot` via Graphviz and the README uses Excalidraw. Both renderers must produce the same color for the same role. Tokens below are the binding vocabulary.

| Role | Token name | `.dot` (`fillcolor`) | Excalidraw fill | Excalidraw stroke | Meaning |
|------|------------|----------------------|-----------------|-------------------|---------|
| **Engine** | `--color-engine` | `#d1fae5` | `#d1fae5` | `#065f46` | Pipeline engine / runner — `runner/*.py`, `.dot` parser, CXDB writer, Healer. Layer 1 of the 3-layer convergence. |
| **Agent** | `--color-agent` | `#dbeafe` | `#dbeafe` | `#1e3a8a` | External coding agent — Claude Code, Codex CLI, Antigravity, AO worker. Layer 2 of the convergence. |
| **LLM** | `--color-llm` | `#ede9fe` | `#ede9fe` | `#5b21b6` | LLM client / inference gateway — OpenClaw, thinclaw MCP. Layer 3 of the convergence. |
| **Gate** | `--color-gate` | `#fef3c7` | `#fef3c7` | `#92400e` | Gate / verdict node — `gate_es`, `gate_er`, `gate_code_standards`, public acceptance. Where the system *decides*. |
| **Holdout** | `--color-holdout` | `#fee2e2` | `#fee2e2` | `#991b1b` | Sealed holdout / evaluator — `holdout_eval` node, sibling repo at `$DARK_FACTORY_HOLDOUTS`. Adversarial, structurally blind to the implementer. |
| **Human** | `--color-human` | `#e5e7eb` | `#e5e7eb` | `#374151` | Human operator — `human_gate` node, the engineer reading this. |
| **Decision** | `--color-decision` | `#fde68a` | `#fde68a` | `#a16207` | Diamond / conditional — pure control flow, no semantic weight of its own. |
| **Start/Exit** | `--color-start-exit` | `#a7f3d0` | `#a7f3d0` | `#047857` | Pipeline `start` and `exit` nodes — entry and terminal. |

**Total: 8 component tokens.** If your diagram needs a 9th, you are conflating roles.

---

## Verdict layer (edge labels + badges)

Verdict is *the result of a check*, not the role of the node. A `holdout_eval` node stays **red (Holdout role)** even when it returns `pass`. The verdict color is shown on the **outgoing edge** (or in a small badge next to the verdict).

| Verdict | Token | `.dot` (edge `color`) | Excalidraw stroke | Meaning |
|---------|-------|----------------------|-------------------|---------|
| **Pass** | `--color-pass` | `#15803d` | `#15803d` | Verdict = pass — node continues to next stage. |
| **Warn** | `--color-warn` | `#a16207` | `#a16207` | Verdict = warn — proceed with annotation. |
| **Fail** | `--color-fail` | `#b91c1c` | `#b91c1c` | Verdict = fail — node routes to `fix` loop. |
| **Error** | `--color-error` | `#6d28d9` | `#6d28d9` | Verdict = error — infra crash, not a real failure. Healer groups these separately from `fail`. |

The `_parse_verdict` function in `runner/handlers.py` normalizes raw outputs to exactly these four verdicts (plus `partial` and `inconclusive` which currently alias to `fail` / `error` — see the Healer logic for the grouping). Anything else surfaces as `error` so it shows up as the violet edge instead of getting silently dropped.

---

## How to apply in a `.dot` file

For each node, set `fillcolor` and `fontcolor` from the component table. For each conditional edge, set `color` from the verdict table. Example:

```dot
spec_review      [shape=box, style="filled,rounded", fillcolor="#ede9fe", fontcolor="#5b21b6", label="spec review\n(codex)"]
holdout_eval     [shape=diamond, style="filled", fillcolor="#fee2e2", fontcolor="#991b1b", label="sealed holdout"]
holdout_eval -> exit [color="#15803d", label="pass"]   // green edge
holdout_eval -> fix  [color="#b91c1c", label="fail"]   // red edge
```

For diagrams with repeated roles, declare a `node [style=filled]` default at the top and override per node, or use a Graphviz record/cluster to group same-role nodes.

---

## How to apply in an Excalidraw diagram

Pull values from the same table. The Excalidraw skill at `~/.claude/skills/excalidraw-diagram/references/color-palette.md` is auto-generated from this table and re-imported whenever the table changes. **If you change a color here, you must also update the skill's `color-palette.md`** — otherwise pipeline previews and the README will disagree.

---

## Forbidden patterns

These mistakes break the semantic contract. If you spot one, fix it before merging.

- **Two different component colors on the same diagram with no legend.** Either it's a real role distinction (add it to the table) or it's noise.
- **Pure black `#000000` or pure white `#ffffff` backgrounds.** Always use the slate scale (`#0f172a` for code, `#fefefe` for canvas).
- **Hot pink `#ec4899`.** Reserved for danger / override prompts in evidence artifacts only.
- **More than 8 distinct component fills in one diagram.** You are conflating layers.
- **Verdict color on a node body.** Verdict lives on edges and badges, not on the node.

---

## Single source of truth

The 8 component tokens and 4 verdict tokens above are the contract. The two renderers that consume them:

- **Graphviz `.dot`** — consumed by `dot -Tpng pipelines/factory/gates.dot` etc. Each `.dot` file in `pipelines/` should use these values directly (or via a shared header `pipelines/_style.inc` once we factor one out).
- **Excalidraw** — consumed by the Excalidraw skill at `~/.claude/skills/excalidraw-diagram/references/color-palette.md`, which is a mirror of this table.

When a token value changes, update **both** in the same commit. When a new role is added, add a row here first, then mirror to the skill.

---

## Cross-references

- `~/.claude/skills/excalidraw-diagram/references/color-palette.md` — the Excalidraw mirror of this table
- `docs/dynamic-vs-deterministic-workflow.md` — diagrams in that doc use these tokens
- `benchmarks/attractor-spec-review/pipelines/review_slim.dot` — small canonical example of a 3-color pipeline
- `pipelines/factory/gates.dot` — large canonical example exercising 5–6 component colors
- `runner/handlers.py:_parse_verdict` — the function that maps raw gate output to the 4 verdict tokens
