"""Executable skeptic reviewer queue contracts.

These tests cover the Python GHA path against the canonical daemon vendor
argv/env contracts.  The default queue must not resolve to a bare personal
Claude or to unsupported Codex/Gemini-only command construction.
"""

from __future__ import annotations

import json
import pytest

import runner.skeptic_gate_cli as cli
from runner.reviewer_priority import gha_reviewer_priority, skeptic_reviewer_priority


def test_default_reviewers_follow_authoritative_queue():
    assert json.loads(cli.DEFAULT_REVIEWERS_JSON) == [
        [vendor, ""] for vendor in gha_reviewer_priority()
    ]
    assert tuple(cli.MANDATORY_REVIEWERS) == tuple(gha_reviewer_priority())
    assert skeptic_reviewer_priority() == ["claudem", "agy", "cursor-agent"]


@pytest.mark.parametrize(
    ("reviewer", "model", "expected"),
    [
        (
            "codex",
            "",
            [
                "codex",
                "exec",
                "--sandbox",
                "read-only",
                "--ephemeral",
                "--skip-git-repo-check",
                "--json",
                "-c",
                "shell.enable=false",
                "-c",
                'web_search="disabled"',
                "-",
            ],
        ),
        (
            "gemini",
            "gemini-3.7-pro",
            [
                "gemini",
                "-m",
                "gemini-3.7-pro",
                "-s",
                "--approval-mode",
                "default",
                "-p",
                "-",
            ],
        ),
    ],
)
def test_build_reviewer_cmd_supports_every_default_vendor(reviewer, model, expected):
    assert cli._build_reviewer_cmd(reviewer, model) == expected


def test_parse_reviewers_accepts_exact_configured_queue():
    parsed = cli._parse_reviewers(cli.DEFAULT_REVIEWERS_JSON)
    assert [vendor for vendor, _ in parsed] == gha_reviewer_priority()


def test_parse_reviewers_rejects_legacy_unsupported_vendor():
    with pytest.raises(SystemExit, match="not allowed"):
        cli._parse_reviewers(json.dumps([["claude", ""], ["gemini", ""]]))


@pytest.mark.parametrize(
    ("reviewer", "parent_env", "expected_cmd", "expects_stdin"),
    [
        ("codex", {"PATH": "/bin"}, ["codex", "exec", "--ephemeral", "--skip-git-repo-check", "--json", "-c", "shell.enable=false", "-c", 'web_search="disabled"', "-"], True),
        ("gemini", {"PATH": "/bin"}, ["gemini", "-m", "gemini-3.7-pro", "-s", "--approval-mode", "default", "-p", "-"], True),
    ],
)
def test_invoke_reviewer_dispatches_vendor_contracts(
    monkeypatch, reviewer, parent_env, expected_cmd, expects_stdin
):
    seen = {}

    class Completed:
        returncode = 0
        stdout = "review output"
        stderr = ""

    def fake_run(cmd, *, input, env, **kwargs):
        seen.update(cmd=cmd, input=input, env=env)
        return Completed()

    monkeypatch.setattr(cli.subprocess, "run", fake_run)
    stdout, error = cli.invoke_reviewer(
        reviewer, "", "review prompt", parent_env=parent_env
    )

    expected_stdout = "" if reviewer == "codex" else "review output"
    assert (stdout, error) == (expected_stdout, None)
    assert seen["cmd"] == expected_cmd
    assert (seen["input"] == "review prompt") is expects_stdin
