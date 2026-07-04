"""End-to-end test for `daemon/factory-lite-harness.sh bead-closed-check`.

Background
----------
jleechan-tdl: a coder subagent discovered dispatched work (jleechan-9bi) had
already shipped under a different bead id's PR, and closed the bead directly
instead of opening a new PR. The overlay row was left stuck in `DISPATCHED`
forever — no PR will ever arrive to advance it, and nothing in the harness
ever re-checked the bead's own `br` status after intake.

`bead-closed-check <bead_id>` closes that gap: called per DISPATCHED/ATTESTED
row each tick, it shells out to `br show <bead_id> --json`, and if the bead
is closed while the overlay row is still DISPATCHED/ATTESTED, parks the row
`HUMAN_HELD` (reusing the existing terminal state; see
daemon/contracts/bead-closed-check.spec.md) and emits `PARKED_HUMAN_HELD`
with `reason=bead_closed_underneath`.

This test builds a scratch CXDB + a stub `br` binary (no real `br` CLI
dependency, no real bead data) and exercises all four subcommand branches
against the real, unmodified harness script.
"""

from __future__ import annotations

import json
import os
import pathlib
import stat
import subprocess

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
HARNESS = ROOT / "daemon" / "factory-lite-harness.sh"

STUB_BR = """#!/usr/bin/env bash
# Stub `br` CLI for tests: only implements `br show <id> --json`.
set -euo pipefail
if [ "$1" = "show" ]; then
  id="$2"
  case "$id" in
    closed-dispatched-bead) echo '[{"id":"closed-dispatched-bead","status":"closed"}]' ;;
    closed-attested-bead)   echo '[{"id":"closed-attested-bead","status":"closed"}]' ;;
    open-bead)              echo '[{"id":"open-bead","status":"open"}]' ;;
    closed-ready-bead)      echo '[{"id":"closed-ready-bead","status":"closed"}]' ;;
    *) exit 1 ;;
  esac
  exit 0
fi
exit 1
"""


@pytest.fixture
def harness_env(tmp_path, monkeypatch):
    """Scratch CXDB/log/br-stub, wired via the harness's env-var overrides."""
    db = tmp_path / "cxdb.sqlite"
    log = tmp_path / "daemon.jsonl"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    br_stub = bin_dir / "br"
    br_stub.write_text(STUB_BR, encoding="utf-8")
    br_stub.chmod(br_stub.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    env = os.environ.copy()
    env["AFD_DB"] = str(db)
    env["AFD_LOG"] = str(log)
    env["AFD_BR_BIN"] = str(br_stub)

    subprocess.run([str(HARNESS), "init"], check=True, capture_output=True, text=True, env=env)
    return {"db": db, "log": log, "env": env}


def _sql(db, statement):
    subprocess.run(["sqlite3", str(db), statement], check=True, capture_output=True, text=True)


def _sql_json(db, query):
    out = subprocess.run(
        ["sqlite3", "-json", str(db), query], check=True, capture_output=True, text=True
    ).stdout.strip()
    return json.loads(out) if out else []


def _insert_row(db, bead_id, state, branch=None, pr_number=None):
    branch_sql = f"'{branch}'" if branch else "NULL"
    pr_sql = str(pr_number) if pr_number is not None else "NULL"
    _sql(
        db,
        f"INSERT INTO bead_overlay (bead_id,state,attempt,branch,pr_number,updated_at) "
        f"VALUES ('{bead_id}','{state}',1,{branch_sql},{pr_sql},'2026-01-01T00:00:00Z');",
    )


def _run_check(bead_id, env):
    return subprocess.run(
        [str(HARNESS), "bead-closed-check", bead_id],
        capture_output=True,
        text=True,
        env=env,
    )


def test_closed_bead_dispatched_row_parks_human_held(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    _insert_row(db, "closed-dispatched-bead", "DISPATCHED", branch="factory/closed-dispatched-bead-r1")

    result = _run_check("closed-dispatched-bead", env)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "parked"
    rows = _sql_json(db, "SELECT state FROM bead_overlay WHERE bead_id='closed-dispatched-bead';")
    assert rows[0]["state"] == "HUMAN_HELD"


def test_closed_bead_attested_row_parks_human_held(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    _insert_row(db, "closed-attested-bead", "ATTESTED", branch="factory/closed-attested-bead-r1", pr_number=111)

    result = _run_check("closed-attested-bead", env)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "parked"
    rows = _sql_json(db, "SELECT state FROM bead_overlay WHERE bead_id='closed-attested-bead';")
    assert rows[0]["state"] == "HUMAN_HELD"


def test_closed_bead_emits_parked_human_held_with_reason(harness_env):
    db, log, env = harness_env["db"], harness_env["log"], harness_env["env"]
    _insert_row(db, "closed-dispatched-bead", "DISPATCHED", branch="factory/closed-dispatched-bead-r1")

    _run_check("closed-dispatched-bead", env)

    lines = [json.loads(l) for l in log.read_text(encoding="utf-8").splitlines() if l.strip()]
    events = [e for e in lines if e["eventType"] == "PARKED_HUMAN_HELD"]
    assert len(events) == 1
    evt = events[0]
    assert evt["beadId"] == "closed-dispatched-bead"
    assert evt["lifecycleState"] == "HUMAN_HELD"
    assert evt["context"]["reason"] == "bead_closed_underneath"
    assert evt["context"]["prior_state"] == "DISPATCHED"
    assert evt["context"]["branch"] == "factory/closed-dispatched-bead-r1"


def test_open_bead_is_a_no_op(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    _insert_row(db, "open-bead", "ATTESTED")

    result = _run_check("open-bead", env)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "open"
    rows = _sql_json(db, "SELECT state FROM bead_overlay WHERE bead_id='open-bead';")
    assert rows[0]["state"] == "ATTESTED"


def test_closed_bead_already_terminal_row_is_a_no_op(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    _insert_row(db, "closed-ready-bead", "READY", pr_number=42)

    result = _run_check("closed-ready-bead", env)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "already_terminal"
    rows = _sql_json(db, "SELECT state FROM bead_overlay WHERE bead_id='closed-ready-bead';")
    assert rows[0]["state"] == "READY"


def test_idempotent_second_call_is_already_terminal(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    _insert_row(db, "closed-dispatched-bead", "DISPATCHED", branch="factory/closed-dispatched-bead-r1")

    first = _run_check("closed-dispatched-bead", env)
    second = _run_check("closed-dispatched-bead", env)

    assert first.stdout.strip() == "parked"
    assert second.returncode == 0, second.stderr
    assert second.stdout.strip() == "already_terminal"


def test_unknown_bead_id_dies_with_clear_message(harness_env):
    env = harness_env["env"]

    result = _run_check("nonexistent-bead", env)

    assert result.returncode != 0
    assert "unknown bead_id" in result.stderr


def test_br_failure_dies_rather_than_treating_as_closed(harness_env):
    db, env = harness_env["db"], harness_env["env"]
    # Row exists in overlay but the stub `br` doesn't know this id -> br show fails.
    _insert_row(db, "br-missing-bead", "DISPATCHED")

    result = _run_check("br-missing-bead", env)

    assert result.returncode != 0
    assert "br show failed" in result.stderr
    # Must NOT have been parked -- a br/CLI failure is not evidence of closure.
    rows = _sql_json(db, "SELECT state FROM bead_overlay WHERE bead_id='br-missing-bead';")
    assert rows[0]["state"] == "DISPATCHED"


def test_wrong_arg_count_dies_with_usage(harness_env):
    env = harness_env["env"]
    result = subprocess.run(
        [str(HARNESS), "bead-closed-check"], capture_output=True, text=True, env=env
    )
    assert result.returncode != 0
    assert "usage: bead-closed-check" in result.stderr
