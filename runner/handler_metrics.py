"""Token + wall-clock metrics extraction from backend subprocess output.

Owns:
  * `_TOKEN_IN_RE`, `_TOKEN_OUT_RE`, `_TOKEN_TOTAL_RE`, `_COST_RE` — anchored
    regexes for the most common token-banner shapes emitted by claude / codex
    / ao workers.
  * `_parse_int` — strip ``_``/``,`` then ``int()``.
  * `_last_match` — last regex match across bodies (token banners prefer the
    FINAL summary line).
  * `_codergen_metrics` — extract ``{wall_ms, tokens_in, tokens_out,
    cost_usd, tokens_total}`` from stdout/stderr.
  * `_claude_json_result` — parse ``--output-format json`` envelope, fall
    back to regex on parse failure.
"""

from __future__ import annotations

import json
import re
from typing import Optional


# Anchored regexes for the most common token-banner shapes emitted by claude /
# codex / ao workers. We tolerate either snake_case or space-separated and
# decimal separators. Each pattern captures a single integer.
_TOKEN_IN_RE = re.compile(
    r"(?:input[_ ]tokens|prompt[_ ]tokens|tokens[_ ]in)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_TOKEN_OUT_RE = re.compile(
    r"(?:output[_ ]tokens|completion[_ ]tokens|tokens[_ ]out)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_TOKEN_TOTAL_RE = re.compile(
    r"(?:total[_ ]tokens|tokens)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_COST_RE = re.compile(
    r"(?:cost[_ ]usd|total[_ ]cost|cost)\s*[:=]\s*\$?\s*(\d+(?:\.\d+)?)",
    re.IGNORECASE,
)


def _parse_int(raw: str) -> Optional[int]:
    if raw is None:
        return None
    cleaned = raw.replace("_", "").replace(",", "")
    try:
        return int(cleaned)
    except (TypeError, ValueError):
        return None


def _last_match(pattern: re.Pattern[str], *bodies: str) -> Optional[str]:
    """Return the last regex match across the supplied bodies, or None.

    Token banners are typically the FINAL summary line — we prefer the last
    occurrence so progress chatter (e.g. streaming usage updates) doesn't win
    over the authoritative final number.
    """
    last: Optional[str] = None
    for body in bodies:
        if not body:
            continue
        for m in pattern.finditer(body):
            last = m.group(1)
    return last


def _codergen_metrics(stdout: str, stderr: str, wall_ms: int) -> dict:
    """Best-effort extraction of token/cost metrics from a backend subprocess.

    The contract is:
      * `wall_ms` is always populated (callers measure it).
      * `tokens_in`, `tokens_out`, `cost_usd` may each be `None` when the
        backend did not emit a recognised banner. Returning `None` (vs. zero)
        is important so aggregates don't silently undercount.

    Strategy: scan stdout first, then stderr, and keep the LAST hit so a
    final usage summary wins over intermediate progress prints. If only a
    total-tokens banner is found and no explicit input/output split, we
    record it under `tokens_total` so the Healer can still aggregate.
    """
    tokens_in = _parse_int(_last_match(_TOKEN_IN_RE, stdout, stderr))
    tokens_out = _parse_int(_last_match(_TOKEN_OUT_RE, stdout, stderr))
    cost_raw = _last_match(_COST_RE, stdout, stderr)
    cost_usd: Optional[float] = None
    if cost_raw is not None:
        try:
            cost_usd = float(cost_raw)
        except (TypeError, ValueError):
            cost_usd = None

    metrics: dict = {"wall_ms": int(wall_ms)}
    # Only include keys whose value is meaningful — None is allowed so
    # downstream consumers can distinguish "absent" from "zero".
    metrics["tokens_in"] = tokens_in
    metrics["tokens_out"] = tokens_out
    metrics["cost_usd"] = cost_usd

    # If no explicit in/out split was found but a generic "tokens=" or
    # "total_tokens=" was, surface it under tokens_total for the Healer.
    if tokens_in is None and tokens_out is None:
        total_raw = _last_match(_TOKEN_TOTAL_RE, stdout, stderr)
        total = _parse_int(total_raw)
        if total is not None:
            metrics["tokens_total"] = total
    return metrics


def _claude_json_result(stdout: str, stderr: str, wall_ms: int) -> tuple[str, dict]:
    """Parse a ``claude --print --output-format json`` envelope.

    The codergen claude branch requests JSON output specifically so the coder's
    token usage and dollar cost are observable (plain ``--print`` text emits no
    usage banner, leaving the cost axis blind). Returns ``(output_text, metrics)``
    where ``output_text`` is the human-readable ``result`` field and ``metrics``
    carries authoritative ``tokens_in`` / ``tokens_out`` / ``cost_usd`` pulled
    from the envelope's ``usage`` / ``total_cost_usd``.

    Robust fallback: when stdout is not the expected JSON object (older CLI, an
    error preamble, a non-JSON crash), we fall back to the raw text plus the
    regex-based ``_codergen_metrics`` so the contract (wall_ms always, tokens
    None-not-zero when absent) still holds.
    """
    metrics = _codergen_metrics(stdout, stderr, wall_ms)
    output_text = stdout
    stripped = stdout.strip()
    if stripped.startswith("{"):
        try:
            envelope = json.loads(stripped)
        except (ValueError, TypeError):
            envelope = None
        if isinstance(envelope, dict):
            result_text = envelope.get("result")
            if isinstance(result_text, str):
                output_text = result_text
            usage = envelope.get("usage")
            if isinstance(usage, dict):
                # `input_tokens` is only the FRESH (uncached) input; with prompt
                # caching most input lands in cache_read / cache_creation. The
                # honest "tokens the coder made the model process" is the sum, so
                # tokens_in counts all three. The cache split is kept separately
                # for transparency. (cost_usd below is the cache-priced truth.)
                fresh = usage.get("input_tokens")
                cache_read = usage.get("cache_read_input_tokens")
                cache_create = usage.get("cache_creation_input_tokens")
                parts = [v for v in (fresh, cache_read, cache_create) if isinstance(v, int)]
                if parts:
                    metrics["tokens_in"] = sum(parts)
                    if isinstance(fresh, int):
                        metrics["tokens_in_fresh"] = fresh
                    if isinstance(cache_read, int):
                        metrics["tokens_cache_read"] = cache_read
                    if isinstance(cache_create, int):
                        metrics["tokens_cache_create"] = cache_create
                to = usage.get("output_tokens")
                if isinstance(to, int):
                    metrics["tokens_out"] = to
            cost = envelope.get("total_cost_usd")
            if isinstance(cost, (int, float)):
                metrics["cost_usd"] = float(cost)
    return output_text, metrics
