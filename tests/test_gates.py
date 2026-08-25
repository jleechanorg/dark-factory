"""Gate handler + CXDB + Healer smoke tests.

Re-export shim: every test function from the split files is re-exported
here so `pytest tests/test_gates.py` continues to collect every gate
test. The actual test implementations live in the per-feature files
extracted per docs/refactor/file-ownership-map.test_gates.md:

  - tests/test_engine_smoke.py
  - tests/test_pipeline_short_circuit.py
  - tests/test_verdict_parsing.py
  - tests/test_gate_universal_prompts.py
  - tests/test_gate_agy_fallback.py
  - tests/test_gate_priority_queue.py
  - tests/test_gate_subprocess_dispatch.py
  - tests/test_gate_custom_prompts.py
  - tests/test_gate_infra_failure.py
  - tests/test_gate_slash.py
  - tests/test_engine_branch_context.py
  - tests/test_gate_audit.py
  - tests/test_gate_registry_smoke.py
"""

from tests.test_engine_smoke import (  # noqa: F401
    test_gate_echo_seeded_outcome,
    test_cxdb_records_steps,
    test_healer_reports_failures,
    test_healer_reports_gate_infra_errors,
    test_healer_no_failures,
)
from tests.test_pipeline_short_circuit import (  # noqa: F401
    test_pr_gates_runs_holdout_before_evidence_gates,
    test_pr_gates_holdout_failure_short_circuits,
    test_gate_failure_short_circuits,
    test_gate_nonzero_returncode_cannot_spoof_pass,
)
from tests.test_verdict_parsing import (  # noqa: F401
    test_parse_verdict_pass_warn_fail,
    test_old_spec_review_verdict_tokens_are_unparseable,
)
from tests.test_gate_universal_prompts import (  # noqa: F401
    test_gate_es_uses_universal_prompt_when_local_es_md_absent,
    test_gate_code_standards_uses_universal_prompt_when_local_file_absent,
)
from tests.test_gate_agy_fallback import (  # noqa: F401
    test_gate_er_runs_agy_when_backend_agy,
    test_gate_agy_falls_back_to_claude_on_infra_failure,
    test_gate_agy_real_fail_verdict_not_retried,
    test_gate_er_falls_back_to_claude_on_agy_infra_failure,
    test_gate_er_does_not_fall_back_on_real_agy_verdict,
)
from tests.test_gate_priority_queue import (  # noqa: F401
    test_adversarial_priority_picks_first_installed,
    test_adversarial_priority_demotes_coder_backend_when_prefer_adversarial,
    test_prefer_adversarial_reviews_on_coder_backend_when_it_is_the_only_option,
    test_adversarial_priority_env_override_honored,
    test_adversarial_priority_falls_through_to_claude_sonnet_when_nothing_else,
    test_adversarial_priority_pinned_across_visits,
)
from tests.test_gate_subprocess_dispatch import (  # noqa: F401
    test_gate_subprocess_args_routes_codex_to_codex_cli,
    test_gate_subprocess_args_routes_claude_sonnet_to_claude_cli,
    test_gate_subprocess_args_routes_bare_claude_to_claude_cli,
    test_gate_subprocess_env_routes_minimax_through_minimax_gateway,
    test_gate_subprocess_env_minimax_is_sanitized,
    test_gate_subprocess_env_does_not_set_minimax_for_other_backends,
    test_execute_gate_runs_codex_subprocess_when_priority_resolves_codex,
    test_execute_gate_runs_minimax_with_correct_env,
    test_resolve_adversarial_backend_falls_back_to_default_when_post_filter_empty,
)
from tests.test_gate_custom_prompts import (  # noqa: F401
    test_gate_er_with_prompt_attr_sends_custom_prompt_to_reviewer,
    test_gate_er_with_missing_prompt_template_errors,
    test_gate_er_with_prompt_attr_echo_preseed,
    test_gate_es_and_cs_with_prompt_attr_route_custom_prompt,
)
from tests.test_gate_infra_failure import (  # noqa: F401
    test_execute_gate_codex_infra_failure_falls_back_to_claude,
    test_execute_gate_codex_real_fail_not_retried,
    test_execute_gate_tags_infra_failure_when_all_backends_die,
)
from tests.test_gate_slash import (  # noqa: F401
    test_gate_slash_missing_command_errors,
    test_gate_slash_unknown_command_errors,
    test_gate_slash_materializes_user_scope_command,
    test_gate_slash_runs_named_command,
)
from tests.test_engine_branch_context import (  # noqa: F401
    test_branch_context_keeps_parent_workdir_for_readonly_gates,
)
from tests.test_gate_audit import (  # noqa: F401
    test_gate_audit_contract,
)
from tests.test_gate_registry_smoke import (  # noqa: F401
    test_gate_slash_registered_in_type_registry,
)
