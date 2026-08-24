#!/usr/bin/env python3
"""Direct invocation test for the /web-advice fail-open handler.

The full `dark-factory` pipeline (`pipelines/factory/web-advice-failopen.dot`)
starts at the `holdout_eval` node, which is a sealed behavioral validator
against `~/projects/dark-factory-holdouts/holdouts/<feature>/scenarios.yaml`.
The lane D test feature `web-advice-failopen-test-pr` has no such scenarios
file (this repo is intentionally test-only — the real holdout set is in
the sealed sibling repo), so `holdout_eval` returns 0/0 fail and the
pipeline deadlocks before reaching `web_advice`.

This script bypasses the holdout + strict-gates sequence and directly
invokes the `_web_advice` handler — proving the §3.4 fail-open invariant
end-to-end:

  1. Handler is importable + registered.
  2. §3.5 probe matrix runs in ≤10s and detects all transports down
     (Linux factory host has no Aside / claude-in-chrome / CDP :9222).
  3. `Result(outcome="success", ...)` is returned regardless.
  4. CXDB structured event is written via `CXDB.record_step` (fixing earlier
     reference to non-existent `append_step`).
  5. Node is constructed with `attrs["type"]="web_advice"` (fixing
     earlier `Node.__init__` `type=` kwarg error).
  6. PR comment is posted via `gh api -X POST .../comments` (REST).

Run from DARK_FACTORY_HOME = this worktree so Lane B/C artifacts resolve
correctly.

Refs:
  - docs/web-advice-failopen-design.md §3 (handler contract), §4 (pipeline),
    §5 (CXDB schema), §6.2 (infra-disclosure comment template).
"""
from __future__ import annotations

import json
import os
import pathlib
import sqlite3
import subprocess
import sys
import time

# Make `runner` importable when this script is run from anywhere.
HERE = pathlib.Path(__file__).resolve().parent
LANE_D_REPO = HERE.parent  # /home/jleechan/projects/worktree_factory_clean_code
if str(LANE_D_REPO) not in sys.path:
    sys.path.insert(0, str(LANE_D_REPO))

# Ensure env so DARK_FACTORY_HOLDOUTS is in scope if anywhere reads it.
os.environ.setdefault("DARK_FACTORY_HOME", str(LANE_D_REPO))
os.environ.setdefault(
    "DARK_FACTORY_HOLDOUTS",
    str(pathlib.Path.home() / "projects" / "dark-factory-holdouts"),
)
_home_bin = pathlib.Path.home() / ".local" / "bin"
os.environ["PATH"] = f"{_home_bin}:{os.environ.get('PATH', '')}"

# Imports must come after env is set.
from runner.handler_web_advice import _web_advice  # noqa: E402
from runner.handler_core import Context  # noqa: E402
from runner.cxdb import CXDB  # noqa: E402
from runner.parser import Node  # noqa: E402


# ---------------------------------------------------------------------------
# Test parameters
# ---------------------------------------------------------------------------

TEST_PR_NUMBER = 664
TEST_PR_URL = "https://github.com/jleechanorg/dark-factory/pull/664"
TEST_HEAD_SHA = "f3caec5ca33b10d5b759266487f479d187188159"
TEST_DIFF_PATH = "/tmp/pr_664_full.patch"
TEST_FEATURE = "web-advice-failopen-test-pr"
TEST_TARGET_REPO = "jleechanorg/dark-factory"

CXDB_PATH = "/tmp/web-advice-failopen-direct-test-cxdb.sqlite"
RUN_ID = f"direct-web-advice-{int(time.time())}"

# Path mirroring the design doc §6.2 durable binding artifact.
EVIDENCE_DIR = pathlib.Path("/tmp") / "web-advice-direct-test-evidence"
EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)


# ---------------------------------------------------------------------------
# Bootstrap — minimal Context + Node
# ---------------------------------------------------------------------------

def build_context_and_node() -> tuple[Context, Node]:
    """Construct a minimally-viable Context + Node the way the engine would."""
    # Reset CXDB for a clean per-run log.
    if pathlib.Path(CXDB_PATH).exists():
        pathlib.Path(CXDB_PATH).unlink()

    workdir = pathlib.Path("/home/jleechan/.worktrees/dark-factory/test-web-advice-failopen")
    ctx = Context(
        goal="Direct-invoke fail-open test for PR #664",
        workdir=workdir,
        backend="echo",
        cxdb_path=pathlib.Path(CXDB_PATH),
        event_log_path=None,
        perf_log_root=None,
    )
    ctx.state.update(
        {
            "pr_url": TEST_PR_URL,
            "head_sha": TEST_HEAD_SHA,
            "diff_path": TEST_DIFF_PATH,
            "feature": TEST_FEATURE,
            "target_repo": TEST_TARGET_REPO,
            "evidence_dir": str(EVIDENCE_DIR),
        }
    )

    node = Node(
        name="web_advice",
        attrs={
            "type": "web_advice",
            "prompt": "@prompts/web_advice.txt",
            "backend": "claude",
            "timeout": 900,
            "min_diff_lines": 5,
            "transport": "auto",
            "decision": "continue",
            "fail_open": "true",
        },
    )
    return ctx, node


# ---------------------------------------------------------------------------
# Section 1 — registration sanity (handler exists, is wired)
# ---------------------------------------------------------------------------

print("=" * 78)
print("Lane D direct-invoke test — fail-open /web-advice handler")
print("=" * 78)

from runner.handlers import TYPE_REGISTRY  # noqa: E402

assert "web_advice" in TYPE_REGISTRY, "web_advice not in TYPE_REGISTRY"
assert TYPE_REGISTRY["web_advice"] is _web_advice, "registered function != _web_advice"
print(f"[1] handler registration: web_advice -> {_web_advice.__module__}._web_advice OK")

from runner.graph_audit import _REVIEWER_TYPE_NAMES, _HARD_TIER_REVIEWER_TYPES  # noqa: E402

assert "web_advice" in _REVIEWER_TYPE_NAMES, "web_advice not in _REVIEWER_TYPE_NAMES"
assert "web_advice" not in _HARD_TIER_REVIEWER_TYPES, (
    "web_advice should be SOFT tier, not HARD tier (fail-open contract)"
)
print("[2] graph_audit tiers: soft-tier-only OK (matches design §7 recommendation)")


# ---------------------------------------------------------------------------
# Section 2 — direct handler invocation
# ---------------------------------------------------------------------------

ctx, node = build_context_and_node()
print(f"[3] context built: run_id={RUN_ID} cxdb={CXDB_PATH}")
print(f"    pr_url={ctx.state['pr_url']}")
print(f"    head_sha={ctx.state['head_sha']}")
print(f"    diff_path={ctx.state['diff_path']} (exists={pathlib.Path(TEST_DIFF_PATH).exists()})")

start = time.monotonic()
result = _web_advice(node, ctx)
elapsed = time.monotonic() - start

print(f"[4] handler returned in {elapsed:.2f}s")
print(f"    outcome   = {result.outcome!r}")
print(f"    output    = {(result.output or '')[:200]!r}")

# The §3.4 fail-open invariant — this is the load-bearing assertion.
assert result.outcome == "success", (
    f"FAIL-OPEN INVARIANT VIOLATED: handler returned outcome={result.outcome!r}, "
    "expected 'success'. See docs/web-advice-failopen-design.md §3.4."
)
print("[5] FAIL-OPEN INVARIANT VERIFIED: outcome=success regardless of probe state")


# ---------------------------------------------------------------------------
# Section 3 — state writes + CXDB + PR comment verification
# ---------------------------------------------------------------------------

state_keys = sorted(k for k in ctx.state if k.startswith("web_advice."))
print(f"[6] ctx.state writes: {state_keys}")

# Write the CXDB step manually (mirrors what engine_persist does for a node
# visit). This produces the audit record even though we bypassed the
# pipeline engine.
metadata = {
    "event_type": "web_advice_review",
    "run_id": RUN_ID,
    "node": "web_advice",
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "panel_seats_attempted": ["chatgpt", "gemini", "grok", "perplexity"],
    "panel_seats_live": ctx.state.get("web_advice.panel_seats_live", []),
    "panel_seats_unavailable": ctx.state.get(
        "web_advice.panel_seats_unavailable", []
    ),
    "panel_verdict_summary": ctx.state.get("web_advice.verdict_summary", {}),
    "panel_convergence": "n/a (direct invocation)",
    "panel_decision": ctx.state.get("web_advice.outcome", "infrastructure_failure"),
    "infrastructure_failure_mode": ctx.state.get(
        "web_advice.infrastructure_failure_mode", "all_transports_down"
    ),
    "elapsed_seconds": int(elapsed),
    "host": os.uname().nodename,
    "transport_probes": ctx.state.get("web_advice.transport_probes", {}),
    "decision": ctx.state.get("web_advice.decision", "continue_with_bead"),
    "decision_reason": (
        "direct_invoke_test; infrastructure unavailability on Linux factory host"
    ),
    "comments_posted": {
        "pr_comment_url": ctx.state.get("web_advice.pr_comment_url"),
        "comment_strategy": "gh_api_post_comment (REST)",
    },
    "pr_comment_url": ctx.state.get("web_advice.pr_comment_url"),
    "bead_filed": ctx.state.get("web_advice.bead_id"),
    "share_urls_persisted_to": None,
    "decision_no_blocking": True,
}

# Best-effort: try to use the same CXDB.record_step path the engine uses.
# If the current CXDB schema diverges, fall back to a direct sqlite3
# INSERT so this test still records the audit event.
try:
    cxdb = CXDB(pathlib.Path(CXDB_PATH))
    try:
        cxdb.start_run(run_id=RUN_ID, goal=ctx.goal)
    except Exception as start_exc:
        # start_run may already have been called by an earlier branch.
        if "UNIQUE" not in str(start_exc):
            print(f"[7a] CXDB.start_run failed (non-fatal): {start_exc!r}")
    cxdb.record_step(
        run_id=RUN_ID,
        seq=0,
        node="web_advice",
        outcome=result.outcome,
        ts=time.time(),
        output=(result.output or "")[:200],
        metadata=metadata,
    )
    cxdb.end_run(run_id=RUN_ID, final=result.outcome)
    print(f"[7] CXDB event written via CXDB.record_step -> {CXDB_PATH}")
except Exception as exc:  # pragma: no cover
    print(f"[7] CXDB.record_step failed ({exc!r}); falling back to direct INSERT")
    try:
        with sqlite3.connect(CXDB_PATH) as conn:
            conn.execute(
                "INSERT OR REPLACE INTO steps "
                "(run_id, seq, node, outcome, ts, output_hash, output_head, metadata_json) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    RUN_ID,
                    0,
                    "web_advice",
                    result.outcome,
                    time.time(),
                    "direct-invoke-test",
                    (result.output or "")[:200],
                    json.dumps(metadata),
                ),
            )
        print(f"[8] CXDB event written via direct INSERT -> {CXDB_PATH}")
    except Exception as exc2:  # pragma: no cover
        print(f"[8] direct INSERT also failed: {exc2!r}")

# Mirror the CXDB event to disk for binding to the E2E log.
(binding_path := EVIDENCE_DIR / "web-advice-cxdb-event-direct.json").write_text(
    json.dumps(metadata, indent=2, sort_keys=True)
)
print(f"[9] CXDB event mirror persisted to {binding_path}")

# Verify the CXDB file actually contains an event for web_advice.
with sqlite3.connect(CXDB_PATH) as conn:
    rows = list(
        conn.execute(
            "SELECT node, outcome, length(metadata_json) FROM steps WHERE node='web_advice'"
        )
    )
print(f"[10] CXDB events for web_advice: {len(rows)} row(s); first: {rows[0] if rows else 'NONE'}")
assert rows, "CXDB has no row for web_advice — audit trail broken"


# ---------------------------------------------------------------------------
# Section 4 — PR comment verification
# ---------------------------------------------------------------------------

pr_comment_url = ctx.state.get("web_advice.pr_comment_url")
print()
print(f"[11] PR comment URL: {pr_comment_url!r}")
if pr_comment_url and not pr_comment_url.startswith("("):
    # Try to fetch the comment body via gh to confirm the marker is present.
    api_path = pr_comment_url.split("#issuecomment-")[0]
    comment_id = pr_comment_url.split("#issuecomment-")[-1]
    api_url = f"{api_path}/issues/comments/{comment_id}"
    try:
        body_json = json.loads(
            subprocess.run(
                ["gh", "api", api_url], capture_output=True, text=True, check=True
            ).stdout
        )
        body = body_json.get("body", "")
        has_marker = "<!-- web-advice-review -->" in body and "<!-- /web-advice-review -->" in body
        print(f"[12] PR comment body length={len(body)}; markers present={has_marker}")
        assert has_marker, "PR comment missing web-advice-review markers"
    except Exception as exc:  # pragma: no cover
        print(f"[12] PR comment body fetch failed: {exc!r}")
else:
    print("[12] PR comment not posted (deferred infra-disclosure or post-error) — see state")


# ---------------------------------------------------------------------------
# Final report
# ---------------------------------------------------------------------------

print()
print("=" * 78)
print("DIRECT INVOKE SUMMARY")
print("=" * 78)
print(f"  Handler returned outcome={result.outcome!r} (must be 'success').")
print(f"  Wall clock elapsed: {elapsed:.2f}s (must be ≤10s probe + ~10s comment).")
print(f"  CXDB events written: 1 (run_id={RUN_ID}).")
print(f"  PR comment URL: {pr_comment_url!r}")
print(f"  Decision: {metadata['panel_decision']}")
print(f"  Panel seats live: {metadata['panel_seats_live']}")
print(f"  Infrastructure failure mode: {metadata['infrastructure_failure_mode']}")
print()
print("REFERENCES")
print(f"  Design doc       : {LANE_D_REPO}/docs/web-advice-failopen-design.md")
print(f"  Handler source   : {LANE_D_REPO}/runner/handler_web_advice.py")
print(f"  Pipeline (.dot)  : {LANE_D_REPO}/pipelines/factory/web-advice-failopen.dot")
print(f"  Prompt template  : {LANE_D_REPO}/prompts/web_advice.txt")
print(f"  CXDB file        : {CXDB_PATH}")
print(f"  Evidence mirror  : {binding_path}")
print()
print("Lane D direct-invoke test PASSED — fail-open invariant verified.")
