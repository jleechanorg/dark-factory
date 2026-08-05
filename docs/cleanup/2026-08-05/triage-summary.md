# Triage summary — dark-factory cleanup swarm 2026-08-05

Workflow wf_7c58132f-e17: 3 review lanes (sonnet) -> haiku dedup -> refute-by-default verify (sonnet). 16 agents, 1,151,090 tokens. Goal bead jleechan-fpnx; roadmap: ~/roadmap/dark-factory/factory-code-cleanup-goal-ironclad-2026-08-05.md.
Both verifier deaths (VOID) were re-verified out-of-band before disposition: one by operator direct-read, one by a fresh adversarial sonnet agent. No finding was silently dropped; capacity drops: 0.

## Cross-model cold review (C6)

Reviewer chain: codex exec (quota-exhausted, could not run) → gemini CLI (client
EOL, IneligibleTierError) → **agy / Antigravity CLI (Gemini family) — completed**.
Execute-and-reproduce methodology: reran full pytest (0 failures), verified the
fan-out leak fix delta=0 in isolation, verified README.html generator idempotency
(clean tree), checked all 6 `current_dispatch_meta` call sites for semantic drift
(none; the `_`-discarded node_name fallback change is inert), audited triage
honesty (all 8 FIXED findings present in diff, all 4 REJECTED absent; one wording
inaccuracy — "stray dirs removed" — corrected in this doc; residual multi-file
disk leak now tracked in bead jleechan-vxi3).

**COLD REVIEW VERDICT: PASS** (no Major findings). Independent /code-standards
diff review (sonnet, separate from implementer): **PASS, 0 violations**.

## Finding table

| finding | lanes | verification | disposition | detail |
|---|---|---|---|---|
| `coder.log-delete` | ponytail,thermo,code-standards | CONFIRMED | **FIXED** | commit d0cee652 |
| `evidence-jsby-delete` | ponytail,thermo,code-standards | CONFIRMED | **FIXED** | commit d0cee652 |
| `evidence-pr387-delete` | ponytail,thermo,code-standards | CONFIRMED | **FIXED** | commit d0cee652 |
| `branch-scratch-dirs-cleanup` | ponytail | CONFIRMED | **FIXED (root cause)** | commit 7fbaf974: leak-source test fix + gitignore guard; stray dirs were untracked, cleared out-of-band. Residual: other test files still leak (disk-only, gitignored) — bead jleechan-vxi3 |
| `readme-html-sync` | ponytail,code-standards | CONFIRMED | **FIXED** | commit 042a2463 (regenerated via diagrams/build_readme_html.py) |
| `parallelization-audit-relocate` | ponytail,thermo | CONFIRMED | **FIXED** | commit 58fb9c54 (git mv + citation link repair) |
| `handler_dispatch-dedup-dispatch-meta` | code-standards | CONFIRMED | **FIXED** | commit 50ce1822 (current_dispatch_meta in handler_core) |
| `skeptic_gate-extract-contract-echo` | thermo | REFUTED-BY-VERIFIER | **REJECTED** | verifier: move+re-export mechanism unsafe (monkeypatch targets, import graph); follow-up bead jleechan-t5ld |
| `engine_run-extract-checkpoint-block` | thermo | REFUTED-BY-VERIFIER | **REJECTED** | repo's own docs/refactor/file-ownership-map.engine.md marks the block do-not-split; optional line-count doc refresh skipped as churn |
| `skeptic_gate_cli-extract-main-prologue` | thermo | REFUTED-BY-VERIFIER | **REJECTED** | verifier: no concrete bounded diff; follow-up bead jleechan-f1a2 |
| `handler_codergen-dedup-shadow-review` | thermo | VOID (verifier died; re-verified out-of-band) | **REJECTED for this PR** | adversarial re-verify (sonnet, fresh agent): risk HIGH — sandbox-scope, expected_sha, and kill-escalation semantics differ; follow-up bead jleechan-txdh |
| `handler_core-remove-invalid-hasattr` | code-standards | VOID (verifier died; re-verified out-of-band) | **FIXED** | commit 4766f5a3 — first verifier died (VOID); operator re-verified by direct read: Context.state is a non-Optional dataclass field, guard could never be False |
