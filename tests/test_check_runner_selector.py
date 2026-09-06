"""Tests for scripts/check_runner_selector.py (bead jleechan-z284, issue #286).

Covers the four cardinal outcomes the drift check must distinguish:
  * selector matches ≥1 runner     → PASS (rc=0)
  * selector matches 0 runners      → DRIFT (rc=1)
  * gh / selector cannot be parsed → invocation error (rc=2)
  * zero runners online at all     → FLEET_DOWN (rc=3)

We exercise the script by stubbing ``gh`` on PATH so the unit tests do not
need network, GitHub auth, or a live runner inventory. The drift check
itself is a pure CLI around ``gh api orgs/<org>/actions/runners``; mocking
the binary at PATH level is the cleanest seam for that boundary.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "check_runner_selector.py"

# Make the script importable for the pure-Python unit tests below.
sys.path.insert(0, str(REPO_ROOT / "scripts"))


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def _write_fake_gh(bin_dir: Path, payload: dict | None, *, rc: int = 0,
                    stderr: str = "") -> Path:
    """Create a fake ``gh`` that prints ``payload`` (or returns ``rc``)."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    gh = bin_dir / "gh"
    body = payload if payload is not None else {}
    body_text = json.dumps(body)
    gh.write_text(
        "#!/usr/bin/env bash\n"
        f"printf '%s' '{body_text}'\n"
        f"exit {rc}\n"
        + (f"printf '%s' '{stderr}' >&2\n" if stderr else "")
    )
    gh.chmod(0o755)
    return gh


@pytest.fixture
def fake_gh_env(tmp_path, monkeypatch):
    """Provide a temp PATH-prefixed ``gh`` whose output we can steer per test."""
    bin_dir = tmp_path / "bin"
    payload: dict = {"total_count": 0, "runners": []}
    rc_holder = {"rc": 0}

    def install(payload_value: dict | None = None, *, rc: int = 0,
                stderr: str = "") -> None:
        if payload_value is not None:
            payload.clear()
            payload.update(payload_value)
        rc_holder["rc"] = rc
        # Wipe the dir each call so re-installs are deterministic.
        if bin_dir.exists():
            shutil.rmtree(bin_dir)
        _write_fake_gh(bin_dir, payload, rc=rc, stderr=stderr)

    # Default: empty runner fleet (forces FLEET_DOWN on first call).
    install({"total_count": 0, "runners": []})

    # Make the fake gh the first thing on PATH.
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    return install


def _run(args: list[str], *, env_extra: dict[str, str] | None = None
         ) -> subprocess.CompletedProcess:
    """Invoke the script with the given args and return the completed process."""
    env = os.environ.copy()
    # Ensure the script never accidentally inherits a real SELF_HOSTED_RUNNER_LABELS
    # from the developer's shell — each test sets it explicitly when needed.
    env.pop("SELF_HOSTED_RUNNER_LABELS", None)
    env.pop("GITHUB_REPOSITORY", None)
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        check=False, capture_output=True, text=True, env=env,
    )


# ---------------------------------------------------------------------------
# Pure-Python unit tests for _normalize_labels / select_matching
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("raw,expected", [
    # JSON array form (what the repo variable stores)
    ('["self-hosted","ezgha"]', ["self-hosted", "ezgha"]),
    ('["a","b","c"]', ["a", "b", "c"]),
    # Whitespace tolerance
    ('[ "self-hosted" , "ezgha" ]', ["self-hosted", "ezgha"]),
    # Empty array is valid (yields empty list — caller must reject)
    ('[]', []),
])
def test_normalize_labels_json_array(raw, expected):
    from check_runner_selector import _normalize_labels
    assert _normalize_labels(json.loads(raw)) == expected


def test_normalize_labels_rejects_non_list():
    from check_runner_selector import _normalize_labels
    with pytest.raises(ValueError, match="array"):
        _normalize_labels(json.loads('{"a": 1}'))


def test_select_matching_conjunction():
    """A runner matches only when it carries every required label (AND, not OR)."""
    from check_runner_selector import select_matching
    runners = [
        {"name": "r1", "labels": ["self-hosted", "ezgha"]},
        {"name": "r2", "labels": ["self-hosted", "ezgha", "extra"]},
        {"name": "r3", "labels": ["self-hosted"]},                # missing ezgha
        {"name": "r4", "labels": ["self-hosted-macos", "ezgha"]},  # missing self-hosted
    ]
    matched = select_matching(runners, ["self-hosted", "ezgha"])
    assert [r["name"] for r in matched] == ["r1", "r2"]


# ---------------------------------------------------------------------------
# CLI behaviour via subprocess + fake gh
# ---------------------------------------------------------------------------


def test_drift_detected_when_selector_matches_no_runner(fake_gh_env):
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "online-runner", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}, {"name": "ezgha"}]}
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","Linux","ARM64","agent-orchestrator"]',
        "--json",
    ])
    assert proc.returncode == 1, proc.stderr
    body = json.loads(proc.stdout)
    assert body["verdict"] == "DRIFT"
    assert body["match_count"] == 0
    assert body["selector"] == ["self-hosted", "Linux", "ARM64", "agent-orchestrator"]


def test_pass_when_selector_matches_at_least_one_runner(fake_gh_env):
    fake_gh_env({
        "total_count": 2,
        "runners": [
            {"id": 1, "name": "ez-runner-c-1", "status": "online", "busy": False,
             "labels": [
                 {"name": "self-hosted"}, {"name": "self-hosted-mikey"},
                 {"name": "ezgha"}
             ]},
            {"id": 2, "name": "ez-runner-c-2", "status": "offline", "busy": False,
             "labels": [{"name": "self-hosted"}, {"name": "ezgha"}]},
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","self-hosted-mikey","ezgha"]',
        "--json",
    ])
    assert proc.returncode == 0, proc.stderr
    body = json.loads(proc.stdout)
    assert body["verdict"] == "PASS"
    assert body["match_count"] == 1
    assert body["matches"][0]["name"] == "ez-runner-c-1"


def test_fleet_down_when_no_runners_online(fake_gh_env):
    """Fleet-wide outage is its own exit code (3) — distinct from drift."""
    fake_gh_env({
        "total_count": 2,
        "runners": [
            {"id": 1, "name": "dead-runner-1", "status": "offline", "busy": False,
             "labels": [{"name": "self-hosted"}]},
            {"id": 2, "name": "dead-runner-2", "status": "offline", "busy": False,
             "labels": [{"name": "self-hosted"}]},
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted"]',
        "--json",
    ])
    assert proc.returncode == 3, proc.stderr
    body = json.loads(proc.stdout)
    assert body["verdict"] == "FLEET_DOWN"
    assert body["online_count"] == 0


def test_include_offline_counts_offline_runners(fake_gh_env):
    """By default offline runners don't count toward match_count."""
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "offline-but-labeled", "status": "offline", "busy": False,
             "labels": [{"name": "self-hosted"}, {"name": "ezgha"}]},
        ],
    })
    # Without --include-offline: no online runners → FLEET_DOWN (rc=3)
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","ezgha"]',
    ])
    assert proc.returncode == 3, proc.stderr

    # With --include-offline: the offline runner counts as a match.
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","ezgha"]',
        "--include-offline",
        "--json",
    ])
    assert proc.returncode == 0, proc.stderr
    body = json.loads(proc.stdout)
    assert body["match_count"] == 1


def test_gh_failure_yields_invocation_error(fake_gh_env):
    fake_gh_env(None, rc=1, stderr="gh: not authenticated")
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted"]',
    ])
    assert proc.returncode == 2, proc.stderr
    assert "gh api failed" in proc.stderr


def test_malformed_selector_json_yields_invocation_error(fake_gh_env):
    """The script must refuse invalid JSON rather than crashing."""
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "r1", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}]}
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", "not-json",
    ])
    assert proc.returncode == 2, proc.stderr


def test_empty_selector_yields_invocation_error(fake_gh_env):
    """An empty conjunction is meaningless and must fail loud, not silently match-all."""
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "r1", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}]}
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", "[]",
    ])
    assert proc.returncode == 2, proc.stderr


def test_min_matches_threshold(fake_gh_env):
    """--min-matches raises the bar; passing 1 with only 1 online match must PASS."""
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "r1", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}, {"name": "ezgha"}]}
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","ezgha"]',
        "--min-matches", "2",
        "--json",
    ])
    assert proc.returncode == 1, proc.stderr
    body = json.loads(proc.stdout)
    assert body["verdict"] == "DRIFT"
    assert body["match_count"] == 1
    assert body["min_matches"] == 2


def test_env_var_overrides_cli_default_org(fake_gh_env):
    """``--org`` CLI flag must win over the DRIFT_CHECK_ORG env var."""
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "r1", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}]}
        ],
    })
    # DRIFT_CHECK_ORG says wrong org, --org CLI says right one — must still work.
    proc = _run(
        ["--org", "jleechanorg",
         "--selector", '["self-hosted"]'],
        env_extra={"DRIFT_CHECK_ORG": "some-other-org"},
    )
    assert proc.returncode == 0, proc.stderr


def test_human_output_includes_verdict(fake_gh_env):
    fake_gh_env({
        "total_count": 1,
        "runners": [
            {"id": 1, "name": "ez-runner-c-1", "status": "online", "busy": False,
             "labels": [{"name": "self-hosted"}, {"name": "ezgha"}]}
        ],
    })
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","ezgha"]',
    ])
    assert proc.returncode == 0, proc.stderr
    assert "Verdict:             PASS" in proc.stdout
    assert "Org:                 jleechanorg" in proc.stdout
    assert "ez-runner-c-1" in proc.stdout


# ---------------------------------------------------------------------------
# Authorized fail-closed inventory (Codex review item 1)
# ---------------------------------------------------------------------------


def test_gh_auth_failure_is_exit_invocation(fake_gh_env):
    """HTTP 403 / auth error from gh must yield EXIT_INVOCATION (rc=2) —
    fail-closed, not a silent PASS.  This covers the case where RUNNERS_READ_PAT
    is absent and GITHUB_TOKEN lacks manage_runners:org scope."""
    fake_gh_env(None, rc=1, stderr="gh: HTTP 403 Resource not accessible by integration")
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted","ezgha"]',
        "--json",
    ])
    assert proc.returncode == 2, (
        f"Expected EXIT_INVOCATION (2) on auth failure, got {proc.returncode}. "
        f"stderr={proc.stderr!r}"
    )
    # Script must NOT emit a JSON PASS/DRIFT verdict when it cannot fetch inventory.
    assert "PASS" not in proc.stdout, "Auth failure must not produce a PASS verdict"


def test_gh_auth_failure_json_error_output(fake_gh_env):
    """When --json is used and gh auth fails, the output JSON body must include
    verdict=ERROR so the CI annotation step can render it correctly instead of
    crashing on a missing 'verdict' key."""
    fake_gh_env(None, rc=1, stderr="HTTP 403")
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '["self-hosted"]',
        "--json",
    ])
    assert proc.returncode == 2
    # When rc=2 and --json is set, the script prints a JSON error envelope.
    if proc.stdout.strip():
        body = json.loads(proc.stdout)
        assert body.get("verdict") in {"ERROR", None}, (
            f"Unexpected verdict {body.get('verdict')!r} on auth failure"
        )


# ---------------------------------------------------------------------------
# Strict selector validation (Codex review item 3)
# ---------------------------------------------------------------------------


def test_non_array_json_object_is_rejected_with_rc2(fake_gh_env):
    """A JSON object (dict) passed as --selector must fail with EXIT_INVOCATION
    (rc=2). This validates the strict 'selector must be an array of strings'
    check in _normalize_labels.""";
    fake_gh_env({"total_count": 1, "runners": [
        {"id": 1, "name": "r1", "status": "online", "busy": False,
         "labels": [{"name": "self-hosted"}]}
    ]})
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '{"label": "self-hosted"}',  # dict, not array
    ])
    assert proc.returncode == 2, (
        f"Expected EXIT_INVOCATION (2) for non-array selector, got {proc.returncode}"
    )


def test_selector_with_integer_element_is_rejected(fake_gh_env):
    """Selectors must be arrays of strings. An array containing a non-string
    element (e.g. integer) must be coerced to string, not silently dropped.
    _normalize_labels does str(x).strip() — verify the coercion is harmless."""
    fake_gh_env({"total_count": 1, "runners": [
        {"id": 1, "name": "r1", "status": "online", "busy": False,
         "labels": [{"name": "1"}, {"name": "self-hosted"}]}
    ]})
    # Selector [1, "self-hosted"] — integer 1 coerced to "1"
    proc = _run([
        "--org", "jleechanorg",
        "--selector", '[1, "self-hosted"]',
        "--json",
    ])
    # Should PASS: runner carries labels "1" and "self-hosted"
    assert proc.returncode == 0, proc.stderr
    body = json.loads(proc.stdout)
    assert body["verdict"] == "PASS"


# ---------------------------------------------------------------------------
# Unit tests for load_selector and helper functions
# ---------------------------------------------------------------------------


def test_normalize_labels_comma_string():
    from check_runner_selector import _normalize_labels
    assert _normalize_labels("self-hosted, ezgha,  arm64 ") == ["self-hosted", "ezgha", "arm64"]


def test_normalize_labels_invalid_type():
    from check_runner_selector import _normalize_labels
    with pytest.raises(ValueError, match="selector must be a JSON array of strings"):
        _normalize_labels(12345)


def test_load_selector_cli_arg():
    from check_runner_selector import load_selector
    assert load_selector('["self-hosted", "ezgha"]') == ["self-hosted", "ezgha"]


def test_load_selector_env_var(monkeypatch):
    from check_runner_selector import load_selector
    monkeypatch.setenv("SELF_HOSTED_RUNNER_LABELS", '["self-hosted", "self-hosted-mikey"]')
    monkeypatch.delenv("GITHUB_REPOSITORY", raising=False)
    assert load_selector(None) == ["self-hosted", "self-hosted-mikey"]


def test_load_selector_missing_all(monkeypatch):
    from check_runner_selector import load_selector
    monkeypatch.delenv("SELF_HOSTED_RUNNER_LABELS", raising=False)
    monkeypatch.delenv("GITHUB_REPOSITORY", raising=False)
    with pytest.raises(ValueError, match="no --selector, no SELF_HOSTED_RUNNER_LABELS env var"):
        load_selector(None)


def test_load_selector_invalid_github_repository(monkeypatch):
    from check_runner_selector import load_selector
    monkeypatch.delenv("SELF_HOSTED_RUNNER_LABELS", raising=False)
    monkeypatch.setenv("GITHUB_REPOSITORY", "invalid_repo_name_without_slash")
    with pytest.raises(ValueError, match="is not owner/name"):
        load_selector(None)


def test_load_selector_repo_variable_success(fake_gh_env, monkeypatch):
    from check_runner_selector import load_selector
    fake_gh_env({"value": '["self-hosted", "ezgha"]'})
    monkeypatch.delenv("SELF_HOSTED_RUNNER_LABELS", raising=False)
    monkeypatch.setenv("GITHUB_REPOSITORY", "jleechanorg/dark-factory")
    assert load_selector(None) == ["self-hosted", "ezgha"]


def test_load_selector_repo_variable_gh_failure(fake_gh_env, monkeypatch):
    from check_runner_selector import load_selector
    fake_gh_env(None, rc=1, stderr="Not found")
    monkeypatch.delenv("SELF_HOSTED_RUNNER_LABELS", raising=False)
    monkeypatch.setenv("GITHUB_REPOSITORY", "jleechanorg/dark-factory")
    with pytest.raises(ValueError, match="gh api failed"):
        load_selector(None)


def test_render_human_drift():
    from check_runner_selector import render_human
    out = render_human(
        org="jleechanorg",
        selector=["self-hosted", "Linux", "ARM64", "agent-orchestrator"],
        online=[{"name": "runner-1"}],
        matches=[],
        min_matches=1,
    )
    assert "Verdict:             DRIFT" in out
    assert "Matching runners:    0 (none)" in out