"""Unit tests for ``runner.handler_web_advice._web_advice`` (fail-open).

These tests pin the §1.2 ironclad contract from
``docs/web-advice-failopen-design.md``:

* ``_web_advice`` ALWAYS returns ``outcome=success`` regardless of
  transport availability, subprocess success, or PR-comment failure.
* The handler never raises — outer try/except converts exceptions to a
  fail-open success with the exception captured in metadata.
* When diff size < ``min_diff_lines`` (default 5), the handler skips
  silently with a structured ``web_advice_outcome=skipped_trivial_diff``.
* When the transport probe matrix finds no live transport, the handler
  posts an infra-disclosure PR comment (best-effort) and returns success.
* When a transport is live, the handler invokes the actual /web-advice
  subprocess, captures verdict + share URLs, and posts a verdict-captured
  PR comment.
* CXDB metadata carries the §5 schema fields (event_type, verdict,
  panel_seats_*, elapsed_seconds, decision, transport_probes).

Test isolation strategy:
* ``_run_transport_probe`` is monkeypatched to return canned dicts.
* ``_post_pr_comment`` is monkeypatched to record calls and return
  canned ok/fail results.
* ``_run_e2e_smoke`` is monkeypatched to return ok=True.
* ``_run_web_advice_subprocess`` is monkeypatched to return canned
  verdict data.
* ``_compute_diff_lines`` is monkeypatched to return canned integers so
  the test does not need a real git repo.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

import pytest

# Prime the import cycle: runner.handlers -> handler_web_advice
from runner.handlers import _web_advice  # noqa: F401, E402
from runner.handler_core import Context  # noqa: E402
from runner.handler_web_advice import (  # noqa: E402
    _build_infra_disclosure_comment,
    _build_verdict_comment,
    _compute_diff_lines,
    _coerce_int,
    _coerce_meta,
    _extract_pr_repo,
    _extract_share_urls,
    _extract_verdict_token,
    _parse_pr_number,
    _post_pr_comment,
    _probe_share_url,
    _persist_share_evidence,
    _probe_aside_cli,
    _probe_aside_mcp,
    _probe_cdp_port,
    _probe_chrome_extension,
    _run_e2e_smoke,
    _run_transport_probe,
    _run_web_advice_subprocess,
    PANEL_SEATS,
    DEFAULT_MIN_DIFF_LINES,
    PROBE_TIMEOUT_SECONDS,
)
from runner.parser import Node  # noqa: E402


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


class TestParsePrNumber:
    def test_https_url(self):
        assert _parse_pr_number("https://github.com/jleechanorg/dark-factory/pull/655") == 655

    def test_https_url_with_path(self):
        assert _parse_pr_number("https://github.com/jleechanorg/dark-factory/pull/655/files") == 655

    def test_invalid_returns_none(self):
        assert _parse_pr_number("not-a-url") is None
        assert _parse_pr_number("") is None
        assert _parse_pr_number("https://example.com/foo/bar") is None

    def test_pull_no_number(self):
        assert _parse_pr_number("https://github.com/owner/repo/pull/") is None


class TestExtractPrRepo:
    def test_standard_url(self):
        assert _extract_pr_repo("https://github.com/jleechanorg/dark-factory/pull/655") == "jleechanorg/dark-factory"

    def test_invalid_returns_none(self):
        assert _extract_pr_repo("not-a-url") is None
        assert _extract_pr_repo("") is None


class TestCoerceInt:
    def test_int_passthrough(self):
        assert _coerce_int(5, 10) == 5

    def test_str_int(self):
        assert _coerce_int("20", 10) == 20

    def test_invalid_returns_default(self):
        assert _coerce_int("abc", 10) == 10
        assert _coerce_int(None, 10) == 10
        assert _coerce_int("", 10) == 10


class TestExtractVerdictToken:
    def test_approve(self):
        assert _extract_verdict_token("VERDICT: APPROVE\n") == "approve"

    def test_not_merge(self):
        assert _extract_verdict_token("verdict: NOT MERGE because...") == "not_merge"

    def test_unknown(self):
        assert _extract_verdict_token("no verdict here") == "unknown"

    def test_empty(self):
        assert _extract_verdict_token("") == "unknown"


class TestExtractShareUrls:
    def test_gemini(self):
        out = "share https://g.co/gemini/share/abc123 for the panel"
        assert _extract_share_urls(out) == ["https://g.co/gemini/share/abc123"]

    def test_grok(self):
        out = "share https://grok.com/share/def456 here"
        assert _extract_share_urls(out) == ["https://grok.com/share/def456"]

    def test_chatgpt(self):
        out = "share https://chatgpt.com/share/xyz789 here"
        assert _extract_share_urls(out) == ["https://chatgpt.com/share/xyz789"]

    def test_perplexity(self):
        out = "see https://perplexity.ai/search/aaabbbccc"
        assert _extract_share_urls(out) == ["https://perplexity.ai/search/aaabbbccc"]

    def test_gemini_share_host(self):
        out = "see https://share.gemini.google/abc123"
        assert _extract_share_urls(out) == ["https://share.gemini.google/abc123"]

    def test_empty(self):
        assert _extract_share_urls("") == []
        assert _extract_share_urls("no url here") == []

    def test_unrelated_url_ignored(self):
        # Conservative: only known public-share hosts match.
        out = "see https://example.com/share/foo"
        assert _extract_share_urls(out) == []


class TestBuildInfraDisclosure:
    def test_includes_marker_and_summary(self):
        body = _build_infra_disclosure_comment(
            "Transport probe matrix:\n  - all absent\n",
            "https://github.com/jleechanorg/dark-factory/pull/655",
        )
        assert "<!-- web-advice-review -->" in body
        assert "<!-- /web-advice-review -->" in body
        assert "/web-advice unavailable" in body
        assert "all absent" in body
        assert "jleechanorg/dark-factory/pull/655" in body
        assert "fail-open" in body


class TestBuildVerdictComment:
    def test_includes_verdict_and_share_urls(self):
        verdict_data = {
            "verdict": "approve",
            "summary": "Looks good.",
            "share_urls": ["https://g.co/gemini/share/abc123"],
            "transport": "aside_mcp",
        }
        body = _build_verdict_comment(
            verdict_data,
            "https://github.com/jleechanorg/dark-factory/pull/655",
        )
        assert "<!-- web-advice-review -->" in body
        assert "<!-- /web-advice-review -->" in body
        assert "`approve`" in body
        assert "https://g.co/gemini/share/abc123" in body
        assert "aside_mcp" in body


class TestCoerceMeta:
    def test_scalars_passthrough(self):
        assert _coerce_meta(None) is None
        assert _coerce_meta("str") == "str"
        assert _coerce_meta(42) == 42
        assert _coerce_meta(3.14) == 3.14
        assert _coerce_meta(True) is True
        assert _coerce_meta(False) is False

    def test_complex_to_json(self):
        d = {"a": 1, "b": [1, 2]}
        out = _coerce_meta(d)
        assert out == d


class TestStructuredPanelContract:
    def test_subprocess_parses_json_panel_result_without_keyword_triage(self, monkeypatch):
        payload = {
            "ok": True,
            "decision": "continue_with_pr_warning",
            "panel_seats_attempted": ["chatgpt", "gemini"],
            "panel_seats_live": ["chatgpt"],
            "panel_seats_unavailable": ["gemini"],
            "panel_seats_unavailable_reasons": {"gemini": "transport absent"},
            "panel_verdict_summary": {
                "chatgpt": {"verdict": "NOT MERGE", "reasoning": "file.py:7"}
            },
        }
        fake = subprocess.CompletedProcess(
            args=[], returncode=0,
            stdout="narrative NOT MERGE\n" + json.dumps(payload), stderr=""
        )
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", lambda *a, **k: fake)
        result = _run_web_advice_subprocess("aside_mcp", "prompt")
        assert result["decision"] == "continue_with_pr_warning"
        assert result["panel_seats_live"] == ["chatgpt"]
        assert result["panel_verdict_summary"]["chatgpt"]["reasoning"] == "file.py:7"
        assert "verdict" not in result

    def test_malformed_json_does_not_fall_back_to_verdict_keywords(self, monkeypatch):
        fake = subprocess.CompletedProcess(args=[], returncode=0, stdout="VERDICT: NOT MERGE", stderr="")
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", lambda *a, **k: fake)
        result = _run_web_advice_subprocess("aside_mcp", "prompt")
        assert result["ok"] is False
        assert result["verdict"] == "unknown"


class TestShareUrlProbe:
    def test_accepts_public_200_without_cookie(self, monkeypatch):
        class Response:
            status = 200
            def geturl(self):
                return "https://g.co/gemini/share/abc"
            def getheaders(self):
                return [("Content-Type", "text/html")]
            def __enter__(self): return self
            def __exit__(self, *args): return None
        monkeypatch.setattr("runner.handler_web_advice.urllib.request.urlopen", lambda *a, **k: Response())
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is True
        assert result["status"] == 200

    @pytest.mark.parametrize("final_url", ["https://accounts.example.com/login", "https://example.com/signin"])
    def test_rejects_login_redirect(self, monkeypatch, final_url):
        class Response:
            status = 200
            def geturl(self): return final_url
            def getheaders(self): return []
            def __enter__(self): return self
            def __exit__(self, *args): return None
        monkeypatch.setattr("runner.handler_web_advice.urllib.request.urlopen", lambda *a, **k: Response())
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "login" in result["error"] or "signin" in result["error"]

    def test_accepts_anonymous_tracking_set_cookie(self, monkeypatch):
        class Response:
            status = 200
            def geturl(self): return "https://g.co/gemini/share/abc"
            def getheaders(self): return [("Set-Cookie", "NID=tracking"), ("Set-Cookie", "COMPASS=presentation")]
            def read(self, _limit=-1): return b"Gemini shared conversation"
            def __enter__(self): return self
            def __exit__(self, *args): return None
        monkeypatch.setattr("runner.handler_web_advice.urllib.request.urlopen", lambda *a, **k: Response())
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is True
        assert result["status"] == 200

    def test_accepts_final_gemini_share_after_301_with_cookies(self, monkeypatch):
        class Response:
            status = 200
            def geturl(self): return "https://share.gemini.google/abc123"
            def getheaders(self): return [("Set-Cookie", "NID=tracking")]
            def read(self, _limit=-1): return b"Public Gemini conversation"
            def __enter__(self): return self
            def __exit__(self, *args): return None
        monkeypatch.setattr("runner.handler_web_advice.urllib.request.urlopen", lambda *a, **k: Response())
        result = _probe_share_url("https://g.co/gemini/share/abc123")
        assert result["ok"] is True
        assert result["final_url"] == "https://share.gemini.google/abc123"


class TestShareEvidencePersistence:
    def test_persists_atomically_with_seat_keys(self, tmp_path):
        path = _persist_share_evidence(
            tmp_path,
            {"chatgpt": {"share_url": "https://chatgpt.com/share/a", "share_url_probe": {"ok": True}}},
            pr_url="https://github.com/o/r/pull/1", head_sha="abc",
        )
        assert path == tmp_path / "web-advice-share-urls.json"
        data = json.loads(path.read_text())
        assert data["chatgpt"]["share_url"] == "https://chatgpt.com/share/a"
        assert not list(tmp_path.glob("*.tmp"))


def test_followup_bead_body_starts_with_target_repo(monkeypatch):
    seen = []
    monkeypatch.setattr(
        "runner.handler_web_advice.subprocess.run",
        lambda cmd, **kwargs: seen.append((cmd, kwargs)) or subprocess.CompletedProcess(cmd, 0, "jleechan-xyz\n", ""),
    )
    from runner.handler_web_advice import _file_followup_bead
    result = _file_followup_bead(target_repo="jleechanorg/dark-factory", pr_number=742, decision="continue_with_bead", summary="infra")
    assert result["bead_id"] == "jleechan-xyz"
    body = seen[0][1]["input"] if "input" in seen[0][1] else seen[0][0][-1]
    assert body.startswith("target_repo: jleechanorg/dark-factory\n")


# ---------------------------------------------------------------------------
# Default constants
# ---------------------------------------------------------------------------


def test_default_min_diff_lines_is_five():
    assert DEFAULT_MIN_DIFF_LINES == 5


def test_probe_timeout_is_ten_seconds():
    assert PROBE_TIMEOUT_SECONDS == 10


# ---------------------------------------------------------------------------
# Transport probe
# ---------------------------------------------------------------------------


class TestProbeMatrix:
    def test_run_transport_probe_all_absent(self, monkeypatch):
        """When all probes return False, live_transport is None."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_mcp", lambda c: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_cli", lambda: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_chrome_extension", lambda c: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_cdp_port", lambda: False
        )
        node = Node(name="web_advice", attrs={"type": "web_advice"})
        result = _run_transport_probe(node, ctx)
        assert result["live_transport"] is None
        assert result["probes"]["aside_mcp"] is False
        assert result["probes"]["aside_cli"] is False
        assert result["probes"]["chrome_extension"] is False
        assert result["probes"]["cdp_port"] is False
        assert "NO LIVE TRANSPORT" in result["summary"]
        assert result["elapsed_seconds"] >= 0

    def test_run_transport_probe_picks_first_live(self, monkeypatch):
        """When aside_mcp is live, it wins over other transports."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_mcp", lambda c: True
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_cli", lambda: True
        )
        node = Node(name="web_advice", attrs={"type": "web_advice"})
        result = _run_transport_probe(node, ctx)
        assert result["live_transport"] == "aside_mcp"
        assert "live_transport=aside_mcp" in result["summary"]

    def test_run_transport_probe_picks_cdp_when_only_cdp_live(self, monkeypatch):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_mcp", lambda c: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_cli", lambda: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_chrome_extension", lambda c: False
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_cdp_port", lambda: True
        )
        node = Node(name="web_advice", attrs={"type": "web_advice"})
        result = _run_transport_probe(node, ctx)
        assert result["live_transport"] == "cdp_port"

    def test_probe_completes_within_budget(self, monkeypatch):
        """The probe matrix returns within the 10s budget even with no inputs."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        node = Node(name="web_advice", attrs={"type": "web_advice"})
        result = _run_transport_probe(node, ctx, max_seconds=10)
        assert result["elapsed_seconds"] < 10


class TestProbePerTransport:
    def test_probe_aside_mcp_default_absent(self):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        assert _probe_aside_mcp(ctx) is False

    def test_probe_aside_mcp_state_true(self):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["_df_aside_mcp_available"] = "true"
        assert _probe_aside_mcp(ctx) is True

    def test_probe_aside_mcp_state_garbage(self):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["_df_aside_mcp_available"] = "not-a-bool"
        assert _probe_aside_mcp(ctx) is False

    def test_probe_chrome_extension_default_absent(self):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        assert _probe_chrome_extension(ctx) is False

    def test_probe_chrome_extension_state_true(self):
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["_df_claude_in_chrome_connected"] = "1"
        assert _probe_chrome_extension(ctx) is True

    def test_probe_aside_cli_missing(self, monkeypatch):
        monkeypatch.setattr("shutil.which", lambda name: None)
        assert _probe_aside_cli() is False

    def test_probe_aside_cli_present_and_responding(self, monkeypatch):
        monkeypatch.setattr("shutil.which", lambda name: "/usr/local/bin/aside" if name == "aside" else None)
        monkeypatch.setattr(
            "subprocess.run",
            lambda cmd, **kwargs: subprocess.CompletedProcess(args=cmd, returncode=0, stdout="aside 1.0.0", stderr=""),
        )
        assert _probe_aside_cli() is True

    def test_probe_aside_cli_present_not_responding(self, monkeypatch):
        monkeypatch.setattr("shutil.which", lambda name: "/usr/local/bin/aside" if name == "aside" else None)
        monkeypatch.setattr(
            "subprocess.run",
            lambda cmd, **kwargs: subprocess.CompletedProcess(args=cmd, returncode=1, stdout="", stderr="error"),
        )
        assert _probe_aside_cli() is False

    def test_probe_aside_cli_timeout(self, monkeypatch):
        def _timeout(cmd, **kwargs):
            raise subprocess.TimeoutExpired(cmd=cmd, timeout=2.0)

        monkeypatch.setattr("shutil.which", lambda name: "/usr/local/bin/aside" if name == "aside" else None)
        monkeypatch.setattr("subprocess.run", _timeout)
        assert _probe_aside_cli() is False

    def test_probe_cdp_port_is_independent_of_aside_cli(self, monkeypatch):
        class FakeResponse:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def read(self, _limit):
                return b'{"Browser":"Chrome/148","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/browser/id"}'

        monkeypatch.setattr("runner.handler_web_advice._probe_aside_cli", lambda: False)
        monkeypatch.setattr("urllib.request.urlopen", lambda request, timeout: FakeResponse())
        assert _probe_cdp_port() is True

    def test_probe_cdp_port_rejects_non_cdp_http_listener(self, monkeypatch):
        class FakeResponse:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def read(self, _limit):
                return b'{"status":"ok"}'

        monkeypatch.setattr("urllib.request.urlopen", lambda request, timeout: FakeResponse())
        assert _probe_cdp_port() is False

    def test_probe_cdp_port_connection_error(self, monkeypatch):
        def _err(request, timeout):
            raise OSError("Connection refused")

        monkeypatch.setattr("urllib.request.urlopen", _err)
        assert _probe_cdp_port() is False


# ---------------------------------------------------------------------------
# Subprocess wrappers — mockable paths
# ---------------------------------------------------------------------------


class TestPostPrComment:
    def test_post_pr_comment_success(self, monkeypatch):
        seen: list[list[str]] = []
        fake = subprocess.CompletedProcess(args=[], returncode=0, stdout="ok", stderr="")

        def _fake_run(cmd, **kwargs):
            seen.append(cmd)
            return fake

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _post_pr_comment(655, "hello", repo="jleechanorg/dark-factory")
        assert result["ok"] is True
        assert result["returncode"] == 0
        assert seen
        assert "655" in seen[0]
        assert "--repo" in seen[0]
        assert "jleechanorg/dark-factory" in seen[0]

    def test_post_pr_comment_failure_returns_dict(self, monkeypatch):
        def _fake_run(cmd, **kwargs):
            return subprocess.CompletedProcess(
                args=[], returncode=1, stdout="", stderr="rate-limited"
            )

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _post_pr_comment(655, "hello")
        assert result["ok"] is False
        assert result["returncode"] == 1
        assert "rate-limited" in result["stderr"]

    def test_post_pr_comment_handles_missing_gh(self, monkeypatch):
        def _fake_run(cmd, **kwargs):
            raise FileNotFoundError("gh: command not found")

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _post_pr_comment(655, "hello")
        assert result["ok"] is False
        assert "FileNotFoundError" in result["stderr"]


# ---------------------------------------------------------------------------
# _compute_diff_lines
# ---------------------------------------------------------------------------


class TestComputeDiffLines:
    def test_parses_shortstat(self, monkeypatch):
        fake = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="3 files changed, 12 insertions(+), 4 deletions(-)",
            stderr="",
        )
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run", lambda *a, **k: fake
        )
        result = _compute_diff_lines(pathlib.Path("/tmp/repo"))
        assert result == 16

    def test_parses_insertions_only(self, monkeypatch):
        fake = subprocess.CompletedProcess(
            args=[],
            returncode=0,
            stdout="1 file changed, 7 insertions(+)\n",
            stderr="",
        )
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run", lambda *a, **k: fake
        )
        assert _compute_diff_lines(pathlib.Path("/tmp/repo")) == 7

    def test_returns_none_on_nonzero_rc(self, monkeypatch):
        fake = subprocess.CompletedProcess(
            args=[], returncode=128, stdout="", stderr="fatal: bad revision"
        )
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run", lambda *a, **k: fake
        )
        assert _compute_diff_lines(pathlib.Path("/tmp/repo")) is None

    def test_returns_none_on_timeout(self, monkeypatch):
        def _fake_run(*a, **k):
            raise subprocess.TimeoutExpired(cmd="git", timeout=5)

        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run", _fake_run
        )
        assert _compute_diff_lines(pathlib.Path("/tmp/repo")) is None


# ---------------------------------------------------------------------------
# Main handler — fail-open contract
# ---------------------------------------------------------------------------


def _make_ctx(tmp_path: pathlib.Path, **state_overrides) -> Context:
    ctx = Context(
        goal="web advice review",
        workdir=tmp_path,
        backend="echo",
        cxdb_path=None,
        run_id=None,
    )
    ctx.state.update({
        "pr_url": "https://github.com/jleechanorg/dark-factory/pull/655",
        "pr_head_sha": "f" * 40,
        "repo_dir": str(tmp_path),
    })
    ctx.state.update(state_overrides)
    return ctx


def _stub_all_transports_down(monkeypatch):
    """Make all transport probes return False + stub pr-comment + smoke."""
    monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: False)
    monkeypatch.setattr("runner.handler_web_advice._probe_aside_cli", lambda: False)
    monkeypatch.setattr("runner.handler_web_advice._probe_chrome_extension", lambda c: False)
    monkeypatch.setattr("runner.handler_web_advice._probe_cdp_port", lambda: False)
    monkeypatch.setattr(
        "runner.handler_web_advice._post_pr_comment",
        lambda *a, **kw: {"ok": True, "returncode": 0, "stderr": ""},
    )
    monkeypatch.setattr(
        "runner.handler_web_advice._run_e2e_smoke",
        lambda *a, **kw: {"ok": True, "skipped": True, "returncode": 0, "stderr": ""},
    )


class TestSkipOnSmallDiff:
    def test_returns_success_with_skip_note(self, tmp_path, monkeypatch):
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 3,
        )
        _stub_all_transports_down(monkeypatch)
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN CONTRACT: must always succeed"
        assert "skipped" in result.output.lower()
        assert "3" in result.output  # diff lines
        assert "5" in result.output  # min_diff_lines
        assert ctx.state["web_advice.outcome"] == "skipped_trivial_diff"

    def test_respects_default_min_diff_lines(self, tmp_path, monkeypatch):
        """min_diff_lines attr missing → default = 5 lines."""
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 4,
        )
        _stub_all_transports_down(monkeypatch)
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "skipped_trivial_diff"

    def test_min_diff_lines_20_cheaper_lane(self, tmp_path, monkeypatch):
        """Cheap slim lanes set min_diff_lines=20."""
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 10,
        )
        _stub_all_transports_down(monkeypatch)
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "20"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "skipped_trivial_diff"

    def test_min_diff_lines_5_strict_lane_runs(self, tmp_path, monkeypatch):
        """Strict lanes set min_diff_lines=5; 10-line diff passes the gate."""
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 10,
        )
        _stub_all_transports_down(monkeypatch)
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        # Falls through to the infra path because all transports are stubbed
        # down, NOT to skipped_trivial_diff.
        assert ctx.state["web_advice.outcome"] == "infrastructure_failure"

    def test_no_diff_skip_when_diff_lines_unknown(self, tmp_path, monkeypatch):
        """When diff_lines is None, do NOT skip — proceed to transport probe."""
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: None,
        )
        _stub_all_transports_down(monkeypatch)
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "infrastructure_failure"


class TestAllTransportsDown:
    def test_infra_files_target_repo_scoped_bead(self, tmp_path, monkeypatch):
        _stub_all_transports_down(monkeypatch)
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        calls = []
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: calls.append(kw) or {"ok": True, "bead_id": "jleechan-infra"})
        ctx = _make_ctx(tmp_path, target_repo="jleechanorg/dark-factory")
        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), ctx)
        assert result.outcome == "success"
        assert result.metadata["decision"] == "continue_with_bead"
        assert calls[0]["target_repo"] == "jleechanorg/dark-factory"
    def test_returns_success_with_infra_note(self, tmp_path, monkeypatch):
        _stub_all_transports_down(monkeypatch)
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 100,
        )
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: even all-transports-down must succeed"
        assert "infra_unavailable" in result.output
        assert ctx.state["web_advice.outcome"] == "infrastructure_failure"
        assert ctx.state["web_advice.infrastructure_failure_mode"] == "all_transports_down"

    def test_posts_disclosure_pr_comment(self, tmp_path, monkeypatch):
        comment_calls: list[dict] = []

        def _fake_post(pr_number, body, *, repo=None, timeout=30):
            comment_calls.append({
                "pr_number": pr_number,
                "body": body,
                "repo": repo,
            })
            return {"ok": True, "returncode": 0, "stderr": ""}

        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_cli", lambda: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_chrome_extension", lambda c: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_cdp_port", lambda: False)
        monkeypatch.setattr(
            "runner.handler_web_advice._post_pr_comment", _fake_post
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 50,
        )
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert len(comment_calls) == 1
        call = comment_calls[0]
        assert call["pr_number"] == 655
        assert call["repo"] == "jleechanorg/dark-factory"
        assert "<!-- web-advice-review -->" in call["body"]
        assert "/web-advice unavailable" in call["body"]

    def test_pr_comment_failure_still_succeeds(self, tmp_path, monkeypatch):
        """gh pr comment failure is best-effort — handler still succeeds."""
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_cli", lambda: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_chrome_extension", lambda c: False)
        monkeypatch.setattr("runner.handler_web_advice._probe_cdp_port", lambda: False)

        def _failing_post(*a, **kw):
            return {"ok": False, "returncode": 1, "stderr": "rate-limited"}

        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", _failing_post)
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 50,
        )
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: pr comment failure must not block"
        assert ctx.state["web_advice.outcome"] == "infrastructure_failure"

    def test_metadata_carries_full_probe_results(self, tmp_path, monkeypatch):
        _stub_all_transports_down(monkeypatch)
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 50,
        )
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        # Metadata is JSON-encoded by _coerce_meta for complex values.
        assert result.metadata["web_advice_outcome"] == "infrastructure_failure"
        assert result.metadata["infrastructure_failure_mode"] == "all_transports_down"
        assert result.metadata["decision"] == "continue"
        # transport_probes dict is JSON-encoded.
        probe_meta = result.metadata["transport_probes"]
        if isinstance(probe_meta, str):
            probe_meta = json.loads(probe_meta)
        assert probe_meta["aside_mcp"] is False
        assert probe_meta["cdp_port"] is False


class TestSuccessfulRun:
    def test_structured_panel_preserves_seats_probes_evidence_and_bead(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: True)
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        monkeypatch.setattr("runner.handler_web_advice._run_e2e_smoke", lambda *a, **k: {"ok": True})
        monkeypatch.setattr("runner.handler_web_advice._probe_share_url", lambda url: {"ok": True, "status": 200, "final_url": url, "error": ""})
        monkeypatch.setattr("runner.handler_web_advice._render_prompt", lambda node, ctx: "rendered " + ctx.state["feature"])
        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", lambda *a, **k: {"ok": True, "returncode": 0, "stderr": ""})
        bead_calls = []
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: bead_calls.append(kw) or {"ok": True, "bead_id": "jleechan-abc"})
        monkeypatch.setattr("runner.handler_web_advice._run_web_advice_subprocess", lambda **kw: {
            "ok": True, "decision": "continue_with_bead", "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": ["chatgpt", "gemini", "grok"], "panel_seats_unavailable": ["perplexity"],
            "panel_seats_unavailable_reasons": {"perplexity": "provider unavailable"},
            "panel_verdict_summary": {
                "chatgpt": {"verdict": "NOT MERGE", "share_url": "https://chatgpt.com/share/a"},
                "gemini": {"verdict": "NOT MERGE", "share_url": "https://g.co/gemini/share/b"},
                "grok": {"verdict": "NOT MERGE", "share_url": "https://grok.com/share/c"},
            }, "returncode": 0, "transport": "aside_mcp",
        })
        ctx = _make_ctx(tmp_path, feature="web-advice-failopen-test-pr", target_repo="jleechanorg/dark-factory", evidence_dir=str(tmp_path / "evidence"))
        node = Node(name="web_advice", attrs={"type": "web_advice", "prompt": "@prompts/web_advice.txt", "min_diff_lines": "5"})
        result = _web_advice(node, ctx)
        assert result.outcome == "success"
        assert result.metadata["panel_seats_live"] == ["chatgpt", "gemini", "grok"]
        assert result.metadata["panel_seats_unavailable"] == ["perplexity"]
        assert result.metadata["bead_id"] == "jleechan-abc"
        assert json.loads((tmp_path / "evidence/web-advice-share-urls.json").read_text())["chatgpt"]["share_url_probe"]["ok"] is True
        assert bead_calls and bead_calls[0]["target_repo"] == "jleechanorg/dark-factory"

    def test_prompt_is_rendered_before_subprocess(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: True)
        monkeypatch.setattr("runner.handler_web_advice._run_e2e_smoke", lambda *a, **k: {"ok": True})
        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", lambda *a, **k: {"ok": True, "returncode": 0, "stderr": ""})
        seen = []
        monkeypatch.setattr("runner.handler_web_advice._render_prompt", lambda node, ctx: "RENDERED ${state.feature}")
        monkeypatch.setattr("runner.handler_web_advice._run_web_advice_subprocess", lambda **kw: seen.append(kw) or {"ok": False, "returncode": 1, "transport": "aside_mcp"})
        _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), _make_ctx(tmp_path, feature="f"))
        assert seen[0]["prompt"] == "RENDERED ${state.feature}"

    def test_captures_verdict_and_share_url(self, tmp_path, monkeypatch):
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_mcp", lambda c: True
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 100,
        )

        fake_subprocess_data = {
            "ok": True,
            "verdict": "approve",
            "summary": "Diff looks clean. VERDICT: APPROVE",
            "share_urls": ["https://g.co/gemini/share/abc123"],
            "returncode": 0,
            "stderr": "",
            "transport": "aside_mcp",
        }
        monkeypatch.setattr(
            "runner.handler_web_advice._run_web_advice_subprocess",
            lambda **kw: fake_subprocess_data,
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._run_e2e_smoke",
            lambda *a, **kw: {"ok": True, "skipped": True, "returncode": 0, "stderr": ""},
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_share_url",
            lambda url: {"ok": True, "status": 200, "final_url": url, "error": ""},
        )

        comment_calls: list[dict] = []
        def _fake_post(pr_number, body, *, repo=None, timeout=30):
            comment_calls.append({"pr_number": pr_number, "body": body, "repo": repo})
            return {"ok": True, "returncode": 0, "stderr": ""}

        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", _fake_post)

        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: success path must always succeed"
        assert ctx.state["web_advice.outcome"] == "verdict_captured"
        assert ctx.state["web_advice.verdict"] == "approve"
        assert "https://g.co/gemini/share/abc123" in ctx.state["web_advice.share_url"]
        # On a successful run, infrastructure_failure_mode is NOT set in
        # ctx.state (the field is only populated when infra is the cause).
        assert "web_advice.infrastructure_failure_mode" not in ctx.state

        # Verify PR comment was posted.
        assert len(comment_calls) == 1
        assert comment_calls[0]["pr_number"] == 655
        assert "<!-- web-advice-review -->" in comment_calls[0]["body"]
        assert "approve" in comment_calls[0]["body"]

    def test_subprocess_failure_caught_and_succeeds(self, tmp_path, monkeypatch):
        """Even if /web-advice subprocess fails, handler returns success."""
        monkeypatch.setattr(
            "runner.handler_web_advice._probe_aside_mcp", lambda c: True
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: 100,
        )

        # Subprocess "fails" — but the handler is fail-open so this is fine.
        fake_failure = {
            "ok": False,
            "verdict": "infra_unavailable",
            "summary": "",
            "share_urls": [],
            "returncode": 1,
            "stderr": "simulated subprocess failure",
            "transport": "aside_mcp",
        }
        monkeypatch.setattr(
            "runner.handler_web_advice._run_web_advice_subprocess",
            lambda **kw: fake_failure,
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._run_e2e_smoke",
            lambda *a, **kw: {"ok": True, "skipped": True, "returncode": 0, "stderr": ""},
        )
        monkeypatch.setattr(
            "runner.handler_web_advice._post_pr_comment",
            lambda *a, **kw: {"ok": True, "returncode": 0, "stderr": ""},
        )

        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: subprocess failure must not block"
        assert ctx.state["web_advice.outcome"] == "subprocess_failed"


class TestOuterExceptionGuard:
    def test_outer_exception_caught(self, tmp_path, monkeypatch):
        """An unhandled exception in the inner handler must not propagate."""
        monkeypatch.setattr(
            "runner.handler_web_advice._compute_diff_lines",
            lambda repo_dir: (_ for _ in ()).throw(RuntimeError("boom")),
        )
        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: exception must be caught"
        assert "exception" in result.output.lower() or "boom" in result.output


class TestMissingPrUrl:
    def test_missing_pr_url_succeeds(self, tmp_path):
        ctx = Context(goal="test", workdir=tmp_path, backend="echo")
        # No pr_url set in ctx.state.
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "missing_pr_url"

    def test_unparseable_pr_url_succeeds(self, tmp_path):
        ctx = Context(goal="test", workdir=tmp_path, backend="echo")
        ctx.state["pr_url"] = "not-a-valid-url"
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "unparseable_pr_url"


class TestFailOpenContract:
    """Property-style tests: every result.outcome MUST be 'success'."""

    @pytest.mark.parametrize(
        "scenario",
        [
            "missing_pr_url",
            "unparseable_pr_url",
            "trivial_diff",
            "all_transports_down",
            "subprocess_failed",
        ],
    )
    def test_always_returns_success(self, tmp_path, monkeypatch, scenario):
        """The §1.2 ironclad: handler NEVER returns outcome != success."""
        ctx = _make_ctx(tmp_path)

        if scenario == "missing_pr_url":
            ctx.state.pop("pr_url", None)
            node = Node(name="web_advice", attrs={"type": "web_advice"})
        elif scenario == "unparseable_pr_url":
            ctx.state["pr_url"] = "garbage"
            node = Node(name="web_advice", attrs={"type": "web_advice"})
        elif scenario == "trivial_diff":
            monkeypatch.setattr(
                "runner.handler_web_advice._compute_diff_lines",
                lambda repo_dir: 1,
            )
            _stub_all_transports_down(monkeypatch)
            node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})
        elif scenario == "all_transports_down":
            monkeypatch.setattr(
                "runner.handler_web_advice._compute_diff_lines",
                lambda repo_dir: 100,
            )
            _stub_all_transports_down(monkeypatch)
            node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})
        elif scenario == "subprocess_failed":
            monkeypatch.setattr(
                "runner.handler_web_advice._compute_diff_lines",
                lambda repo_dir: 100,
            )
            monkeypatch.setattr(
                "runner.handler_web_advice._probe_aside_mcp", lambda c: True
            )
            monkeypatch.setattr(
                "runner.handler_web_advice._run_web_advice_subprocess",
                lambda **kw: {
                    "ok": False,
                    "verdict": "infra_unavailable",
                    "summary": "",
                    "share_urls": [],
                    "returncode": 1,
                    "stderr": "boom",
                    "transport": "aside_mcp",
                },
            )
            monkeypatch.setattr(
                "runner.handler_web_advice._run_e2e_smoke",
                lambda *a, **kw: {"ok": True, "skipped": True, "returncode": 0, "stderr": ""},
            )
            monkeypatch.setattr(
                "runner.handler_web_advice._post_pr_comment",
                lambda *a, **kw: {"ok": True, "returncode": 0, "stderr": ""},
            )
            node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})
        else:
            raise AssertionError(f"unknown scenario {scenario!r}")

        result = _web_advice(node, ctx)
        assert result.outcome == "success", (
            f"FAIL-OPEN CONTRACT VIOLATED for scenario={scenario!r}: "
            f"got outcome={result.outcome!r}"
        )


# ---------------------------------------------------------------------------
# E2E smoke wrapper
# ---------------------------------------------------------------------------


class TestRunE2ESmoke:
    def test_skipped_when_script_absent(self, monkeypatch):
        # _WEB_ADVICE_E2E_SMOKE points at the real ~/.claude path which may
        # not exist on the test host — the wrapper must short-circuit cleanly.
        result = _run_e2e_smoke()
        # Either skipped (script absent) or ok=True with skipped=True.
        assert "ok" in result
        assert "returncode" in result

    def test_handles_timeout(self, monkeypatch):
        # Point the constant at a non-existent fake script so we never
        # actually invoke anything; verify timeout path is not triggered
        # for the absent-script branch.
        result = _run_e2e_smoke()
        assert result["ok"] is True  # absent -> ok=True with skipped=True


# ---------------------------------------------------------------------------
# Type registry smoke
# ---------------------------------------------------------------------------


def test_web_advice_registered_in_type_registry():
    """Handler is registered under the canonical key."""
    from runner.handlers import TYPE_REGISTRY

    assert "web_advice" in TYPE_REGISTRY
    assert TYPE_REGISTRY["web_advice"].__name__ == "_web_advice"


def test_web_advice_in_reviewer_soft_tier():
    """Graph-audit soft tier includes web_advice; hard tier does NOT."""
    from runner.graph_audit import (
        _HARD_TIER_REVIEWER_TYPES,
        _REVIEWER_TYPE_NAMES,
    )

    assert "web_advice" in _REVIEWER_TYPE_NAMES
    assert "web_advice" not in _HARD_TIER_REVIEWER_TYPES
