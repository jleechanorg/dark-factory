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

import pytest  # noqa: E402

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
    _normalise_panel_result,
    _prepare_diff_path,
    _probe_aside_cli,
    _probe_aside_mcp,
    _probe_cdp_port,
    _probe_chrome_extension,
    _run_e2e_smoke,
    _run_transport_probe,
    _run_web_advice_subprocess,
    _seed_web_advice_state,
    PANEL_SEATS,
    DEFAULT_MIN_DIFF_LINES,
    PROBE_TIMEOUT_SECONDS,
)
from runner.parser import Node  # noqa: E402


@pytest.fixture(autouse=True)
def _scoped_claude_config_for_subprocess_tests(monkeypatch, tmp_path):
    """Keep legacy subprocess fixtures on an explicit project Claude scope."""
    monkeypatch.setattr(
        "runner.handlers._claude_config_dir",
        lambda: tmp_path / "claude-config",
    )


@pytest.fixture(autouse=True)
def _block_external_side_effects(monkeypatch):
    """Tests may opt in by monkeypatching; defaults cannot hit gh/br or repo docs."""
    original_run = subprocess.run

    def guarded_run(cmd, *args, **kwargs):
        argv = [str(item) for item in cmd] if isinstance(cmd, (list, tuple)) else [str(cmd)]
        if argv[:3] == ["git", "rev-parse", "HEAD"]:
            return subprocess.CompletedProcess(cmd, 0, "f" * 40 + "\n", "")
        if argv[:3] == ["gh", "pr", "view"]:
            requested_url = next(
                (item for item in argv[3:] if item.startswith("https://github.com/")),
                "https://github.com/jleechanorg/dark-factory/pull/655",
            )
            return subprocess.CompletedProcess(
                cmd,
                0,
                json.dumps({"url": requested_url, "headRefOid": "f" * 40}),
                "",
            )
        if argv and pathlib.Path(argv[0]).name in {"gh", "br"}:
            raise AssertionError("external gh/br side effect requires an explicit test mock")
        return original_run(cmd, *args, **kwargs)

    monkeypatch.setattr("runner.handler_web_advice.subprocess.run", guarded_run)
    original_persist = _persist_share_evidence

    def guarded_persist(evidence_dir, seats, *, root_dir, **kwargs):
        assert pathlib.Path(root_dir).resolve() != ROOT.resolve(), "tests must contain evidence under tmp_path"
        return original_persist(evidence_dir, seats, root_dir=root_dir, **kwargs)

    monkeypatch.setattr("runner.handler_web_advice._persist_share_evidence", guarded_persist)


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


class TestParsePrNumber:
    def test_https_url(self):
        assert _parse_pr_number("https://github.com/jleechanorg/dark-factory/pull/655") == 655

    def test_url_with_suffix_is_rejected(self):
        assert _parse_pr_number("https://github.com/jleechanorg/dark-factory/pull/655/files") is None

    @pytest.mark.parametrize("value", [
        "prefix https://github.com/o/r/pull/7",
        "https://github.com/o/r/pull/7?x=1",
        "https://github.com/o/r/pull/7/evil",
        "https://github.com/o/r/notpull/7",
        "https://github.com/o/r/pull/7x",
    ])
    def test_malformed_pull_strings_are_rejected(self, value):
        assert _parse_pr_number(value) is None

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
    def test_subprocess_fails_closed_without_scoped_claude_config(self, monkeypatch):
        def _missing_scope():
            raise ValueError("DARK_FACTORY_CLAUDE_CONFIG_DIR is required")

        called = False

        def _unexpected_run(*args, **kwargs):
            nonlocal called
            called = True

        monkeypatch.setattr("runner.handlers._claude_config_dir", _missing_scope)
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _unexpected_run)

        result = _run_web_advice_subprocess("aside_mcp", "prompt")

        assert result["ok"] is False
        assert result["returncode"] == -1
        assert "DARK_FACTORY_CLAUDE_CONFIG_DIR" in result["stderr"]
        assert called is False

    def test_subprocess_passes_scoped_sanitized_env(self, monkeypatch):
        captured = {}
        monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {
            "PATH": "/bin",
            "CLAUDE_CONFIG_DIR": "/personal/default",
            "ANTHROPIC_AUTH_TOKEN": "must-be-scrubbed",
        })
        config = pathlib.Path("/tmp/project-claude-config")
        monkeypatch.setattr("runner.handlers._claude_config_dir", lambda: config)
        payload = {
            "decision": "continue",
            "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": [],
            "panel_seats_unavailable": list(PANEL_SEATS),
            "panel_seats_unavailable_reasons": {seat: "down" for seat in PANEL_SEATS},
            "panel_verdict_summary": {},
            "panel_convergence": {},
        }

        def _fake_run(cmd, **kwargs):
            captured.update(kwargs)
            return subprocess.CompletedProcess(cmd, 0, stdout=json.dumps(payload), stderr="")

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _run_web_advice_subprocess("aside_mcp", "prompt")

        assert result["ok"] is True
        assert captured["env"]["CLAUDE_CONFIG_DIR"] == str(config)
        assert captured["env"]["PATH"] == "/bin"
        assert "ANTHROPIC_AUTH_TOKEN" not in captured["env"]

    def test_rejects_multiple_qualifying_panel_json_objects(self, monkeypatch):
        payload = {
            "decision": "continue",
            "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": ["chatgpt"],
            "panel_seats_unavailable": ["gemini", "grok", "perplexity"],
            "panel_seats_unavailable_reasons": {"gemini": "down", "grok": "down", "perplexity": "down"},
            "panel_verdict_summary": {"chatgpt": {"verdict": "approve"}},
            "panel_convergence": {},
        }
        fake = subprocess.CompletedProcess(args=[], returncode=0, stdout=json.dumps(payload) + "\n" + json.dumps(payload), stderr="")
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", lambda *a, **k: fake)
        result = _run_web_advice_subprocess("aside_mcp", "prompt")
        assert result["ok"] is False
        assert result["verdict"] == "unknown"

    def test_subprocess_parses_json_panel_result_without_keyword_triage(self, monkeypatch):
        payload = {
            "decision": "continue_with_pr_warning",
            "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": ["chatgpt"],
            "panel_seats_unavailable": ["gemini", "grok", "perplexity"],
            "panel_seats_unavailable_reasons": {"gemini": "transport absent", "grok": "transport absent", "perplexity": "transport absent"},
            "panel_verdict_summary": {
                "chatgpt": {"verdict": "not_merge", "confidence": "high", "reasoning": "file.py:7", "share_url": None, "share_url_probe": None}
            },
            "panel_convergence": {"status": "split"},
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
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (True, "g.co", ["93.184.216.34"], ""))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: {"ok": True, "status": 200, "headers": {}, "body": b"public"})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is True
        assert result["status"] == 200

    @pytest.mark.parametrize("final_url", ["https://accounts.example.com/login", "https://example.com/signin"])
    def test_rejects_login_redirect(self, monkeypatch, final_url):
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (True, "g.co", ["93.184.216.34"], "") if url.startswith("https://g.co") else (False, "", [], "login redirect"))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: {"ok": True, "status": 302, "headers": {"location": final_url}, "body": b""})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "login" in result["error"] or "signin" in result["error"]

    def test_accepts_anonymous_tracking_set_cookie(self, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (True, "g.co", ["93.184.216.34"], ""))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: {"ok": True, "status": 200, "headers": {"set-cookie": "NID=tracking"}, "body": b"Gemini shared conversation"})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is True
        assert result["status"] == 200

    def test_accepts_final_gemini_share_after_301_with_cookies(self, monkeypatch):
        calls = iter([
            {"ok": True, "status": 302, "headers": {"location": "https://share.gemini.google/abc123"}, "body": b""},
            {"ok": True, "status": 200, "headers": {"set-cookie": "NID=tracking"}, "body": b"Public Gemini conversation"},
        ])
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (True, "g.co", ["93.184.216.34"], "") if url.startswith("https://g.co") else (True, "share.gemini.google", ["93.184.216.34"], ""))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: next(calls))
        result = _probe_share_url("https://g.co/gemini/share/abc123")
        assert result["ok"] is True
        assert result["final_url"] == "https://share.gemini.google/abc123"

    @pytest.mark.parametrize(
        "url",
        [
            "http://g.co/gemini/share/abc",
            "file:///etc/passwd",
            "https://127.0.0.1/gemini/share/abc",
            "https://192.168.1.10/gemini/share/abc",
            "https://169.254.169.254/gemini/share/abc",
            "https://g.co:8443/gemini/share/abc",
            "https://user:pass@g.co/gemini/share/abc",
            "https://evil.example/share/abc",
        ],
    )
    def test_rejects_unsafe_or_unapproved_urls_before_network(self, monkeypatch, url):
        called = []
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: called.append((a, k)))
        result = _probe_share_url(url)
        assert result["ok"] is False
        assert called == []

    def test_rejects_dns_resolution_to_private_address(self, monkeypatch):
        monkeypatch.setattr(
            "runner.handler_web_advice.socket.getaddrinfo",
            lambda *a, **k: [(2, 1, 6, "", ("169.254.169.254", 443))],
        )
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "private" in result["error"] or "unsafe" in result["error"]

    def test_rejects_cgnat_dns_resolution(self, monkeypatch):
        monkeypatch.setattr(
            "runner.handler_web_advice.socket.getaddrinfo",
            lambda *a, **k: [(2, 1, 6, "", ("100.64.0.1", 443))],
        )
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "unsafe" in result["error"] or "private" in result["error"]

    def test_curl_pinned_get_brackets_ipv6_and_ignores_proxies(self, monkeypatch):
        seen = {}
        monkeypatch.setenv("HTTPS_PROXY", "http://proxy.invalid:8080")
        monkeypatch.setenv("ALL_PROXY", "http://proxy.invalid:8080")

        def fake_run(cmd, **kwargs):
            seen["cmd"] = cmd
            seen["kwargs"] = kwargs
            return subprocess.CompletedProcess(
                args=cmd,
                returncode=0,
                stdout=b"HTTP/1.1 200 OK\r\n\r\nbody",
                stderr=b"",
            )

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        from runner.handler_web_advice import _curl_pinned_get

        result = _curl_pinned_get("https://g.co/gemini/share/abc", "g.co", "2001:db8::1", 2)
        assert result["ok"] is True
        assert "--noproxy" in seen["cmd"]
        assert seen["cmd"][seen["cmd"].index("--noproxy") + 1] == "*"
        resolve = seen["cmd"][seen["cmd"].index("--resolve") + 1]
        assert resolve == "g.co:443:[2001:db8::1]"
        assert "HTTPS_PROXY" not in seen["kwargs"]["env"]
        assert "ALL_PROXY" not in seen["kwargs"]["env"]

    def test_private_redirect_is_rejected_before_connection(self, monkeypatch):
        calls = []
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (False, "", [], "unsafe private/reserved address 127.0.0.1") if "private" in url else (True, "g.co", ["93.184.216.34"], ""))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: calls.append(a) or {"ok": True, "status": 302, "headers": {"location": "https://g.co/gemini/share/private"}, "body": b""})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "before connection" in result["error"]
        assert len(calls) == 1

    def test_dns_rebind_private_second_hop_is_never_connected(self, monkeypatch):
        calls = []
        resolutions = iter([
            [(2, 1, 6, "", ("93.184.216.34", 443))],
            [(2, 1, 6, "", ("10.0.0.1", 443))],
        ])
        monkeypatch.setattr("runner.handler_web_advice.socket.getaddrinfo", lambda *a, **k: next(resolutions))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: calls.append(a) or {"ok": True, "status": 302, "headers": {"location": "https://g.co/gemini/share/next"}, "body": b""})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "private" in result["error"] or "unsafe" in result["error"]
        assert len(calls) == 1

    def test_rejects_off_provider_final_redirect(self, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._validated_share_target", lambda url: (True, "g.co", ["93.184.216.34"], "") if url.startswith("https://g.co") else (False, "", [], "host not approved"))
        monkeypatch.setattr("runner.handler_web_advice._curl_pinned_get", lambda *a, **k: {"ok": True, "status": 302, "headers": {"location": "https://evil.example/share/abc"}, "body": b""})
        result = _probe_share_url("https://g.co/gemini/share/abc")
        assert result["ok"] is False
        assert "approved" in result["error"] or "host" in result["error"]


class TestShareEvidencePersistence:
    def test_persists_atomically_with_seat_keys(self, tmp_path):
        path = _persist_share_evidence(
            pathlib.Path("evidence"),
            {"chatgpt": {"share_url": "https://chatgpt.com/share/a", "share_url_probe": {"ok": True}}},
            pr_url="https://github.com/o/r/pull/1", head_sha="abc",
            root_dir=tmp_path,
        )
        assert path == tmp_path / "evidence/web-advice-share-urls.json"
        data = json.loads(path.read_text())
        assert data["chatgpt"]["share_url"] == "https://chatgpt.com/share/a"
        assert not list((tmp_path / "evidence").glob("*.tmp"))

    @pytest.mark.parametrize("requested", [pathlib.Path("/tmp/evidence"), pathlib.Path("../escape")])
    def test_rejects_absolute_or_traversal_evidence_dir(self, tmp_path, requested):
        with pytest.raises(ValueError):
            _persist_share_evidence(requested, {}, root_dir=tmp_path)

    def test_rejects_symlink_escape_evidence_dir(self, tmp_path):
        outside = tmp_path / "outside"
        outside.mkdir()
        (tmp_path / "link").symlink_to(outside, target_is_directory=True)
        with pytest.raises(ValueError):
            _persist_share_evidence(pathlib.Path("link/sub"), {}, root_dir=tmp_path)


class TestPanelResultContract:
    def test_requires_exact_four_seat_partition_and_live_only_summaries(self):
        result = _normalise_panel_result({
            "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": ["chatgpt", "gemini"],
            "panel_seats_unavailable": ["grok", "perplexity"],
            "panel_seats_unavailable_reasons": {"grok": "down", "perplexity": "down"},
            "panel_verdict_summary": {"chatgpt": {"verdict": "approve", "confidence": "high", "reasoning": "clean", "share_url": None, "share_url_probe": None}, "gemini": {"verdict": "approve", "confidence": "medium", "reasoning": "clean", "share_url": None, "share_url_probe": None}},
            "panel_convergence": {"status": "agree"},
            "decision": "continue",
        })
        assert result["panel_contract_valid"] is True

    @pytest.mark.parametrize(
        "payload",
        [
            {"panel_seats_attempted": ["chatgpt"]},
            {
                "panel_seats_attempted": list(PANEL_SEATS),
                "panel_seats_live": ["chatgpt"],
                "panel_seats_unavailable": ["gemini"],
                "panel_seats_unavailable_reasons": {"gemini": "down"},
                "panel_verdict_summary": {},
            },
            {
                "panel_seats_attempted": list(PANEL_SEATS),
                "panel_seats_live": ["chatgpt"],
                "panel_seats_unavailable": ["gemini", "grok", "perplexity"],
                "panel_seats_unavailable_reasons": {"gemini": "down", "grok": "down", "perplexity": "down"},
                "panel_verdict_summary": {"gemini": {"verdict": "approve"}},
            },
        ],
    )
    def test_marks_invalid_panel_contract(self, payload):
        result = _normalise_panel_result(payload)
        assert result["panel_contract_valid"] is False
        assert result["panel_contract_error"]


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

    def test_large_body_uses_owner_only_body_file_and_cleans_up(self, monkeypatch):
        seen = {}
        body = "line\n" * 50_000

        def _fake_run(cmd, **kwargs):
            seen["cmd"] = cmd
            body_file = pathlib.Path(cmd[cmd.index("--body-file") + 1])
            seen["content"] = body_file.read_text(encoding="utf-8")
            seen["mode"] = body_file.stat().st_mode & 0o777
            return subprocess.CompletedProcess(args=cmd, returncode=0, stdout="ok", stderr="")

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _post_pr_comment(655, body, repo="jleechanorg/dark-factory")

        assert result["ok"] is True
        assert "--body-file" in seen["cmd"]
        assert "--body" not in seen["cmd"]
        assert seen["content"] == body
        assert seen["mode"] == 0o600
        assert not pathlib.Path(seen["cmd"][seen["cmd"].index("--body-file") + 1]).exists()

    def test_body_file_is_cleaned_up_on_timeout(self, monkeypatch):
        seen = {}

        def _timeout(cmd, **kwargs):
            seen["path"] = cmd[cmd.index("--body-file") + 1]
            raise subprocess.TimeoutExpired(cmd, kwargs["timeout"])

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _timeout)
        result = _post_pr_comment(655, "body", timeout=3)

        assert result["ok"] is False
        assert result["returncode"] == -1
        assert not pathlib.Path(seen["path"]).exists()

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
        seen = {}

        def _fake_run(cmd, **kwargs):
            seen["path"] = cmd[cmd.index("--body-file") + 1]
            raise FileNotFoundError("gh: command not found")

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _fake_run)
        result = _post_pr_comment(655, "hello")
        assert result["ok"] is False
        assert "FileNotFoundError" in result["stderr"]
        assert not pathlib.Path(seen["path"]).exists()


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


class TestWebAdviceStateSeeding:
    def test_discovers_pr_identity_from_ao_worktree(self, tmp_path, monkeypatch):
        seen = []
        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run",
            lambda cmd, **kwargs: seen.append((cmd, kwargs)) or subprocess.CompletedProcess(
                cmd, 0, '{"url":"https://github.com/o/r/pull/7","headRefOid":"abc"}', ""
            ),
        )
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        result = _seed_web_advice_state(ctx)
        assert result["ok"] is True
        assert ctx.state["pr_url"].endswith("/pull/7")
        assert ctx.state["pr_head_sha"] == "abc"
        assert ctx.state["target_repo"] == "o/r"
        assert seen[0][1]["cwd"] == str(tmp_path)

    def test_explicit_state_still_binds_remote_identity(self, tmp_path, monkeypatch):
        pr_url = "https://github.com/o/r/pull/7"
        local_head = "a" * 40
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append((cmd, kwargs))
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            assert cmd[:3] == ["gh", "pr", "view"]
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": pr_url, "headRefOid": local_head}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state.update({"pr_url": pr_url, "pr_head_sha": local_head})
        result = _seed_web_advice_state(ctx)
        assert result["source"] == "gh"
        assert ctx.state["target_repo"] == "o/r"
        assert [call[0][0] for call in calls] == ["git", "gh"]

    def test_head_sha_mismatch_is_distinct_failure(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)

        def fake_run(cmd, **kwargs):
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, "a" * 40 + "\n", "")
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": "https://github.com/o/r/pull/7", "headRefOid": "b" * 40}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state["pr_url"] = "https://github.com/o/r/pull/7"
        result = _seed_web_advice_state(ctx)
        assert result["mode"] == "head_sha_mismatch"

    def test_supplied_head_sha_mismatch_is_rejected(self, tmp_path, monkeypatch):
        pr_url = "https://github.com/o/r/pull/7"
        local_head = "a" * 40
        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)

        def fake_run(cmd, **kwargs):
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": pr_url, "headRefOid": local_head}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state.update({"pr_url": pr_url, "head_sha": "b" * 40, "pr_head_sha": local_head})

        result = _seed_web_advice_state(ctx)

        assert result["mode"] == "head_sha_mismatch"
        assert "head_sha" in result["error"]

    def test_explicit_pr_without_head_binds_exact_url_and_local_head(self, tmp_path, monkeypatch):
        pr_url = "https://github.com/o/r/pull/7"
        local_head = "a" * 40
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append((cmd, kwargs))
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            assert cmd[:3] == ["gh", "pr", "view"]
            assert pr_url in cmd
            assert "--repo" in cmd
            assert cmd[cmd.index("--repo") + 1] == "o/r"
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": pr_url, "headRefOid": local_head}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state["pr_url"] = pr_url

        result = _seed_web_advice_state(ctx)

        assert result == {"ok": True, "source": "gh", "canonical_url": pr_url}
        assert ctx.state["pr_head_sha"] == local_head
        assert ctx.state["target_repo"] == "o/r"
        assert [call[0][0] for call in calls] == ["git", "gh"]

    def test_explicit_pr_head_mismatch_is_rejected_before_side_effects(self, tmp_path, monkeypatch):
        pr_url = "https://github.com/o/r/pull/7"
        local_head = "a" * 40
        remote_head = "b" * 40
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": pr_url, "headRefOid": remote_head}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state.update({"pr_url": pr_url, "repo_dir": str(tmp_path)})
        result = _seed_web_advice_state(ctx)

        assert result["mode"] == "head_sha_mismatch"
        assert "local" in result["error"]
        assert calls[0][:3] == ["git", "rev-parse", "HEAD"]
        assert calls[1][:3] == ["gh", "pr", "view"]

    def test_canonical_url_mismatch_is_rejected(self, tmp_path, monkeypatch):
        requested = "https://github.com/o/r/pull/7"
        local_head = "a" * 40
        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)

        def fake_run(cmd, **kwargs):
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"url": requested + "/files", "headRefOid": local_head}), ""
            )

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        ctx = Context(goal="seed", workdir=tmp_path, backend="echo")
        ctx.state["pr_url"] = requested
        result = _seed_web_advice_state(ctx)
        assert result["mode"] == "canonical_url_mismatch"


class TestDiffAttachment:
    def test_writes_committed_diff_with_owner_only_mode(self, tmp_path, monkeypatch):
        calls = []
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run",
            lambda cmd, **kwargs: calls.append(cmd) or (
                subprocess.CompletedProcess(cmd, 0, "a" * 40, "")
                if cmd[1] == "merge-base"
                else subprocess.CompletedProcess(cmd, 0, b"diff --git a/a b/a\n", b"")
            ),
        )
        result = _prepare_diff_path(tmp_path, 7)
        assert result["ok"] is True
        path = result["path"]
        assert path.read_bytes().startswith(b"diff --git")
        assert path.stat().st_mode & 0o777 == 0o600
        path.unlink()
        assert calls[0][1] == "merge-base"
        assert calls[1][1] == "diff"

    def test_reports_patch_generation_failure_without_file(self, tmp_path, monkeypatch):
        monkeypatch.setattr(
            "runner.handler_web_advice.subprocess.run",
            lambda *a, **k: subprocess.CompletedProcess(a[0], 1, "", "no merge base"),
        )
        result = _prepare_diff_path(tmp_path, 7)
        assert result["ok"] is False

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
    monkeypatch.setattr(
        "runner.handler_web_advice._file_followup_bead",
        lambda **kw: {"ok": True, "bead_id": "jleechan-test"},
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
    def test_head_mismatch_returns_without_external_side_effects(self, tmp_path, monkeypatch):
        local_head = "a" * 40
        remote_head = local_head
        supplied_head = "b" * 40
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["git", "rev-parse", "HEAD"]:
                return subprocess.CompletedProcess(cmd, 0, local_head + "\n", "")
            return subprocess.CompletedProcess(
                cmd,
                0,
                json.dumps({
                    "url": "https://github.com/jleechanorg/dark-factory/pull/655",
                    "headRefOid": remote_head,
                }),
                "",
            )

        side_effects = []
        monkeypatch.setattr("runner.handler_web_advice._target_worktree", lambda ctx: tmp_path)
        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", fake_run)
        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", lambda *a, **k: side_effects.append("comment"))
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **k: side_effects.append("bead"))
        monkeypatch.setattr("runner.handler_web_advice._persist_share_evidence", lambda *a, **k: side_effects.append("evidence"))
        ctx = Context(goal="web advice review", workdir=tmp_path, backend="echo")
        ctx.state.update({
            "pr_url": "https://github.com/jleechanorg/dark-factory/pull/655",
            "pr_head_sha": supplied_head,
            "repo_dir": str(tmp_path),
        })

        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.infrastructure_failure_mode"] == "head_sha_mismatch"
        assert side_effects == []
        assert calls[0][:3] == ["git", "rev-parse", "HEAD"]
        assert calls[1][:3] == ["gh", "pr", "view"]

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
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: {"ok": True, "bead_id": "jleechan-test"})
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
        assert result.metadata["decision"] == "continue_with_bead"
        # transport_probes dict is JSON-encoded.
        probe_meta = result.metadata["transport_probes"]
        if isinstance(probe_meta, str):
            probe_meta = json.loads(probe_meta)
        assert probe_meta["aside_mcp"] is False
        assert probe_meta["cdp_port"] is False


class TestSuccessfulRun:
    def test_failed_authoritative_smoke_short_circuits_subprocess(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: True)
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        monkeypatch.setattr(
            "runner.handler_web_advice._run_e2e_smoke",
            lambda *a, **k: {"ok": False, "skipped": False, "returncode": 7, "stderr": "smoke failed"},
        )
        subprocess_calls = []
        monkeypatch.setattr(
            "runner.handler_web_advice._run_web_advice_subprocess",
            lambda **kw: subprocess_calls.append(kw),
        )
        comment_calls = []
        monkeypatch.setattr(
            "runner.handler_web_advice._post_pr_comment",
            lambda *a, **kw: comment_calls.append((a, kw)) or {"ok": True, "returncode": 0, "stderr": ""},
        )
        bead_calls = []
        monkeypatch.setattr(
            "runner.handler_web_advice._file_followup_bead",
            lambda **kw: bead_calls.append(kw) or {"ok": True, "bead_id": "jleechan-smoke"},
        )
        ctx = _make_ctx(tmp_path, target_repo="jleechanorg/dark-factory")
        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), ctx)
        assert result.outcome == "success"
        assert result.metadata["web_advice_outcome"] == "infrastructure_failure"
        assert result.metadata["infrastructure_failure_mode"] == "e2e_smoke_failed"
        assert result.metadata["e2e_smoke_stderr"] == "smoke failed"
        assert subprocess_calls == []
        assert comment_calls and "smoke failed" in comment_calls[0][0][1]
        assert bead_calls and bead_calls[0]["target_repo"] == "jleechanorg/dark-factory"

    def test_invalid_panel_contract_is_infrastructure_failure(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: True)
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        monkeypatch.setattr("runner.handler_web_advice._run_e2e_smoke", lambda *a, **k: {"ok": True, "skipped": True})
        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", lambda *a, **k: {"ok": True, "returncode": 0, "stderr": ""})
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: {"ok": True, "bead_id": "jleechan-test"})
        monkeypatch.setattr(
            "runner.handler_web_advice._run_web_advice_subprocess",
            lambda **kw: {
                "ok": True,
                "verdict": "approve",
                "decision": "continue",
                "panel_seats_attempted": ["chatgpt"],
                "panel_seats_live": ["chatgpt"],
                "panel_seats_unavailable": [],
                "panel_seats_unavailable_reasons": {},
                "panel_verdict_summary": {"chatgpt": {"verdict": "approve"}},
                "returncode": 0,
            },
        )
        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), _make_ctx(tmp_path))
        assert result.outcome == "success"
        assert result.metadata["web_advice_outcome"] == "infrastructure_failure"
        assert result.metadata["infrastructure_failure_mode"] == "panel_contract_invalid"
        assert result.metadata["verdict"] == "unknown"

    def test_target_repo_mismatch_records_error_and_does_not_file_bead(self, tmp_path, monkeypatch):
        _stub_all_transports_down(monkeypatch)
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        bead_calls = []
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: bead_calls.append(kw))
        ctx = _make_ctx(tmp_path, target_repo="other-owner/other-repo")
        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), ctx)
        assert result.outcome == "success"
        assert result.metadata["target_repo_error"]
        assert result.metadata.get("bead_filed") is None
        assert bead_calls == []

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
                "chatgpt": {"verdict": "not_merge", "confidence": "high", "reasoning": "finding", "share_url": "https://chatgpt.com/share/a", "share_url_probe": None},
                "gemini": {"verdict": "not_merge", "confidence": "high", "reasoning": "finding", "share_url": "https://g.co/gemini/share/b", "share_url_probe": None},
                "grok": {"verdict": "not_merge", "confidence": "high", "reasoning": "finding", "share_url": "https://grok.com/share/c", "share_url_probe": None},
            }, "returncode": 0, "transport": "aside_mcp",
            "panel_convergence": {"status": "split"},
        })
        ctx = _make_ctx(tmp_path, feature="web-advice-failopen-test-pr", target_repo="jleechanorg/dark-factory", evidence_dir="evidence")
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
            "panel_seats_attempted": list(PANEL_SEATS),
            "panel_seats_live": ["gemini"],
            "panel_seats_unavailable": ["chatgpt", "grok", "perplexity"],
            "panel_seats_unavailable_reasons": {
                "chatgpt": "not returned", "grok": "not returned", "perplexity": "not returned",
            },
            "panel_verdict_summary": {"gemini": {"verdict": "approve", "confidence": "high", "reasoning": "clean", "share_url": "https://g.co/gemini/share/abc123", "share_url_probe": None}},
            "panel_convergence": {"status": "agree"},
            "decision": "continue",
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
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: {"ok": True, "bead_id": "jleechan-test"})

        ctx = _make_ctx(tmp_path)
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success", "FAIL-OPEN: subprocess failure must not block"
        assert ctx.state["web_advice.outcome"] == "subprocess_failed"

    def test_subprocess_failure_emits_infra_and_one_bead_without_verdict_comment(self, tmp_path, monkeypatch):
        monkeypatch.setattr("runner.handler_web_advice._compute_diff_lines", lambda repo_dir: 100)
        monkeypatch.setattr("runner.handler_web_advice._probe_aside_mcp", lambda c: True)
        monkeypatch.setattr("runner.handler_web_advice._run_e2e_smoke", lambda *a, **k: {"ok": True, "skipped": True})
        monkeypatch.setattr("runner.handler_web_advice._run_web_advice_subprocess", lambda **kw: {"ok": False, "returncode": 1, "stderr": "boom", "transport": "aside_mcp"})
        comments = []
        beads = []
        monkeypatch.setattr("runner.handler_web_advice._post_pr_comment", lambda *a, **kw: comments.append(a[1]) or {"ok": True, "returncode": 0, "stderr": ""})
        monkeypatch.setattr("runner.handler_web_advice._file_followup_bead", lambda **kw: beads.append(kw) or {"ok": True, "bead_id": "jleechan-one"})
        ctx = _make_ctx(tmp_path, target_repo="jleechanorg/dark-factory")
        result = _web_advice(Node(name="web_advice", attrs={"type": "web_advice"}), ctx)
        assert result.outcome == "success"
        assert result.metadata["infrastructure_failure_mode"] == "subprocess_failed"
        assert result.metadata["decision"] == "continue_with_bead"
        assert len(beads) == 1
        assert len(comments) == 1
        assert "unavailable" in comments[0].lower()
        assert "0/0" not in comments[0]


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
    def test_missing_pr_url_succeeds(self, tmp_path, monkeypatch):
        ctx = Context(goal="test", workdir=tmp_path, backend="echo")
        # No pr_url set in ctx.state.
        # Explicitly disable discovery for this input-validation test.
        monkeypatch.setattr("runner.handler_web_advice._seed_web_advice_state", lambda _ctx: {"ok": True})
        node = Node(name="web_advice", attrs={"type": "web_advice", "min_diff_lines": "5"})

        result = _web_advice(node, ctx)

        assert result.outcome == "success"
        assert ctx.state["web_advice.outcome"] == "missing_pr_url"

    def test_unparseable_pr_url_succeeds(self, tmp_path, monkeypatch):
        ctx = Context(goal="test", workdir=tmp_path, backend="echo")
        ctx.state["pr_url"] = "not-a-valid-url"
        monkeypatch.setattr("runner.handler_web_advice._seed_web_advice_state", lambda _ctx: {"ok": True})
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
    def test_skipped_when_script_absent(self, monkeypatch, tmp_path):
        # Select an explicitly absent path so the result is independent of
        # whether the developer's ~/.claude skill is installed.
        monkeypatch.setattr(
            "runner.handler_web_advice._WEB_ADVICE_E2E_SMOKE",
            tmp_path / "missing-e2e-smoke.sh",
        )
        result = _run_e2e_smoke()
        assert result == {
            "ok": True,
            "skipped": True,
            "returncode": 0,
            "stderr": "e2e_smoke.sh not present (skipped)",
        }

    def test_handles_timeout(self, monkeypatch, tmp_path):
        # Make the script appear present, then force subprocess timeout so
        # this exercises the timeout branch without running host commands.
        script = tmp_path / "e2e_smoke.sh"
        script.touch()
        monkeypatch.setattr(
            "runner.handler_web_advice._WEB_ADVICE_E2E_SMOKE", script
        )

        def _raise_timeout(*args, **kwargs):
            raise subprocess.TimeoutExpired(args[0], kwargs["timeout"])

        monkeypatch.setattr("runner.handler_web_advice.subprocess.run", _raise_timeout)

        result = _run_e2e_smoke(timeout=7)
        assert result["ok"] is False
        assert result["skipped"] is False
        assert result["returncode"] == -1
        assert result["stderr"].startswith("TimeoutExpired:")


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
