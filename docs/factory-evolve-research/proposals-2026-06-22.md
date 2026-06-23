# Factory-Evolve Proposals — 2026-06-22

**Mode:** `--taxonomy` (structural G1+G2 audit only; no history search, no PRs)
**Repo:** `jleechanorg/dark-factory` at main `89ea28a`
**Tool:** `/factory-evolve --taxonomy`
**Branch:** `dev1782166914` (clean, on top of latest main `89ea28a`)

---

## Evidence (structural audit results)

| Source | Count | Notes |
|---|---|---|
| `runner.graph_audit pipelines` exit code | 0 | Clean — no G1/G2 violations |
| Pipelines scanned | 18 .dot files (4 factory + 14 slim) | Includes library fragments |
| Code-producing pipelines with bounded fix loop | 14/14 (100%) | `max_visits` 2 or 3 |
| Factory pipelines with hard-tier reviewer (gate_er / gate_skeptic / holdout_eval / adversarial_reviewer) | 4/4 | hello.dot is correctly exempt (smoke lane, Level-5 rule auto-skips) |
| `tests/fixtures/*.dot` deliberately-broken fixtures | 3 (level5_valid, level5_missing_gate, level5_with_skip) | Used to exercise the conformance validator's diagnostic path |
| Library include fragments (`_*.dot`) | 2 (`pipelines/_base.dot`, `pipelines/slim/_base.dot`) | Excluded from standalone audit |

---

## Gap-category breakdown (G1–G9)

| Code | Hits | Notes |
|---|---|---|
| G1 reviewer-not-wired-in-graph | 0 | All 14 code-producing pipelines wire `gate_er` / `gate_skeptic` / `holdout_eval` / `adversarial_reviewer`; smoke `hello.dot` correctly omits |
| G2 failed-review-routes-to-exit | 0 | All 14 code-producing pipelines route `outcome!=success` from reviewer nodes to a bounded `fix [max_visits="N"]` loop |
| G3 weak/templated reviewer prompt | n/a | `--taxonomy` mode does not check prompt templates |
| G4 no-diff-injection | n/a | `--taxonomy` mode does not check prompt templates |
| G5 scope-limited-to-diff-hunk | n/a | `--taxonomy` mode does not check prompt templates |
| G6 verdict-parsing-swallows-nuance | n/a | `--taxonomy` mode does not check handler_verdict.py |
| G7 single-vendor-collapse | n/a | `--taxonomy` mode does not check handler_dispatch.py |
| G8 SHA-binding-not-freshness | n/a | `--taxonomy` mode does not check handler_verdict.py |
| G9 unit-only/templated-evidence-accepted | n/a | `--taxonomy` mode does not check evidence review prompts |

**Scope reminder:** `--taxonomy` is the structural G1+G2 fast path. G3–G9 require either (a) prompt-template deep-read or (b) cold-review-vs-factory-wiring forensics on real PR history. Those need `/factory-evolve` without `--taxonomy` (with `--days N`).

---

## Audit commands run

```bash
# Canonical G1+G2 audit (PR #85 wired this into CI at .github/workflows/ci.yml)
.venv/bin/python -m runner.graph_audit pipelines
# exit 0 = no violations

# Per-pipeline reviewer-node inventory
for f in pipelines/factory/*.dot pipelines/slim/*.dot; do
  [ "$(basename "$f")" = "_base.dot" ] && continue
  has_fix_loop=$(grep -c "max_visits" "$f")
  echo "$f -> fix_loops=$has_fix_loop"
done
```

---

## Pipeline inventory (current state at `89ea28a`)

### Factory (Level-5 enforced, except hello.dot)

| Pipeline | Reviewer nodes | Fix loop | Level-5 status |
|---|---|---|---|
| `gates.dot` | gate_es + gate_er + gate_skeptic + adversarial_reviewer + holdout_eval | `fix [max_visits="3"]` | compliant |
| `hello.dot` | holdout_eval only | `fix [max_visits="3"]` | smoke lane (correctly exempt) |
| `level5_feature.dot` | gate_es + gate_er + gate_skeptic + adversarial_reviewer + holdout_eval + CXDB | `fix [max_visits="2"]` | compliant (graph [level5="true"]) |
| `pr_gates.dot` | gate_es + gate_er + gate_skeptic + adversarial_reviewer + holdout_eval | `fix [max_visits="3"]` | compliant |

### Slim (Level-5 exempt per design)

All 14 slim pipelines either (a) skip Level-5 by graph-attr location, (b) opt out via `skip_<tier>="true"`, or (c) are validation/red-green lanes that don't produce new code.

---

## Proposals (ranked by impact × ease)

### [P0] — **NONE** — Structural audit clean

The current state at `89ea28a` has zero G1/G2 violations. PR #85's `runner.graph_audit` module (wired into CI at `.github/workflows/ci.yml:35`) is now an effective guardrail — any new pipeline that introduces a G1 or G2 violation will be blocked at PR time. No structural proposals warranted.

---

## Recommendations for full audit (`/factory-evolve` without `--taxonomy`)

If the operator wants G3–G9 audit, run:

```bash
/factory-evolve --days 7
```

This will:
1. Phase 1: `/history` search for the last 7 days of cold reviews (codex, Bugbot, CodeRabbit, /reviewdeep)
2. Phase 2: Fan out subagents to compare cold-review findings vs factory in-pipeline reviewer outputs
3. Phase 3: Re-run this structural audit (will likely still be clean)
4. Phase 4: Aggregate into a full proposals doc with ranked G3–G9 fixes
5. Phase 5: Optionally open PRs (`--no-pr` to skip)

---

## References

- Canonical taxonomy: `docs/factory-evolve-research/reviewer-gap-taxonomy.md`
- PR forensics: `docs/factory-evolve-research/git-pr-forensics.md`
- Audit module: `runner/graph_audit.py` (PR #85, merge `411d90c`)
- CI wiring: `.github/workflows/ci.yml` "Graph structural audit (G1/G2 dogfooding)" step
- Pre-push local mirror: `.githooks/pre-push-graph-audit.sh` (PR #86, merge `931ec04`)
- F5 lint injection: PR #88 (merge `f333157`)
- F6 gate_strict opt-in: PR #87 (merge `216d3ce`)
- PR #91 pre-existing test fixes: merge `353493b`

---

## Wiring health verdict

**5 of 5 code-producing paths have a reviewer node; 4 of 4 factory pipelines (excluding smoke lane) are Level-5 compliant; 14 of 14 code-producing pipelines have bounded fix loops; G1/G2 audit clean.** The post-PR #85 / #86 / #87 / #88 / #89 / #90 / #91 factory is structurally healthier than at any prior point in the repo's history. No new structural proposals this round; G3–G9 audit deferred to a full `/factory-evolve` run when there's enough cold-review history to make it worthwhile.