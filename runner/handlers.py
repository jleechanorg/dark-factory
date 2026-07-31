"""Node handlers — what each node *does* when the engine visits it.

This module is now a thin re-export shim. The implementation has been split
into ``runner/handler_core.py``, ``runner/handler_sandbox.py``,
``runner/handler_ao.py``, ``runner/handler_codergen.py``,
``runner/handler_decision.py``, ``runner/handler_control.py``,
``runner/handler_metrics.py``, ``runner/handler_verdict.py``,
``runner/handler_dispatch.py``, ``runner/handler_universal_prompts.py``,
``runner/handler_special_gates.py``, ``runner/handler_holdout.py``,
``runner/handler_render.py``, ``runner/handler_parallel.py``, and
``runner/handler_audit.py``. Every public symbol listed below is re-exported
so external callers (``from runner.handlers import _X`` and
``monkeypatch.setattr("runner.handlers._X", ...)``) keep working unchanged.

The split was mapped in
``docs/refactor/file-ownership-map.handlers.md`` (PR #77, base ``f4412bd``).
"""

from __future__ import annotations

# Re-import subprocess so legacy tests can
# ``monkeypatch.setattr("runner.handlers.subprocess.run", ...)`` against
# the shim namespace (the original ``handlers.py`` did ``import subprocess``
# at module top, and that binding has to remain visible through the shim).
import subprocess  # noqa: F401

# Public data types and trivial builtins.
from .handler_core import (
    Result,
    Context,
    Handler,
    _TIMEOUT_MIN_SECONDS,
    _TIMEOUT_MAX_SECONDS,
    _coerce_timeout,
    _start,
    _exit,
)

# Sandbox + env helpers.
from .handler_sandbox import (
    _sanitized_env,
    _get_claude_executable,
    _holdouts_repo_path,
    _holdout_denied_paths,
    _sealed_benchmark_doc_paths,
    _sandboxed_args,
    _sandboxed_args_for_workdir,
    # Linux isolation backend (jleechan-haux) — re-exported so
    # tests/security/test_agent_isolation.py can exercise + monkeypatch
    # them the same way the macOS sandbox-exec helpers are exercised.
    _linux_preload_lib_path,
    _verify_linux_preload_denies,
    _linux_sandbox_prefix,
    _reset_linux_preload_verification_cache_for_tests,
)

# AO backend helpers.
from .handler_ao import (
    _ao_parse_status,
    _ao_wait_idle,
)

# The single _codergen function (kept whole for the TYPE_REGISTRY contract).
from .handler_codergen import (
    _codergen,
    _capture_diff,
    _DIFF_MAX_CHARS,
)

# Decision + state substitution.
from .handler_decision import (
    _conditional,
    _substitute_state,
    _path_attr,
    _has_unresolved_state_placeholder,
)

# Tool + human-gate handlers.
from .handler_control import (
    _tool,
    _human_gate,
)

# Backend-output parsing (token/cost metrics + claude JSON envelope).
from .handler_metrics import (
    _TOKEN_IN_RE,
    _TOKEN_OUT_RE,
    _TOKEN_TOTAL_RE,
    _COST_RE,
    _parse_int,
    _last_match,
    _codergen_metrics,
    _claude_json_result,
)

# Verdict parsing + SHA binding + outcome/verdict consistency.
from .handler_verdict import (
    _VERDICT_NORMALIZE,
    _HEAD_SHA_ECHO_RE,
    _worktree_head_sha,
    _verify_head_sha_echo,
    _VERDICT_TOKEN,
    _MARKER_RE,
    _MARKER_PRESENT_RE,
    _STANDALONE_RE,
    _parse_verdict,
    _enforce_outcome_verdict_consistency,
)

# Gate subprocess + adversarial priority queue + execute-gate fallback.
from .handler_dispatch import (
    _gate_subprocess_args,
    _gate_subprocess_env,
    _controller_codex_args,
    _run_gate_once,
    _is_gate_infra_failure,
    _DEFAULT_ADVERSARIAL_PRIORITY,
    _parse_priority_env,
    _probe_backend_installed,
    _resolve_adversarial_backend,
    _resolve_gate_backend,
    _coerce_bool_attr,
    _execute_gate,
)

# Universal + custom-prompt + slash gate family.
from .handler_universal_prompts import (
    _slash_gate,
    UNIVERSAL_CODE_STANDARDS_PROMPT,
    UNIVERSAL_EVIDENCE_REVIEW_PROMPT,
    _run_universal_prompt_gate,
    _node_prompt_ref,
    _run_custom_prompt_gate,
    _gate_es,
    _gate_er,
    _gate_code_standards,
    UNIVERSAL_DEAD_CODE_PROMPT,
    _gate_dead_code,
    _gate_skeptic,
)

# Net-LOC + slash + pytest gates.
from .handler_special_gates import (
    _resolve_base_sha,
    _gate_net_loc,
    _gate_slash,
    _run_pytest_test,
    _gate_red,
    _gate_green,
)

# Holdout evaluator orchestration.
from .handler_holdout import (
    _tcp_port_open,
    _holdout_eval,
)

# Prompt template resolution.
from .handler_render import (
    _render_prompt,
)

# Parallel fan-out/join.
from .handler_parallel import (
    _parallel_fanout,
    _join_handler,
)

# Parallel reviewer pair: primary + shadow Codex in one node.
from .handler_parallel_reviewer import (
    _parallel_reviewer,
)

# Evidence audit gate + git/gh helpers.
from .handler_audit import (
    _git_config_origin_url,
    _git_merge_base,
    _git_diff_stat,
    _gh_pr_body,
    _compute_evidence_sha,
    _verify_evidence_freshness,
    _check_unresolved_review_state,
    _is_replacement_or_deletion_work,
    _gate_audit,
)

# Registries + dispatcher live at the bottom of the shim so they pick up
# every re-exported handler above.
from .parser import Node, is_start_node, is_exit_node


REGISTRY: "dict[str, Handler]" = {
    # by shape
    "Mdiamond": _start,
    "Msquare": _exit,
    "hexagon": _conditional,
    "component": _parallel_fanout,      # fan-out shape alias (Kilroy: shape=component)
    "tripleoctagon": _join_handler,     # join shape alias (Kilroy: shape=tripleoctagon)
}

TYPE_REGISTRY: "dict[str, Handler]" = {
    "start": _start,
    "exit": _exit,
    "codergen": _codergen,
    "conditional": _conditional,
    "tool": _tool,
    "human_gate": _human_gate,
    "holdout_eval": _holdout_eval,
    "gate_es": _gate_es,
    "gate_er": _gate_er,
    "gate_code_standards": _gate_code_standards,
    "gate_net_loc": _gate_net_loc,
    "gate_dead_code": _gate_dead_code,
    "gate_skeptic": _gate_skeptic,
    "gate_slash": _gate_slash,
    "gate_red": _gate_red,
    "gate_green": _gate_green,
    "gate_audit": _gate_audit,
    "gate_evidence_audit": _gate_audit,
    "parallel": _parallel_fanout,       # fan-out type (type=parallel)
    "join": _join_handler,              # fan-in type (type=join)
    "parallel_reviewer": _parallel_reviewer,
}


def resolve(node) -> Handler:
    t = node.attrs.get("type")
    if t and t in TYPE_REGISTRY:
        return TYPE_REGISTRY[t]
    if is_start_node(node):
        return _start
    if is_exit_node(node):
        return _exit
    s = node.shape
    if s in REGISTRY:
        return REGISTRY[s]
    return _codergen
