# tests/test_gates.py — File Ownership Index

**Status**: Map PR for bead `jleechan-gs2`. Documentation only — no production code changes.
**Actual line count at HEAD**: 1370 lines (prompt stated 1173; difference is WIP drift since the bead was filed).
**Target**: ≤ 400 lines per new test file after the follow-up split PR. The runtime split caps at ~600; tests can be tighter because they exercise one handler family at a time.

## External API surface

This is a test file. There are no importers — only assertions. The "external surface" is therefore *which runtime handler / system-under-test each test exercises*. The split mirrors the runtime split in `file-ownership-map.handlers.md`.

## Top-level index (by responsibility family)

### Pipeline orchestration smoke tests (L24–L207)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_parse_verdict_pass_warn_fail` | 24–35 | `_parse_verdict` | Pure parser. Belongs with verdict-parsing test family. |
| `test_gate_echo_seeded_outcome` | 38–53 | `run` + `_gate_es`/`_gate_er`/`_gate_cs` echo branch via `ctx.state` | Echo-path smoke. Belongs with echo/seed test family. |
| `test_pr_gates_runs_holdout_before_evidence_gates` | 56–74 | `run` + `gates.dot` ordering | Pipeline-shape smoke. Belongs with orchestration. |
| `test_pr_gates_holdout_failure_short_circuits` | 77–90 | `run` short-circuit on holdout failure | Pipeline-shape smoke. |
| `test_gate_failure_short_circuits` | 93–107 | `run` short-circuit on gate failure | Pipeline-shape smoke. |
| `test_gate_nonzero_returncode_cannot_spoof_pass` | 110–128 | `run` + `_run_gate_once` non-zero rc path | Belongs with gate-dispatch test family. |
| `test_cxdb_records_steps` | 131–155 | `run` + `CXDB.record_step` | Belongs with CXDB/integration test family. |
| `test_healer_reports_failures` | 158–171 | `run` + `healer.report` | Belongs with CXDB/integration test family. |
| `test_healer_reports_gate_infra_errors` | 174–190 | `run` + `healer.report` for `outcome="error"` | Belongs with CXDB/integration test family. |
| `test_healer_no_failures` | 193–207 | `run` + `healer.report` happy path | Belongs with CXDB/integration test family. |

### Universal-prompt fallback tests (L215–L297)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_gate_es_uses_universal_prompt_when_local_es_md_absent` | 215–256 | `_gate_es` universal-prompt fallback | Regression for PR #39. Belongs with universal-prompt family. |
| `test_gate_code_standards_uses_universal_prompt_when_local_file_absent` | 259–297 | `_gate_code_standards` universal-prompt fallback | Regression for PR #39. |

### agy reviewer + claude fallback (L308–L399)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `_agy_gate_node` (helper) | 308 | build an agy-backed gate node | Helper. |
| `test_gate_er_runs_agy_when_backend_agy` | 312–339 | `_gate_er` with backend=agy | Belongs with agy-fallback family. |
| `test_gate_agy_falls_back_to_claude_on_infra_failure` | 342–370 | `_execute_gate` agy→claude infra fallback | Belongs with agy-fallback family. |
| `test_gate_agy_real_fail_verdict_not_retried` | 373–399 | `_execute_gate` no-reviewer-shopping for real agy verdict | Belongs with agy-fallback family. |

### Adversarial priority queue (L411–L584)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `_priority_node` (helper) | 411–415 | build a node with `backend_priority=` | Helper. |
| `test_adversarial_priority_picks_first_installed` | 418–448 | `_resolve_adversarial_backend`, `_resolve_gate_backend` | Belongs with priority-queue family. |
| `test_adversarial_priority_skips_coder_backend_when_prefer_adversarial` | 451–492 | same | Belongs with priority-queue family. |
| `test_adversarial_priority_env_override_honored` | 495–512 | same + env var | Belongs with priority-queue family. |
| `test_adversarial_priority_falls_through_to_claude_sonnet_when_nothing_else` | 515–541 | same | Belongs with priority-queue family. |
| `test_adversarial_priority_pinned_across_visits` | 544–583 | same + cross-visit pin | Belongs with priority-queue family. |

### Subprocess dispatch (L592–L779)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_gate_subprocess_args_routes_codex_to_codex_cli` | 592–603 | `_gate_subprocess_args("codex", ...)` | Belongs with subprocess-dispatch family. |
| `test_gate_subprocess_args_routes_claude_sonnet_to_claude_cli` | 606–615 | `_gate_subprocess_args("claude-sonnet", ...)` | Belongs with subprocess-dispatch family. |
| `test_gate_subprocess_args_routes_bare_claude_to_claude_cli` | 618–624 | `_gate_subprocess_args("claude", ...)` | Belongs with subprocess-dispatch family. |
| `test_gate_subprocess_env_routes_minimax_through_minimax_gateway` | 629–633 | `_gate_subprocess_env("minimax")` | Belongs with subprocess-dispatch family. |
| `test_gate_subprocess_env_minimax_is_sanitized` | 636–645 | `_gate_subprocess_env("minimax")` sanitization | Belongs with subprocess-dispatch family. |
| `test_gate_subprocess_env_does_not_set_minimax_for_other_backends` | 648–661 | `_gate_subprocess_env` for non-minimax backends | Belongs with subprocess-dispatch family. |
| `test_execute_gate_runs_codex_subprocess_when_priority_resolves_codex` | 664–697 | `_execute_gate("codex", ...)` end-to-end | Belongs with subprocess-dispatch family. |
| `test_execute_gate_runs_minimax_with_correct_env` | 700–730 | `_execute_gate("minimax", ...)` env override | Belongs with subprocess-dispatch family. |
| `test_resolve_adversarial_backend_falls_back_to_default_when_post_filter_empty` | 733–779 | `_resolve_gate_backend` empty-list fallback | Belongs with subprocess-dispatch family. |

### Custom prompt gates (L789–L933)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_old_spec_review_verdict_tokens_are_unparseable` | 789–797 | `_parse_verdict` rejects "VERDICT: success" | RED proof. Belongs with verdict-parsing family. |
| `test_gate_er_with_prompt_attr_sends_custom_prompt_to_reviewer` | 800–859 | `_gate_er` with `prompt="@..."` | Belongs with custom-prompt family. |
| `test_gate_er_with_missing_prompt_template_errors` | 862–884 | `_gate_er` missing template → error | Belongs with custom-prompt family. |
| `test_gate_er_with_prompt_attr_echo_preseed` | 887–898 | `_gate_er` echo backend with prompt attr | Belongs with custom-prompt family. |
| `test_gate_es_and_cs_with_prompt_attr_route_custom_prompt` | 902–933 | `_gate_es`/`_gate_code_standards` custom prompt | Belongs with custom-prompt family. |

### Universal infra fallback + infra_failure (L941–L1025)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_execute_gate_codex_infra_failure_falls_back_to_claude` | 941–970 | `_execute_gate("codex")` → claude fallback | Belongs with infra-fallback family. |
| `test_execute_gate_codex_real_fail_not_retried` | 973–996 | `_execute_gate("codex")` real FAIL kept | Belongs with infra-fallback family. |
| `test_execute_gate_tags_infra_failure_when_all_backends_die` | 999–1025 | `_execute_gate` `verdict=infra_failure` | Belongs with infra-fallback family. |

### agy fallback (second copy, L1038–L1132)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `_agy_gate_node` (helper, second copy) | 1038 | duplicate of L308 | Helper. Should be deduplicated in the split (single helper in `conftest.py` or shared module). |
| `test_gate_er_runs_agy_when_backend_agy` (second copy) | 1042–1066 | same name as L312 | Duplicate test — keep one in the split. |
| `test_gate_er_falls_back_to_claude_on_agy_infra_failure` | 1069–1102 | agy infra failure → claude | Belongs with agy-fallback family. |
| `test_gate_er_does_not_fall_back_on_real_agy_verdict` | 1105–1132 | agy real fail kept | Belongs with agy-fallback family. |

### gate_slash + branch isolation (L1140–L1267)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `_slash_node` (helper) | 1140 | build a `_gate_slash` node | Helper. |
| `test_gate_slash_missing_command_errors` | 1145–1151 | `_gate_slash` missing command | Belongs with gate_slash family. |
| `test_gate_slash_unknown_command_errors` | 1154–1167 | `_gate_slash` undefined command (no repo + no user-scope) | Belongs with gate_slash family. |
| `test_gate_slash_materializes_user_scope_command` | 1170–1202 | `_gate_slash` materializes `~/.claude/commands/<cmd>.md` into workdir | Belongs with gate_slash family. |
| `test_branch_context_keeps_parent_workdir_for_readonly_gates` | 1205–1224 | `_branch_context` for read-only gate types | Belongs with engine-parallel family. |
| `test_gate_slash_runs_named_command` | 1227–1261 | `_gate_slash` inlines the cmd file into prompt | Belongs with gate_slash family. |
| `test_gate_slash_registered_in_type_registry` | 1264–1267 | `TYPE_REGISTRY["gate_slash"]` exists | Trivial registration check. Belongs with registry smoke. |

### gate_audit (L1280–L1369)

| Symbol | Lines | System under test | Notes |
|---|---|---|---|
| `test_gate_audit_contract` | 1280–1369 | `_gate_audit` end-to-end: missing artifact → stale evidence → unresolved review → replacement/warn → non-replacement/warn → pass | Single test, six contract checks. Belongs with audit-gate family. |

## Proposed split (per-feature test files mirroring the runtime split)

| New file | Lines moved | Cap | Rationale |
|---|---|---|---|
| `tests/test_engine_smoke.py` | L131–L207 (CXDB + Healer smoke) | ~200 | Engine-level integration: `run` + `cxdb` + `healer`. Tests the pipeline-end-to-end loop without exercising specific handlers. |
| `tests/test_pipeline_short_circuit.py` | L56–L128 (short-circuit + spoofing) | ~150 | Pipeline-control-flow tests (gate-failure short-circuit, holdout short-circuit, rc!=0 spoof guard). These are pipeline-mechanics, not handler-mechanics. |
| `tests/test_verdict_parsing.py` | L24–L35 + L789–L797 | ~80 | `_parse_verdict` regression suite. Includes the PR #39 RED proof. |
| `tests/test_gate_universal_prompts.py` | L215–L297 | ~150 | `_gate_es` / `_gate_code_standards` universal-prompt fallback. |
| `tests/test_gate_agy_fallback.py` | L308–L399 + L1038–L1132 (dedupe the duplicate helper + duplicate test) | ~250 | agy→claude infra fallback + no-reviewer-shopping. The two copies of `_agy_gate_node` and `test_gate_er_runs_agy_when_backend_agy` collapse to one. |
| `tests/test_gate_priority_queue.py` | L411–L584 | ~200 | Adversarial-review priority queue: resolver, prefer_adversarial, env override, fallthrough, cross-visit pin. |
| `tests/test_gate_subprocess_dispatch.py` | L592–L779 | ~250 | `_gate_subprocess_args` / `_gate_subprocess_env` / `_execute_gate` per-backend dispatch. |
| `tests/test_gate_custom_prompts.py` | L800–L933 | ~200 | `prompt="@..."` routing for gate_er/es/code_standards. |
| `tests/test_gate_infra_failure.py` | L941–L1025 | ~150 | Infra failure tagging + universal infra fallback (codex→claude timeout path, etc.). |
| `tests/test_gate_slash.py` | L1140–L1267 | ~200 | `_gate_slash` generic single-lane reviewer gate. |
| `tests/test_engine_branch_context.py` | L1205–L1224 | ~50 | `_branch_context` read-only passthrough. |
| `tests/test_gate_audit.py` | L1280–L1369 | ~150 | `_gate_audit` end-to-end contract. |
| `tests/test_gate_registry_smoke.py` | L1264–L1267 (extract registry smoke from gate_slash file) | ~50 | Minimal "every handler in TYPE_REGISTRY has a working echo path" smoke. |
| `tests/test_gates.py` (re-export shim) | only imports + re-exports | ~10 | Additive re-export so existing `pytest tests/test_gates.py` paths keep working. The split PR deletes it after one release cycle. |

**Line-count budget**: every new test file is ≤ 250 lines; per-feature cap mirrors the runtime cap.

**Unknown test functions**: None. Each test above has been read end-to-end and the system-under-test identified. The two duplicate helpers (`_agy_gate_node` at L308 and L1038) and the two duplicate tests (`test_gate_er_runs_agy_when_backend_agy` at L312 and L1042) are flagged for deduplication as part of the split (they are byte-identical except for one helper location).

**Cross-file assertion policy**: monkeypatching is the dominant pattern. Every monkeypatch target (`runner.handlers._worktree_head_sha`, `runner.handlers._sandboxed_args`, etc.) must remain importable at the same path in the new module layout. The split PR must preserve the monkeypatch *target strings* even when moving the function to a new module, OR update the monkeypatch strings in lockstep (preferred for clarity).
