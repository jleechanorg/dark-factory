"""Cross-model reviewer vendor-fallback chain (issue #385 / bead jleechan-984e).

Pure-logic module owning:
  * `CROSS_MODEL_VENDOR_CHAIN` — canonical vendor fallback order
    ``cursor-agent`` → ``codex`` → ``agy``.
  * `CrossModelResolution` / `CrossModelVerdict` — immutable result shapes.
  * `resolve_cross_model_reviewer` — vendor-fallback resolver with the
    differ-from-coder-family rule.
  * `aggregate_cross_model_verdict` — verdict wrapper around the canonical
    ``_parse_verdict`` so cross-model review reuses the same
    verdict-token semantics as ``gate_es`` / ``gate_skeptic``.
  * `build_metadata` — Result.metadata shape consumed by CXDB /
    perf_log. Strict-merge policy #328 reads
    ``metadata["cross_model_degraded"]`` and refuses strict-green when
    it equals ``"true"``.

Hard rule: this module NEVER invents a backend. Empty chain /
uninstalled vendor → ``resolution.vendor is None``,
``resolution.degraded is True``. Callers (the ``gate_cross_model``
node handler) must read both fields and act accordingly.

Wiring philosophy: the cross-model reviewer is a *gate output*. Its
verdict-token semantics live in ``runner.handler_verdict._parse_verdict``,
shared by every other gate; we deliberately route through that single
function instead of inlining another regex here.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Optional, Tuple

from .handler_verdict import _parse_verdict  # canonical verdict semantics
from .handler_dispatch import _probe_backend_installed  # PATH-based vendor probe


# Canonical chain order. The fallback is total: ``cursor-agent`` first
# because it is the spec-named entry; ``codex`` is the high-coverage
# fallback used elsewhere in the dispatch queue; ``agy`` (Gemini /
# Google) is the final broad-coverage fallback.
CROSS_MODEL_VENDOR_CHAIN: Tuple[str, ...] = ("cursor-agent", "codex", "agy")


# Model-family identity. The differ-from-coder rule is enforced at the
# family level — Anthropic Claude ≠ OpenAI Codex ≠ Google Gemini ≠
# Cursor (Anysphere) models. "cursor" is its own family rather than
# folded into anthropic/openai because Cursor ships multi-model
# products and a same-family "cursor" invocation is structurally not
# the same vendor chain entry.
VENDOR_FAMILY: dict[str, str] = {
    "cursor-agent": "cursor",
    "codex": "openai",
    "agy": "google",
}

# Backend → family mapping for the *coder* (implementing agent). Mirrors
# `runner.handlers` mapping so the resolver can read ``ctx.backend`` and
# translate without depending on private handlers internals.
CODER_BACKEND_FAMILY: dict[str, str] = {
    "claude": "anthropic",
    "claude-sonnet": "anthropic",
    "claude-opus": "anthropic",
    "codex": "openai",
    "agy": "google",
    "minimax": "anthropic",  # Anthropic-routed via minimax gateway; same family
    "cursor-agent": "cursor",
    "ao": "anthropic",  # default AO routes via antigravity → Claude
    "antigravity": "anthropic",
    "echo": "echo",  # test-only; treated as a synthetic single-family case
    "mock_llm": "mock",  # test-only
}


@dataclass(frozen=True)
class CrossModelResolution:
    """Resolved cross-model reviewer vendor.

    `skipped` records the chain entries that did NOT resolve (PATH was
    missing or ``--version`` failed) — operator/CXDB audit trail, not a
    free-form message.
    """

    vendor: Optional[str]
    family: Optional[str]
    skipped: Tuple[str, ...] = ()
    degraded: bool = False


@dataclass(frozen=True)
class CrossModelVerdict:
    """Verdict shape produced by `aggregate_cross_model_verdict`.

    `raw_verdict` is the literal token captured by ``_parse_verdict``
    (e.g. ``"pass"``, ``"fail"``, ``"unknown"``).
    `normalized_outcome` is the success/failure map that the runner
    reads from ``Result.outcome``.
    """

    raw_verdict: str
    normalized_outcome: str
    parsed_text: str = ""


def _coder_family(coder_backend: str) -> str:
    """Translate the coder backend name to its model family.

    Unknown / empty coders default to ``"unknown"`` so the resolver
    never silently *agrees* with the coder's family.
    """
    if not coder_backend:
        return "unknown"
    return CODER_BACKEND_FAMILY.get(coder_backend, "unknown")


def _resolve_with_chain(
    chain: Tuple[str, ...],
    coder_family_value: str,
    probe_fn: Callable[[str], bool],
) -> CrossModelResolution:
    """Walk the chain until an installed, cross-family entry is found.

    Differ-from-coder rule: when the coder's family is known and a
    later chain entry has a different family than the coder, prefer
    that entry even if an earlier entry was installed AND had the
    coder's family.

    Walking policy:
      * Pass 1 — find the first installed entry whose family != coder's.
      * Pass 2 — if no cross-family entry exists, pick the first
        installed entry (same-family → ``degraded=True``).
      * Pass 3 — if nothing is installed, return ``vendor=None`` with
        ``degraded=True`` (strict-merge #328 must refuse strict-green).
    """
    if not chain:
        raise ValueError("chain must be a non-empty tuple of vendor names")

    # Validate chain membership against the canonical vendor set so we
    # do not silently invent backends (issue #385 acceptance: failed
    # closed on unknown vendors).
    known = set(CROSS_MODEL_VENDOR_CHAIN)
    extra = [v for v in chain if v not in known]
    if extra:
        raise ValueError(
            f"unknown vendor(s) in chain: {extra}; expected subset of "
            f"{list(CROSS_MODEL_VENDOR_CHAIN)}"
        )

    # ``skipped`` records every entry probed-and-not-resolved BEFORE the
    # resolution point. That covers BOTH unavailable (not installed)
    # entries AND installed-but-same-family entries that were rejected
    # for cross-family escalation. Entries past the resolution point
    # are NOT probed, so they are NOT recorded.
    skipped: list[str] = []
    installed_same_family: list[tuple[str, str]] = []  # (vendor, family)

    cross_pick: Optional[tuple[str, str]] = None
    for vendor in chain:
        if not probe_fn(vendor):
            skipped.append(vendor)
            continue
        family = VENDOR_FAMILY[vendor]
        if coder_family_value in ("unknown", "echo", "mock"):
            # Unknown coder family cannot be compared → any installed
            # entry qualifies as cross-family.
            cross_pick = (vendor, family)
            break
        if family != coder_family_value:
            cross_pick = (vendor, family)
            break
        # Installed but same-family — record the rejection for audit
        # trail. We keep looking for a cross-family upgrade; if none is
        # found, this entry is the degraded fallback.
        skipped.append(vendor)
        installed_same_family.append((vendor, family))

    if cross_pick is not None:
        vendor, family = cross_pick
        return CrossModelResolution(
            vendor=vendor,
            family=family,
            skipped=tuple(skipped),
            degraded=False,
        )

    if installed_same_family:
        vendor, family = installed_same_family[0]
        return CrossModelResolution(
            vendor=vendor,
            family=family,
            skipped=tuple(skipped),
            degraded=True,
        )

    # Nothing installed — fail closed. The handler emits
    # outcome=failure with cross_model_degraded="true", and strict-
    # merge #328 refuses strict-green on this run.
    return CrossModelResolution(
        vendor=None,
        family=None,
        skipped=tuple(skipped),
        degraded=True,
    )


def resolve_cross_model_reviewer(
    coder_backend: str = "",
    *,
    coder_family: Optional[str] = None,
    chain: Tuple[str, ...] = CROSS_MODEL_VENDOR_CHAIN,
    probe_fn: Optional[Callable[[str], bool]] = None,
) -> CrossModelResolution:
    """Resolve the next cross-model reviewer for this run.

    Args:
      coder_backend: Implementing agent backend name (e.g. ``"claude"``,
        ``"codex"``, ``"agy"``). Used to look up the coder's family
        unless ``coder_family`` is supplied explicitly.
      coder_family: Explicit coder family override (escape hatch for
        tests and unusual backend names — supply ``"unknown"`` to
        disable the cross-family preference).
      chain: Vendor chain to walk. Defaults to
        ``CROSS_MODEL_VENDOR_CHAIN``. Must be a non-empty tuple whose
        entries are all members of the canonical chain.
      probe_fn: Vendor probe function. Defaults to
        ``_probe_backend_installed`` so PATH semantics match the rest
        of the dispatch handler. Tests substitute deterministic fakes.

    Returns:
      ``CrossModelResolution`` with ``vendor``, ``family``, ``skipped``,
      ``degraded``. ``vendor is None`` ⇒ no chain entry was installed;
      ``degraded is True`` ⇒ the cross-family preference could not be
      satisfied (caller should surface ``review_degraded=True``).
    """
    if probe_fn is None:
        probe_fn = _probe_backend_installed
    coder_family_value = coder_family if coder_family is not None else _coder_family(coder_backend)
    return _resolve_with_chain(chain, coder_family_value, probe_fn)


def aggregate_cross_model_verdict(
    text: str,
    *,
    gate_strict: bool = False,
) -> CrossModelVerdict:
    """Wrap the canonical ``_parse_verdict`` so cross-model review shares
    verdict semantics with ``gate_es`` / ``gate_skeptic``.

    Verdict parsing is a contract — the cross-model reviewer MUST NOT
    invent its own token map. Routing through the shared parser keeps
    `verdict: pass` → success, `verdict: warn` (strict) → failure,
    marker-but-invalid → ``unknown`` (fail closed), etc.
    """
    raw, normalized = _parse_verdict(text, gate_strict=gate_strict)
    return CrossModelVerdict(
        raw_verdict=raw,
        normalized_outcome=normalized,
        parsed_text=text or "",
    )


def build_metadata(
    *,
    resolution: CrossModelResolution,
    verdict: CrossModelVerdict,
) -> dict[str, str]:
    """Build ``Result.metadata`` for the cross-model gate step.

    Strict-merge policy #328 reads ``cross_model_degraded`` and refuses
    strict-green when it equals ``"true"``. Operator/CXDB can read
    ``cross_model_vendor`` / ``cross_model_family`` /
    ``cross_model_skipped`` to see which vendor actually reviewed and
    which entries were skipped.
    """
    return {
        "cross_model_vendor": resolution.vendor or "(none)",
        "cross_model_family": resolution.family or "(none)",
        "cross_model_skipped": ",".join(resolution.skipped),
        "cross_model_degraded": "true" if resolution.degraded else "false",
        "cross_model_verdict": verdict.raw_verdict or "unknown",
        "cross_model_normalized_outcome": verdict.normalized_outcome,
    }
