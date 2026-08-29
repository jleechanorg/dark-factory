"""Single source of truth for outcome string normalization.

Both runner.engine and runner.perf_log need to bucket raw outcome strings
(success/pass/warn -> success; partial -> partial; failure/fail/exhausted/
stuck/inconclusive -> failure; error -> error). The function was previously
duplicated in both modules with a 'duplicated to avoid circular imports'
comment that was provably wrong — engine.py already imports perf_log.
"""
from __future__ import annotations


def _classify_outcome(raw: str) -> str:
    """Normalize diverse outcomes to a small engine outcome taxonomy.

    Notes:
      - `pass` and `warn` are treated as success for routing.
      - `partial` is preserved as partial so caller can opt into
        `allow_partial` semantics.
      - `exhausted` and `stuck` are preserved as distinct terminal outcomes.
      - Any non-empty unknown token collapses to failure.
    """
    value = str(raw).strip().lower()
    if value in {"success", "pass", "warn"}:
        return "success"
    if value in {"partial", "partial_success"}:
        return "partial"
    if value == "error":
        return "error"
    if value in {"exhausted", "stuck"}:
        return value
    if value in {"failure", "fail", "inconclusive"}:
        return "failure"
    return "failure"
