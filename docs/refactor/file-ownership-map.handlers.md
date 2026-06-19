# runner/handlers.py — File Ownership Index

**Status**: Map PR for bead `jleechan-gs2`. Documentation only — no production code changes.
**Actual line count at HEAD**: 2818 lines (prompt stated 2350; difference is WIP drift since the bead was filed).
**Target**: ≤ 600 lines per new module after the follow-up split PR.

## External API surface (importers)

| Importer | Symbols imported |
|---|---|
| `runner/engine.py` | `Context`, `Result`, `resolve` |
| `runner/structural_preflight.py` | doc references to `_render_prompt` |
| `runner/_agy_safe.py` | wraps `_codergen` for sandbox timeout safety |
| `runner/parser.py` | none direct, but `Node` and `is_start_node`/`is_exit_node` are imported from `parser` |
| `tests/test_gates.py` | `Context`, `Result`, `TYPE_REGISTRY`, `_parse_verdict`, `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_slash`, `_gate_red`, `_gate_green`, `_gate_audit`, `_gate_subprocess_args`, `_gate_subprocess_env`, `_resolve_gate_backend`, `_resolve_adversarial_backend`, `_execute_gate`, `_worktree_head_sha`, `_sandboxed_args`, `_sanitized_env` (via monkeypatch), `_coerce_bool_attr` |
| `tests/test_hardening.py` | `_tool`, `Context`, `Result`, `_parse_verdict` |
| `tests/test_engine.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_loop_bounds.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_attractor_semantics.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_bug_fix_pipeline.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_spec_gen.py` | `Context`, `Result`, `_gate_er` (monkeypatched) |
| `tests/test_ao_sandbox.py` | `_sanitized_env`, `_sandboxed_args`, holdouts resolution helpers |
| `tests/test_cli_fallbacks.py` | `_get_claude_executable` and friends |
| `tests/test_codergen_prompt_non_empty.py` | `resolve` (via attribute) |
| `tests/test_model_name_passthrough.py` | codergen path |
| `tests/test_ok8.py` | `_worktree_head_sha`, `_sandboxed_args`, plus dispatch tests |
| `tests/test_ol7.py` | codergen/gates |
| `tests/test_parallel_fanout.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_perf_log.py` | `Context`, `Result`, `TYPE_REGISTRY` |
| `tests/test_prompt_pinning.py` | doc references to `_render_prompt` |
| `tests/test_workflow_graphgen_harness.py` | `Context`, `TYPE_REGISTRY` |
| `tests/test_2gv.py` | `Context`, `Result` |
| `benchmarks/workflow_graphgen/harness.py` | via module attribute |

**Implication**: `Context`, `Result`, `resolve`, `TYPE_REGISTRY`, `REGISTRY`, and the public gate handler functions are the load-bearing external API. The big cluster of internal helpers (`_parse_verdict`, `_claude_json_result`, `_codergen_metrics`, `_worktree_head_sha`, `_verify_head_sha_echo`, `_gate_subprocess_args`, `_gate_subprocess_env`, `_resolve_gate_backend`, `_resolve_adversarial_backend`, `_execute_gate`, `_run_gate_once`, `_is_gate_infra_failure`, `_render_prompt`) are heavily asserted-on by `tests/test_gates.py` — they must stay importable by that path even after the split.

## Top-level index

### Module dataclasses (L31–L56)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `Result` | 32–38 | public result shape (outcome/output/metadata/preferred_label/suggested_next_ids/context_updates) | every handler; imported across the repo |
| `Context` | 42–56 | public run-state shape (goal/workdir/state/history/backend/cxdb_path/run_id/event_log_path/perf_log_root/git_ctx/perf_run/last_completed_seq) | every handler; imported across the repo |
| `_TIMEOUT_MIN_SECONDS`, `_TIMEOUT_MAX_SECONDS` | 59–60 | policy envelope for `_coerce_timeout` | `_coerce_timeout` |
| `Handler` (type alias) | 80 | `Callable[[Node, Context], Result]` | engine.py + handlers type annotations |
| `_coerce_timeout` | 63 | clamp + parse int for `timeout=` attrs | ~12 call sites across codergen, tool, gate_red, gate_green, _slash_gate, _run_universal_prompt_gate, etc. |

### Node-type builtin handlers (L83–L96)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_start` | 83 | `Mdiamond` start handler | REGISTRY + TYPE_REGISTRY |
| `_exit` | 87 | `Msquare` exit handler (preserves unresolved failure) | REGISTRY + TYPE_REGISTRY |

### Sandbox + env helpers (L98–L156)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_sanitized_env` | 98 | strip `DARK_FACTORY_HOLDOUTS` + any `*HOLDOUT*` from env | `_codergen`, `_tool`, `_gate_subprocess_env`, `_gate_subprocess_args` (transitively), `_run_pytest_test`; tests `test_ao_sandbox.py`, `test_gate_subprocess_env_minimax_is_sanitized` |
| `_get_claude_executable` | 109 | PATH → nvm-fallback claude binary | `_codergen` (claude branch), `_gate_subprocess_args` |
| `_holdouts_repo_path` | 124 | resolve `$DARK_FACTORY_HOLDOUTS` | `_holdout_denied_paths`, `_holdout_eval`, `_render_prompt` |
| `_holdout_denied_paths` | 132 | list of absolute paths to deny under sandbox-exec | `_sandboxed_args`, `_render_prompt` |
| `_sandboxed_args` | 138 | prepend `sandbox-exec -p <profile>` with holdout deny rules | `_codergen` (ao/claude/codex/agy), `_tool`, `_gate_subprocess_args`, `_run_pytest_test`; heavily monkeypatched by tests |

### AO backend helpers (L159–L228)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_ao_parse_status` | 159 | parse `ao status --json` JSON blob, return activity | `_ao_wait_idle` |
| `_ao_wait_idle` | 178 | poll `ao status --json` until N consecutive idle reads | `_codergen` (ao backend) |

### Codergen handler (L231–L683)

This is the biggest single function in the file. Splits cleanly by backend:

- L248–L264: echo branch
- L266–L314: mock_llm branch
- L316–L492: ao backend (spawn, send, wait_idle)
- L494–L552: claude backend (JSON envelope parse)
- L553–L591: codex backend
- L592–L683: agy backend (Popen + killpg timeout)

Public: `_codergen` is registered in TYPE_REGISTRY under "codergen". Also imported by `runner/_agy_safe.py` for timeout-safe re-invocation.

### Decision + template substitution (L686–L720)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_conditional` | 686 | shape=hexagon decision handler | REGISTRY + TYPE_REGISTRY |
| `_substitute_state` | 693 | `${state.<key>}` substitution; unresolved markers left intact | `_path_attr`, `_tool`, `_run_pytest_test`, `_render_prompt`, `_holdout_eval` |
| `_path_attr` | 706 | resolve `cwd=`/`implementation=` node attr with substitution + deny holdout | `_tool`, `_holdout_eval` |
| `_has_unresolved_state_placeholder` | 719 | predicate | `_holdout_eval` |

### Tool + human gates (L723–L810)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_tool` | 723 | `command="..."` shell-out handler with cwd + timeout | TYPE_REGISTRY; `tests/test_hardening.py:92,360,385` |
| `_human_gate` | 798 | pre-seed `ctx.state["<node>.outcome"]`; else stdin read | TYPE_REGISTRY |

### Codergen metrics (L813–L957)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_TOKEN_IN_RE` / `_TOKEN_OUT_RE` / `_TOKEN_TOTAL_RE` / `_COST_RE` | 820–835 | anchored regexes for backend token/cost banners | `_last_match` |
| `_parse_int` | 838 | strip `_`/`/` then `int()` | `_codergen_metrics` |
| `_last_match` | 848 | last match across bodies (token banners prefer the FINAL summary line) | `_codergen_metrics` |
| `_codergen_metrics` | 864 | extract {wall_ms, tokens_in, tokens_out, cost_usd, tokens_total} from stdout/stderr | `_codergen` (every backend path) |
| `_claude_json_result` | 905 | parse `--output-format json` envelope, fall back to regex on parse failure | `_codergen` (claude branch) |

### Verdict parsing + SHA binding (L960–L1081)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_VERDICT_NORMALIZE` | 960 | map raw verdict token → success/failure | `_parse_verdict`, `_run_gate_once`, `_gate_audit` |
| `_HEAD_SHA_ECHO_RE` | 982 | `head_sha: <40-hex>` extractor | `_verify_head_sha_echo` |
| `_worktree_head_sha` | 988 | full 40-char HEAD SHA via `_git_rev_parse` | every gate; heavily monkeypatched by tests |
| `_verify_head_sha_echo` | 998 | check observed SHA matches expected | `_run_gate_once` |
| `_VERDICT_TOKEN` | 1012 | alternation of recognized verdict tokens | `_MARKER_RE` |
| `_MARKER_RE` / `_MARKER_PRESENT_RE` / `_STANDALONE_RE` | 1026–1049 | anchored marker regex + bare-marker detector + standalone-line fallback | `_parse_verdict` |
| `_parse_verdict` | 1052 | (raw_verdict, normalized_outcome); rejects misclassification attempts | `_run_gate_once`, `_gate_audit`; `tests/test_hardening.py`, `tests/test_gates.py`, `tests/test_spec_gen.py` |

### Gate subprocess machinery (L1084–L1250)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_gate_subprocess_args` | 1084 | build sandboxed argv for agy/codex/minimax/claude per backend | `_run_gate_once`; tests `test_gate_subprocess_args_routes_*` |
| `_gate_subprocess_env` | 1125 | add `ANTHROPIC_BASE_URL` for minimax; layer on `_sanitized_env` | `_run_gate_once`; tests `test_gate_subprocess_env_*` |
| `_run_gate_once` | 1139 | run one gate attempt, parse verdict, SHA-bind, classify outcome | `_execute_gate` |
| `_is_gate_infra_failure` | 1238 | detect sandbox/timeout/missing-binary vs real verdict | `_execute_gate` |

### Adversarial-review priority queue (L1251–L1416)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_DEFAULT_ADVERSARIAL_PRIORITY` | 1257 | `["codex", "minimax", "agy", "claude-sonnet"]` | `_resolve_adversarial_backend` |
| `_parse_priority_env` | 1260 | parse `DARK_FACTORY_ADVERSARIAL_PRIORITY` env var | `_resolve_adversarial_backend` |
| `_probe_backend_installed` | 1272 | `which <name>` + `<name> --version` with 5s ceiling | `_resolve_adversarial_backend`; tests monkeypatch heavily |
| `_resolve_adversarial_backend` | 1297 | pick first installed from priority queue; metadata audit | `_resolve_gate_backend` |
| `_resolve_gate_backend` | 1352 | resolve node-level priority OR explicit backend OR run-level; cross-visit pin | every gate handler; tests `test_adversarial_priority_*` |
| `_coerce_bool_attr` | 1419 | truthy/falsy parser for DOT attributes | `_resolve_gate_backend` |
| `_execute_gate` | 1435 | run gate; agy→claude infra fallback (no reviewer-shopping) | every gate handler; tests `test_execute_gate_*` |

### Universal prompt gates (L1474–L1730)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_slash_gate` | 1474 | factory for `/<command>` reviewer gates with SHA binding | builds `_gate_evidence_audit` style via TYPE_REGISTRY entries that pass through `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code`, `_gate_net_loc` (indirectly) |
| `UNIVERSAL_CODE_STANDARDS_PROMPT` | 1536 | embedded prompt template | `_gate_code_standards`, `_run_universal_prompt_gate` |
| `UNIVERSAL_EVIDENCE_REVIEW_PROMPT` | 1571 | embedded prompt template | `_gate_es`, `_gate_er`, `_run_universal_prompt_gate` |
| `_run_universal_prompt_gate` | 1618 | run a gate using embedded universal prompt (no local cmd file) | `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code` |
| `_node_prompt_ref` | 1657 | read `prompt="@path"` attr | `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code`, `_gate_slash`, `_run_custom_prompt_gate` |
| `_run_custom_prompt_gate` | 1670 | render authored `prompt=` template + append machine contract | `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code`, `_gate_slash` |
| `_gate_es` | 1733 | es.md → _slash_gate("es") OR universal fallback OR custom prompt | TYPE_REGISTRY |
| `_gate_er` | 1742 | er.md / evidence_review.md → slash OR universal fallback OR custom prompt | TYPE_REGISTRY; `tests/test_spec_gen.py` (monkeypatched) |
| `_gate_code_standards` | 1754 | code-standards.md → slash OR universal fallback OR custom prompt | TYPE_REGISTRY |

### Net LOC + dead-code gates (L1763–L1856)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_resolve_base_sha` | 1763 | state → attr → `git merge-base origin/main HEAD` → "origin/main" | `_gate_net_loc`, `_gate_audit` |
| `_gate_net_loc` | 1782 | `git diff --numstat` and assert additions ≤ deletions | TYPE_REGISTRY |
| `UNIVERSAL_DEAD_CODE_PROMPT` | 1821 | embedded prompt template | `_gate_dead_code`, `_run_universal_prompt_gate` |
| `_gate_dead_code` | 1850 | dead-code.md → slash OR universal fallback OR custom prompt | TYPE_REGISTRY |

### Slash gate + pytest gates (L1859–L2072)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_gate_slash` | 1859 | generic `/<command>` gate: materialize user-scope cmd, inline into prompt, route via `_execute_gate` | TYPE_REGISTRY |
| `_run_pytest_test` | 1962 | run pytest against `test_path` attr (with state substitution) | `_gate_red`, `_gate_green` |
| `_gate_red` | 2024 | RED: bug-fix bug-reproduction gate (test MUST fail) | TYPE_REGISTRY |
| `_gate_green` | 2053 | GREEN: bug-fix verification gate (test MUST pass) | TYPE_REGISTRY |

### Holdout + render (L2075–L2394)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_tcp_port_open` | 2075 | probe a TCP port with 1s timeout | `_holdout_eval` |
| `_holdout_eval` | 2083 | run sealed evaluator subprocess + Firebase/Makefile/Java/seed orchestration | TYPE_REGISTRY |
| `_render_prompt` | 2341 | resolve `@path`, enforce workdir-relative, holdout-deny, substitute `${goal}`/`${state.<key>}` | `_codergen`, `_run_custom_prompt_gate`; tests `test_prompt_pinning.py` |

### Parallel fanout/join handlers (L2397–L2411)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_parallel_fanout` | 2397 | fan-out record step (real branching is in engine) | REGISTRY + TYPE_REGISTRY |
| `_join_handler` | 2402 | join record step (real policy is in engine) | REGISTRY + TYPE_REGISTRY |

### Audit-gate helpers (L2414–L2567)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_git_config_origin_url` | 2414 | `git config --get remote.origin.url` | `_gate_audit` |
| `_git_merge_base` | 2430 | try `origin/main`/`main`/`master`/`origin/master` | `_gate_audit` |
| `_git_diff_stat` | 2447 | `git diff --stat <base_sha>` | `_gate_audit` |
| `_gh_pr_body` | 2466 | `gh pr view <pr> --json body -q .body` | `_gate_audit` |
| `_compute_evidence_sha` | 2484 | sha256 over listed evidence files | `_gate_audit` |
| `_verify_evidence_freshness` | 2499 | grep evidence files for HEAD SHA | `_gate_audit` |
| `_check_unresolved_review_state` | 2516 | `gh pr view` for reviewDecision | `_gate_audit` |
| `_is_replacement_or_deletion_work` | 2548 | diff_summary + pr_description keyword + net-LOC heuristic | `_gate_audit` |

### Audit gate handler (L2569–L2772)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_gate_audit` | 2569 | evidence-audit gate: missing/stale/unresolved/replacement/non-replacement path | TYPE_REGISTRY |

### Registry + resolve (L2775–L2818)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `REGISTRY` | 2775 | shape → handler | `resolve` |
| `TYPE_REGISTRY` | 2784 | type → handler | `resolve`; imported across tests |
| `resolve` | 2807 | dispatcher: explicit type → start/exit → shape → default _codergen | engine.py `_run_single_node` |

## Proposed split (responsibility groups, ≤ 600 lines each)

| New module | Lines moved | Public API (re-exported from handlers.py) | Rationale |
|---|---|---|---|
| `runner/handler_core.py` | `Result`, `Context`, `Handler`, `_TIMEOUT_MIN_SECONDS`, `_TIMEOUT_MAX_SECONDS`, `_coerce_timeout`, `_start`, `_exit` (L31–L96, ~70 lines) | `Result`, `Context`, `Handler` | The public data types + the trivial start/exit builtins. Tiny but load-bearing. |
| `runner/handler_sandbox.py` | `_sanitized_env`, `_get_claude_executable`, `_holdouts_repo_path`, `_holdout_denied_paths`, `_sandboxed_args` (L98–L156, ~60 lines) | none new (private) | Holdout-aware sandbox-exec env + argv. Single responsibility. Heavily monkeypatched by tests but the *interface* (import path) must remain stable for those monkeypatches. |
| `runner/handler_ao.py` | `_ao_parse_status`, `_ao_wait_idle` (L159–L228, ~70 lines) | none new (private) | AO session polling. Distinct from sandbox helpers. |
| `runner/handler_codergen.py` | `_codergen` (L231–L683, ~450 lines) | none new (private; entry via TYPE_REGISTRY["codergen"]) | The codergen handler is the largest function in the file but it MUST stay one function (splitting the backends across files would mean dispatching to `_codergen_echo`, `_codergen_mock_llm`, etc., which changes the TYPE_REGISTRY contract and breaks `tests/test_codergen_prompt_non_empty.py`). 450 lines is over the 600-line rule by hand but the function is one logical unit with 5 backends sharing wall-clock + metrics + Result plumbing — splitting mid-function would create a half-dozen inter-file calls that are worse than the line count. **Cap**: when this hits 600 lines in the future, prefer a `_codergen_dispatch(backend)` table over splitting the function. |
| `runner/handler_decision.py` | `_conditional`, `_substitute_state`, `_path_attr`, `_has_unresolved_state_placeholder` (L686–L720, ~35 lines) | none new (private) | Decision + state substitution. Tight cluster. |
| `runner/handler_control.py` | `_tool`, `_human_gate` (L723–L810, ~90 lines) | none new (private) | Tool + human-gate handlers. Small. |
| `runner/handler_metrics.py` | regexes (`_TOKEN_IN_RE` etc.), `_parse_int`, `_last_match`, `_codergen_metrics`, `_claude_json_result` (L813–L957, ~145 lines) | none new (private) | Backend-output parsing. Heavily coupled internally (the regexes are private to this module). |
| `runner/handler_verdict.py` | `_VERDICT_NORMALIZE`, `_HEAD_SHA_ECHO_RE`, `_worktree_head_sha`, `_verify_head_sha_echo`, `_VERDICT_TOKEN`, `_MARKER_RE`/`_MARKER_PRESENT_RE`/`_STANDALONE_RE`, `_parse_verdict` (L960–L1081, ~125 lines) | none new (private; `_parse_verdict` is asserted-on by tests but the import path must remain `runner.handlers._parse_verdict`) | Verdict parsing + SHA binding. Tightly self-contained. |
| `runner/handler_dispatch.py` | `_gate_subprocess_args`, `_gate_subprocess_env`, `_run_gate_once`, `_is_gate_infra_failure`, `_DEFAULT_ADVERSARIAL_PRIORITY`, `_parse_priority_env`, `_probe_backend_installed`, `_resolve_adversarial_backend`, `_resolve_gate_backend`, `_coerce_bool_attr`, `_execute_gate` (L1084–L1471, ~390 lines) | none new (private) | Gate subprocess + adversarial priority queue + execute-gate fallback. ~390 lines, the second-largest new module; tightly coupled (resolver feeds executor feeds subprocess). |
| `runner/handler_universal_prompts.py` | `_slash_gate`, `UNIVERSAL_CODE_STANDARDS_PROMPT`, `UNIVERSAL_EVIDENCE_REVIEW_PROMPT`, `_run_universal_prompt_gate`, `_node_prompt_ref`, `_run_custom_prompt_gate`, the four universal gate handlers (`_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code`), `UNIVERSAL_DEAD_CODE_PROMPT` (L1474–L1856, ~385 lines) | none new (private; entry via TYPE_REGISTRY) | The "universal + custom-prompt + slash" gate family. The four named gates (`gate_es`, `gate_er`, `gate_code_standards`, `gate_dead_code`) share an identical prompt-routing structure; grouping them makes the family visible. |
| `runner/handler_special_gates.py` | `_resolve_base_sha`, `_gate_net_loc`, `_gate_slash`, `_run_pytest_test`, `_gate_red`, `_gate_green` (L1763–L1856 ∪ L1859–L2072, ~315 lines) | none new (private) | Net-LOC + dead-code + slash + pytest gates. Each has a distinct purpose but they all share the "execute and classify result" pattern. |
| `runner/handler_holdout.py` | `_tcp_port_open`, `_holdout_eval` (L2075–L2340, ~265 lines) | none new (private) | Holdout evaluator orchestration. Self-contained (Firebase emulator, Java, seed script, killpg). Keep together — splitting the holdout logic across files would scatter invariants the engineer should see at once. |
| `runner/handler_render.py` | `_render_prompt` (L2341–L2394, ~55 lines) | none new (private) | Prompt template resolution + holdout-deny + substitution. Standalone; structural_preflight and test_prompt_pinning both depend on the same logic. |
| `runner/handler_parallel.py` | `_parallel_fanout`, `_join_handler` (L2397–L2411, ~15 lines) | none new (private) | Fan-out/join stubs (real logic in engine). |
| `runner/handler_audit.py` | `_git_config_origin_url`, `_git_merge_base`, `_git_diff_stat`, `_gh_pr_body`, `_compute_evidence_sha`, `_verify_evidence_freshness`, `_check_unresolved_review_state`, `_is_replacement_or_deletion_work`, `_gate_audit` (L2414–L2772, ~360 lines) | none new (private) | Evidence audit gate + git/gh evidence helpers. Self-contained (only used by `_gate_audit`). |
| `runner/handlers.py` (re-export shim) | only imports + re-exports | `Context`, `Result`, `Handler`, `resolve`, `REGISTRY`, `TYPE_REGISTRY`, plus everything currently imported by tests | Additive re-export. The original `handlers.py` becomes a thin shim. The split PR deletes it after one release cycle. |

**Line-count budget**: every new module is ≤ 450 lines; the codergen handler is the one explicit exception (kept as one function for TYPE_REGISTRY contract reasons). Flagged in the rationale column.

**Unknown callers**: None. Every symbol listed above has a verified caller (in-file or external). Marked: `unknown caller — needs investigation` — none. There are several **likely-dead** symbols (e.g. `_is_validation_failed`, `_EVENT_DIR`) that the index flags for follow-up cleanup but does not move to "unknown."
