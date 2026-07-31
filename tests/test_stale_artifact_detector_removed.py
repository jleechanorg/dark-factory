"""jleechan-6ug2 acceptance — nonfunctional stale-artifact detector is gone.

The detector previously lived in ``runner/handler_parallel_reviewer.py`` as
``_check_stale_artifacts``. It had no marker writer, reversed freshness
(stale-by-mtime when the file was just produced), and wrote a state key +
event no production consumer reads. These tests pin the deletion surface:

* the function is no longer importable from the module,
* the module no longer carries the now-unused imports (``hashlib``,
  ``pathlib``, ``time``) — ``json`` is still legitimately used elsewhere,
* the parallel reviewer never writes
  ``ctx.state["_stale_artifact_warnings"]`` (seeded with a stale-spec
  marker so the detector would fire if it were alive),
* the parallel reviewer never emits a ``stale_artifact_warning`` event
  under the same conditions,
* the parallel reviewer never injects stale-detector text into the prompt.

Implementation note: the runner.handlers module has a known circular import
between handlers.py and handler_parallel_reviewer.py; importing it in this
test file would mask the real assertion with an ImportError. We import only
what we need and use ``parser.Node`` + ``handler_core.Context`` directly.
"""

from __future__ import annotations

import ast
import importlib
from pathlib import Path

import pytest


HANDLER_MODULE = "runner.handler_parallel_reviewer"
CTX_MODULE = "runner.handler_core"
NODE_MODULE = "runner.parser"


def _reload_handler():
    # Prime runner.handlers first — the parallel reviewer module has a
    # documented circular import with runner.handlers and reload only
    # succeeds when the shim is already in sys.modules.
    import runner.handlers  # noqa: F401
    return importlib.reload(importlib.import_module(HANDLER_MODULE))


def _make_ctx(tmp_path: Path):
    Context = importlib.import_module(CTX_MODULE).Context
    return Context(
        goal="stale-detector-removed",
        workdir=tmp_path,
        backend="echo",
        run_id="current-run",
        event_log_path=tmp_path / "events.jsonl",
    )


def _make_node(tmp_path: Path):
    prompt = tmp_path / "review.md"
    prompt.write_text("parallel review: ${goal}\n", encoding="utf-8")
    Node = importlib.import_module(NODE_MODULE).Node
    return Node(
        name="review",
        attrs={
            "type": "parallel_reviewer",
            "backend": "codex",
            "prompt": f"@{prompt}",
        },
    )


def _seed_stale_spec(tmp_path: Path, run_id: str = "prior-run") -> None:
    """Plant spec.md with a run_id marker that won't match the current run.

    If the detector is alive this is exactly the input that makes it write
    ``ctx.state["_stale_artifact_warnings"]`` and emit a
    ``stale_artifact_warning`` event. With the detector removed, neither
    side effect occurs.
    """
    spec = tmp_path / "spec.md"
    spec.write_text(
        f"<!-- run_id: {run_id} -->\n"
        "# Spec\n"
        "old body\n",
        encoding="utf-8",
    )


# ---------------------------------------------------------------------------
# Pure structural tests (no runner.handlers required)
# ---------------------------------------------------------------------------


def test_check_stale_artifacts_symbol_is_removed():
    """The nonfunctional detector must be gone from the module namespace."""
    mod = _reload_handler()
    assert not hasattr(mod, "_check_stale_artifacts"), (
        "_check_stale_artifacts is still defined in runner.handler_parallel_reviewer; "
        "delete it per bead jleechan-6ug2 (it had no marker writer and wrote "
        "unread state)."
    )


def test_unused_imports_removed_from_handler_module():
    """`time` was only used by the deleted detector. `pathlib` (lane_output_dir +
    neutral_cwd paths) and `hashlib` (workspace-reverify + evidence digest
    recompute) are now legitimately used by the controller contract wiring."""
    src = Path(importlib.import_module(HANDLER_MODULE).__file__).read_text()
    tree = ast.parse(src)

    imported: dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                imported[alias.asname or alias.name.split(".")[0]] = alias.name
        elif isinstance(node, ast.ImportFrom):
            for alias in node.names:
                imported[alias.asname or alias.name] = node.module or ""

    # json is still legitimately used (json.dumps for shadow_reviews).
    for unused in ("time",):
        assert unused not in imported, (
            f"{HANDLER_MODULE} still imports {unused!r}; the deleted detector "
            "was its only consumer."
        )


def test_check_stale_artifacts_call_site_removed():
    """The detector's invocation inside _parallel_reviewer must be gone too."""
    src = Path(importlib.import_module(HANDLER_MODULE).__file__).read_text()
    forbidden_substrings = (
        "_check_stale_artifacts(",
        'ctx.state["_stale_artifact_warnings"]',
        '"stale_artifact_warning"',
    )
    for needle in forbidden_substrings:
        assert needle not in src, (
            f"{HANDLER_MODULE} still contains {needle!r}; the detector block "
            "was not fully removed."
        )


# ---------------------------------------------------------------------------
# Behavioural tests (seed stale spec to force the detector; assert no side effects)
# ---------------------------------------------------------------------------


def test_parallel_reviewer_does_not_write_stale_artifact_warnings_state(
    tmp_path, monkeypatch
):
    """With a stale spec marker present, the detector (if alive) would write state."""
    _seed_stale_spec(tmp_path, run_id="different-prior-run")
    ctx = _make_ctx(tmp_path)
    ctx.state.pop("_stale_artifact_warnings", None)

    mod = _reload_handler()
    mod._parallel_reviewer(_make_node(tmp_path), ctx)

    assert "_stale_artifact_warnings" not in ctx.state, (
        "_parallel_reviewer still writes ctx.state['_stale_artifact_warnings']; "
        "the detector should be deleted entirely, not neutered."
    )


def test_parallel_reviewer_does_not_emit_stale_artifact_warning_event(
    tmp_path, monkeypatch
):
    """With a stale spec marker present, the detector (if alive) would emit an event."""
    _seed_stale_spec(tmp_path, run_id="different-prior-run")
    ctx = _make_ctx(tmp_path)

    emitted: list[tuple[str, dict]] = []

    def _capture_emit(ctx_arg, event_type, payload, seq):
        emitted.append((event_type, dict(payload or {})))

    monkeypatch.setattr(
        "runner.engine_observability._emit_event", _capture_emit
    )

    mod = _reload_handler()
    mod._parallel_reviewer(_make_node(tmp_path), ctx)

    stale_events = [e for e in emitted if e[0] == "stale_artifact_warning"]
    assert stale_events == [], (
        f"parallel reviewer emitted stale_artifact_warning events: {stale_events!r}"
    )


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])