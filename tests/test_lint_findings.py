"""Tests for the F5 lint_findings substitution (jleechan-zba).

Closes the F5 gap surfaced by the 2026-06-22 /factory-evolve proposals:
the reviewer prompt sees `${diff}` only, so cold reviewers catch
patterns the engine could have detected but didn't
(`datetime.utcnow()`, zizmor template-injection, iOS `video=` config).
This module is the engine side of the fix.

Three layers covered:
  1. `runner/pre_review_lint.py::lint_findings` — pattern scanner.
  2. `findings_to_markdown` — deterministic renderer for prompt injection.
  3. `runner/handler_render.py::_substitute_placeholders` — the
     `${lint_findings}` substitution + caching.

Adding a new lint pattern = add to `_LINT_PATTERNS` in
`runner/pre_review_lint.py` + a test in this module. No prompt or
runner change required (the substitution is data-driven).
"""
from __future__ import annotations

import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

# Load the shim first so the `runner.handler_render ↔ runner.handlers`
# circular import is broken in the right order. Without this, a
# `from runner.handler_render import ...` triggers a partially-initialized
# module error because the shim imports `_render_prompt` from handler_render
# before handler_render's body has finished executing.
import runner.handlers  # noqa: E402,F401


# ---------------------------------------------------------------------------
# Layer 1: pattern scanner
# ---------------------------------------------------------------------------


def test_lint_findings_returns_empty_for_missing_workdir(tmp_path):
    """A non-existent workdir must return [] (no crash, no traceback)."""
    from runner.pre_review_lint import lint_findings

    bogus = tmp_path / "does-not-exist"
    assert lint_findings(bogus) == []


def test_lint_findings_flags_datetime_utcnow(tmp_path):
    """The py_datetime_utcnow pattern fires on `datetime.utcnow()`."""
    from runner.pre_review_lint import lint_findings

    (tmp_path / "module.py").write_text(
        "from datetime import datetime\n"
        "def now():\n"
        "    return datetime.utcnow()\n"
    )
    findings = lint_findings(tmp_path)
    matches = [f for f in findings if f["pattern_id"] == "py_datetime_utcnow"]
    assert len(matches) == 1, findings
    assert matches[0]["severity"] == "fail"
    assert matches[0]["line"] == 3
    assert "datetime.utcnow" in matches[0]["snippet"]
    assert "datetime.now(timezone.utc)" in matches[0]["rationale"]


def test_lint_findings_flags_zizmor_template_injection(tmp_path):
    """The gh_zizmor_template pattern fires on `${{{ var }}}` in yml files."""
    from runner.pre_review_lint import lint_findings

    (tmp_path / ".github").mkdir()
    (tmp_path / ".github" / "workflow.yml").write_text(
        "name: ci\n"
        "on: push\n"
        "jobs:\n"
        "  build:\n"
        "    run: |\n"
        "      echo \"${{ github.event.pull_request.title }}\"\n"
    )
    findings = lint_findings(tmp_path)
    matches = [f for f in findings if f["pattern_id"] == "gh_zizmor_template"]
    assert len(matches) == 1, findings
    assert matches[0]["severity"] == "fail"
    # Line number is the 0-indexed offset of the line where `${{ ` first
    # appears — depends on the test fixture's exact prefix. We assert ≥ 1.
    assert matches[0]["line"] >= 1
    assert "template-injection" in matches[0]["rationale"]


def test_lint_findings_flags_ios_video_config(tmp_path):
    """The ios_video_config pattern fires on `video=` in iOS simctl paths (warn).

    The simctl recorder config is an INI-style `.json` blob with `video=`
    codec flags. The path-gate (jleechan-fpi, audit-2026-06-27 Lane F) restricts
    the pattern to files whose path contains `ios` or `simctl`. We put the
    fixture inside an `ios/` subdir to satisfy that gate."""
    from runner.pre_review_lint import lint_findings

    ios_dir = tmp_path / "ios" / "simctl"
    ios_dir.mkdir(parents=True)
    (ios_dir / "sim_config.json").write_text(
        "video=h264\n"
        "mask=alpha\n"
    )
    findings = lint_findings(tmp_path)
    matches = [f for f in findings if f["pattern_id"] == "ios_video_config"]
    assert len(matches) == 1, findings
    assert matches[0]["severity"] == "warn"
    assert matches[0]["line"] == 1


def test_lint_findings_ios_video_config_does_not_fire_globally(tmp_path):
    """Regression for jleechan-fpi: ios_video_config must NOT fire globally.

    A fibonacci-style worktree with a stray `video=1280` in a generic
    JSON config (e.g. `fibonacci.json` at the workdir root, no `ios` or
    `simctl` anywhere in the path) must not produce an ios_video_config
    finding — the pattern leaks the iOS benchmark shape into every
    reviewer's prompt if gated only by the json glob."""
    from runner.pre_review_lint import lint_findings

    # Simulate a fibonacci worktree root with a generic JSON config.
    fib_root = tmp_path / "fibonacci-bench-2026-06-26"
    fib_root.mkdir()
    (fib_root / "fibonacci.json").write_text(
        "{\n"
        "  \"max_value\": 1280,\n"
        "  \"video\": \"tutorial-recording-not-applicable\",\n"
        "  \"codec\": \"h264\"\n"
        "}\n"
    )
    # Also throw in an unrelated python file to ensure the global patterns
    # still work — the test is that ONLY ios_video_config is gated, not the
    # whole scanner.
    (fib_root / "module.py").write_text(
        "from datetime import datetime\n"
        "def now():\n"
        "    return datetime.utcnow()\n"
    )

    findings = lint_findings(fib_root)
    ios_matches = [f for f in findings if f["pattern_id"] == "ios_video_config"]
    assert ios_matches == [], (
        "ios_video_config leaked into non-iOS workdir: %r" % ios_matches
    )
    # Sanity: the global py_datetime_utcnow still fires — the gate only
    # restricts ios_video_config, not the whole scanner.
    py_matches = [f for f in findings if f["pattern_id"] == "py_datetime_utcnow"]
    assert len(py_matches) == 1, findings


def test_lint_findings_ios_video_config_fires_when_workdir_named_ios(tmp_path):
    """The path-gate also accepts the workdir name itself matching ios/simctl.

    Some users drop the fixture at the workdir root with no `ios/`
    subdirectory; the gate should still fire if the workdir name is, say,
    `ios-simulator-bench` because the absolute path contains `ios`."""
    from runner.pre_review_lint import lint_findings

    ios_root = tmp_path / "ios-simulator-bench"
    ios_root.mkdir()
    (ios_root / "recorder.json").write_text(
        "video=h264\n"
    )
    findings = lint_findings(ios_root)
    matches = [f for f in findings if f["pattern_id"] == "ios_video_config"]
    assert len(matches) == 1, findings


def test_lint_findings_ios_video_config_fires_via_substring_in_subpath(tmp_path):
    """The path-gate matches substrings, not just whole path components.

    A file at `tmp/.../simctl-recorder/cfg.json` should still trigger
    even though the directory name is `simctl-recorder`, not exactly
    `simctl`. This mirrors real-world layouts where the iOS rig lives
    under a `simctl-recorder/` subdir rather than the canonical name."""
    from runner.pre_review_lint import lint_findings

    subdir = tmp_path / "simctl-recorder"
    subdir.mkdir()
    (subdir / "cfg.json").write_text("video=h264\n")
    findings = lint_findings(tmp_path)
    matches = [f for f in findings if f["pattern_id"] == "ios_video_config"]
    assert len(matches) == 1, findings


def test_lint_findings_deduplicates_same_pattern_at_same_line(tmp_path):
    """Two identical hits at the same (file, line) collapse into one finding."""
    from runner.pre_review_lint import lint_findings

    (tmp_path / "dup.py").write_text("datetime.utcnow()  # also datetime.utcnow()\n")
    findings = lint_findings(tmp_path)
    matches = [f for f in findings if f["pattern_id"] == "py_datetime_utcnow"]
    # Two `utcnow()` calls, but `finditer` will report both — deduplication is
    # by (file, line). For this test the snippet is on the same line so the
    # dedup key collapses them. If a future change keeps both, the test
    # documents the current behaviour.
    assert len(matches) == 1, findings


def test_lint_findings_skips_binary_files(tmp_path):
    """Binary files don't crash the scanner and produce no findings."""
    from runner.pre_review_lint import lint_findings

    (tmp_path / "blob.bin").write_bytes(b"\x00\x01\x02datetime.utcnow()\x00")
    findings = lint_findings(tmp_path)
    assert findings == []


# ---------------------------------------------------------------------------
# Layer 2: markdown renderer
# ---------------------------------------------------------------------------


def test_findings_to_markdown_empty_returns_none_marker():
    """No findings → `(none)` so the reviewer prompt never gets an empty block."""
    from runner.pre_review_lint import findings_to_markdown

    out = findings_to_markdown([])
    assert "## Lint findings" in out
    assert "(none)" in out


def test_findings_to_markdown_includes_severity_and_location():
    """Markdown rows include pattern_id, severity, file:line, snippet, rationale."""
    from runner.pre_review_lint import findings_to_markdown

    findings = [{
        "pattern_id": "py_datetime_utcnow",
        "severity": "fail",
        "file": "runner/foo.py",
        "line": 42,
        "snippet": "datetime.utcnow()",
        "rationale": "use datetime.now(timezone.utc)",
    }]
    out = findings_to_markdown(findings)
    assert "| pattern_id | severity | location | snippet | rationale |" in out
    assert "py_datetime_utcnow" in out
    assert "fail" in out
    assert "runner/foo.py:42" in out
    assert "datetime.utcnow()" in out
    assert "datetime.now(timezone.utc)" in out


def test_findings_to_markdown_is_deterministic():
    """Two equal inputs render byte-identical output (stable column order)."""
    from runner.pre_review_lint import findings_to_markdown

    f = {
        "pattern_id": "p",
        "severity": "fail",
        "file": "a.py",
        "line": 1,
        "snippet": "x",
        "rationale": "r",
    }
    assert findings_to_markdown([f, f]) == findings_to_markdown([f, f])


# ---------------------------------------------------------------------------
# Layer 3: prompt substitution (handler_render)
# ---------------------------------------------------------------------------


def _ctx(tmp_path, **state):
    from runner.handler_core import Context
    return Context(goal="g", workdir=tmp_path, backend="echo", state=dict(state))


def test_lint_findings_substitution_replaces_placeholder(tmp_path):
    """`${lint_findings}` in a template resolves to the Markdown table."""
    from runner.handler_render import _substitute_placeholders

    (tmp_path / "bad.py").write_text("datetime.utcnow()\n")
    ctx = _ctx(tmp_path)
    out = _substitute_placeholders("FINDINGS:\n${lint_findings}\nEND\n", ctx)
    assert "FINDINGS:\n" in out
    assert "END\n" in out
    assert "py_datetime_utcnow" in out
    assert "runner/bad.py" in out or "bad.py" in out


def test_lint_findings_substitution_caches_in_state(tmp_path):
    """A second `${lint_findings}` in the same render does not re-scan."""
    from runner.handler_render import _substitute_placeholders

    (tmp_path / "bad.py").write_text("datetime.utcnow()\n")
    ctx = _ctx(tmp_path)
    out = _substitute_placeholders(
        "A:\n${lint_findings}\nB:\n${lint_findings}\n", ctx
    )
    # Cache key present.
    assert "_lint_findings" in ctx.state
    cached = ctx.state["_lint_findings"]
    assert isinstance(cached, str)
    # Cached as JSON; re-parsing gives the same findings as a fresh render.
    parsed = json.loads(cached)
    assert any(f["pattern_id"] == "py_datetime_utcnow" for f in parsed)
    # The placeholder was replaced in both A: and B: blocks.
    a_idx = out.index("A:\n") + 3
    b_idx = out.index("B:\n") + 3
    assert "py_datetime_utcnow" in out[a_idx:b_idx]
    assert "py_datetime_utcnow" in out[b_idx:]


def test_lint_findings_substitution_absent_when_not_referenced(tmp_path):
    """A prompt without `${lint_findings}` does NOT trigger a workdir scan."""
    from runner.handler_render import _substitute_placeholders

    # Plant a file that WOULD match if scanned.
    (tmp_path / "bad.py").write_text("datetime.utcnow()\n")
    ctx = _ctx(tmp_path)
    out = _substitute_placeholders("Goal:\n${goal}\nDiff:\n${diff}\n", ctx)
    assert "py_datetime_utcnow" not in out
    # Cache key must NOT exist — we never scanned.
    assert "_lint_findings" not in ctx.state


def test_lint_findings_default_when_no_matches(tmp_path):
    """No findings → placeholder resolves to the `(none)` Markdown block."""
    from runner.handler_render import _substitute_placeholders

    ctx = _ctx(tmp_path)
    out = _substitute_placeholders("${lint_findings}", ctx)
    assert "(none)" in out
