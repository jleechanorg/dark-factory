"""/web-advice fail-open reviewer handler.

Owns:
  * `_web_advice` — non-blocking reviewer node that runs the `/web-advice`
    skill (or, on a host without the required transport ladder, posts an
    infra-disclosure PR comment) and ALWAYS returns ``outcome=success`` to
    the .dot engine. Three out-of-band channels carry the verdict signal:
    CXDB metadata (always written by the engine via `_persist`), a PR
    comment (best-effort), and `ctx.state["web_advice.*"]` updates.

The fail-open contract is the §1.2 ironclad ruling from
``docs/web-advice-failopen-design.md``: the handler NEVER returns
``outcome != success``. The actual `/web-advice` verdict (APPROVE /
NOT MERGE / INSUFFICIENT / infra-unavailable) is recorded in CXDB metadata
+ ctx.state, but it never becomes a pipeline-blocking condition. This is
the §3.4 structural guarantee — `web_advice` does not need a `fix` loop
edge, and graph_audit G2 cannot fire because the node never produces
``outcome!=success``.

Implementation boundaries:

* `_parse_pr_number`, `_extract_pr_repo`, `_compute_diff_lines` —
  pure helpers, mockable for tests.
* `_run_transport_probe` — runs the §3.5 probe matrix (≤10s total).
  Mockable so tests can synthesize "all transports absent".
* `_run_e2e_smoke` — invokes ``~/.claude/skills/web-advice/scripts/e2e_smoke.sh``
  to validate a live transport before sending the real prompt.
* `_run_web_advice_subprocess` — invokes ``claude --print "/web-advice <prompt>"``
  (or, for unit tests, a fake) to drive the actual /web-advice flow.
* `_post_pr_comment` — `gh pr comment` invocation via subprocess;
  best-effort, never raises.
* `_record_state` — populates ctx.state["web_advice.*"] and Result.metadata.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time
import urllib.request
import urllib.error
from typing import TYPE_CHECKING, Any, Optional

from .handler_core import Result
from .handler_render import _render_prompt

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Per /web-advice SKILL.md §3.5: the probe gate runs in ≤10s before any
# browser tab is opened. We add a small grace budget so the probe itself
# does not become a long-tail delay on hosts where subprocess.run of
# `command -v` and `aside ping` would each consume measurable time.
PROBE_TIMEOUT_SECONDS = 10

# Default node-level `min_diff_lines` when the .dot omits it. Strict lanes
# (gates.dot / pr_gates.dot) set min_diff_lines=5 in the .dot; cheap slim
# lanes (slim/minimal_pr.dot / slim/minimal_feature.dot) set 20 there.
DEFAULT_MIN_DIFF_LINES = 5

# Web-advice subprocess timeout. The §7 design doc recommends 900s; we
# clamp at 1800s (the codergen envelope ceiling in handler_core).
WEB_ADVICE_SUBPROCESS_TIMEOUT = 1800

# CDP port probed by the §2b ladder rung 4 / 5.
_CDP_PORT = 9222

# Path to the /web-advice skill's e2e_smoke.sh script. Best-effort: when
# absent the probe is skipped (still safe to call).
_WEB_ADVICE_E2E_SMOKE = pathlib.Path(
    os.path.expanduser("~/.claude/skills/web-advice/scripts/e2e_smoke.sh")
)

PANEL_SEATS = ("chatgpt", "gemini", "grok", "perplexity")


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


def _coerce_int(value: Any, default: int) -> int:
    """Coerce a node attribute to int with fail-soft fallback."""
    if value is None or value == "":
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _parse_pr_number(pr_url: str) -> Optional[int]:
    """Extract the PR number from a GitHub PR URL.

    Accepts forms like:
      - https://github.com/jleechanorg/dark-factory/pull/655
      - https://github.com/jleechanorg/dark-factory/pull/655/files
      - git@github.com:jleechanorg/dark-factory.git (ref form)

    Returns ``None`` for unparseable / non-numeric inputs so the handler
    can short-circuit cleanly without raising.
    """
    if not isinstance(pr_url, str) or not pr_url:
        return None
    # Look for /pull/N where N is an integer.
    marker = "/pull/"
    idx = pr_url.find(marker)
    if idx == -1:
        return None
    tail = pr_url[idx + len(marker):]
    digits: list[str] = []
    for ch in tail:
        if ch.isdigit():
            digits.append(ch)
        elif digits:
            break
    if not digits:
        return None
    try:
        return int("".join(digits))
    except ValueError:
        return None


def _extract_pr_repo(pr_url: str) -> Optional[str]:
    """Extract ``owner/repo`` from a GitHub PR URL.

    Returns ``None`` for unparseable inputs. Used by `gh pr comment`'s
    ``--repo`` flag for explicit scoping.
    """
    if not isinstance(pr_url, str) or not pr_url:
        return None
    # Match ``https://github.com/<owner>/<repo>/pull/<n>``.
    marker = "github.com/"
    idx = pr_url.find(marker)
    if idx == -1:
        return None
    tail = pr_url[idx + len(marker):]
    parts = tail.split("/")
    if len(parts) < 2:
        return None
    owner, repo = parts[0], parts[1]
    if not owner or not repo:
        return None
    # Strip trailing ".git" if present (rare on PR URLs, but harmless).
    repo = repo[:-4] if repo.endswith(".git") else repo
    return f"{owner}/{repo}"


def _compute_diff_lines(repo_dir: pathlib.Path, base_ref: str = "origin/main") -> Optional[int]:
    """Return the total number of changed lines in ``repo_dir`` vs ``base_ref``.

    Uses ``git diff --shortstat`` to read the summary line
    (e.g. ``3 files changed, 12 insertions(+), 4 deletions(-)``) and
    extracts the insertions + deletions total. Returns ``None`` when git
    fails or the repo is missing — the handler treats ``None`` as
    "diff size unknown, do not skip".
    """
    try:
        res = subprocess.run(
            ["git", "diff", "--shortstat", f"{base_ref}...HEAD"],
            cwd=str(repo_dir),
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if res.returncode != 0 or not res.stdout:
        return None
    # Parse "X files changed, Y insertions(+), Z deletions(-)"
    line_str = res.stdout.strip()
    insertions = 0
    deletions = 0
    for token in line_str.split(","):
        token = token.strip()
        if "insertion" in token:
            num = "".join(c for c in token.split()[0] if c.isdigit())
            insertions = int(num) if num else 0
        elif "deletion" in token:
            num = "".join(c for c in token.split()[0] if c.isdigit())
            deletions = int(num) if num else 0
    return insertions + deletions


# ---------------------------------------------------------------------------
# Transport probe matrix (≤10s)
# ---------------------------------------------------------------------------


def _probe_aside_mcp(ctx: "Context") -> bool:
    """Probe whether ``mcp__aside-mcp__*`` tools are registered with this session.

    Per SKILL.md §2b, the runner cannot introspect MCP tool registration
    from Python — we accept ctx.state["_df_aside_mcp_available"] (bool /
    "true" / "1") as the pre-declared signal. When the env var is unset
    and ctx.state carries no hint, we treat the transport as absent
    (fail-closed at the probe gate, which matches Linux/jeff-ubuntu where
    Aside is genuinely absent).
    """
    state_val = ctx.state.get("_df_aside_mcp_available")
    if isinstance(state_val, str):
        return state_val.strip().lower() in {"true", "1", "yes", "on"}
    if state_val is None:
        return False
    return bool(state_val)


def _probe_aside_cli() -> bool:
    """Probe whether the ``aside`` CLI binary is on PATH and responding."""
    if shutil.which("aside") is None:
        return False
    try:
        res = subprocess.run(
            ["aside", "--version"],
            capture_output=True,
            timeout=2.0,
            check=False,
        )
        return res.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def _probe_chrome_extension(ctx: "Context") -> bool:
    """Probe whether the claude-in-chrome extension is connected.

    Same caveat as _probe_aside_mcp — runner cannot introspect extension
    status; we accept a pre-declared ctx.state signal.
    """
    state_val = ctx.state.get("_df_claude_in_chrome_connected")
    if isinstance(state_val, str):
        return state_val.strip().lower() in {"true", "1", "yes", "on"}
    if state_val is None:
        return False
    return bool(state_val)


def _probe_cdp_port() -> bool:
    """Probe the local Chrome DevTools protocol endpoint.

    CDP is an independent Linux transport rung: Aside may be unavailable while
    a headless Chrome session is healthy.  Checking ``/json/version`` avoids
    treating an arbitrary TCP listener on port 9222 as Chrome DevTools.
    """
    try:
        request = urllib.request.Request(
            f"http://127.0.0.1:{_CDP_PORT}/json/version",
            headers={"Accept": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=0.5) as response:
            payload = json.loads(response.read(65_536))
    except (OSError, ValueError, json.JSONDecodeError, UnicodeDecodeError):
        return False

    browser = payload.get("Browser") if isinstance(payload, dict) else None
    websocket_url = payload.get("webSocketDebuggerUrl") if isinstance(payload, dict) else None
    return (
        isinstance(browser, str)
        and bool(browser.strip())
        and isinstance(websocket_url, str)
        and websocket_url.startswith(("ws://", "wss://"))
        and "/devtools/browser/" in websocket_url
    )


def _run_transport_probe(node: "Node", ctx: "Context", max_seconds: int = PROBE_TIMEOUT_SECONDS) -> dict:
    """Run the §3.5 transport probe matrix within ``max_seconds``.

    Returns a dict with the boolean probe results plus a single resolved
    ``live_transport`` field (``"aside_mcp" | "aside_cli" |
    "chrome_extension" | "cdp_port" | None``) and a human-readable
    ``summary`` string suitable for the infra-disclosure PR comment.

    The probe is wrapped in a soft wall-clock budget: each probe is
    independent and bounded to ≤1s; if the cumulative wall clock exceeds
    ``max_seconds`` we return what we have so the handler can short-circuit
    to the infra-disclosure path without burning the lane.
    """
    deadline = time.monotonic() + max_seconds
    started = time.monotonic()
    probes: dict[str, bool] = {}

    def _budget() -> bool:
        return time.monotonic() < deadline

    if _budget():
        try:
            probes["aside_mcp"] = _probe_aside_mcp(ctx)
        except Exception:
            probes["aside_mcp"] = False

    if _budget():
        try:
            probes["aside_cli"] = _probe_aside_cli()
        except Exception:
            probes["aside_cli"] = False

    if _budget():
        try:
            probes["chrome_extension"] = _probe_chrome_extension(ctx)
        except Exception:
            probes["chrome_extension"] = False

    if _budget():
        try:
            probes["cdp_port"] = _probe_cdp_port()
        except Exception:
            probes["cdp_port"] = False

    # Resolve the first live transport (ladder order matches §2 / §3.5).
    live: Optional[str] = None
    for name in ("aside_mcp", "aside_cli", "chrome_extension", "cdp_port"):
        if probes.get(name):
            live = name
            break

    elapsed = round(time.monotonic() - started, 3)
    summary_lines = [
        f"  - aside_mcp: {'present' if probes.get('aside_mcp') else 'absent'}",
        f"  - aside_cli: {'present' if probes.get('aside_cli') else 'absent'}",
        f"  - chrome_extension: {'present' if probes.get('chrome_extension') else 'absent'}",
        f"  - cdp_port: {'present' if probes.get('cdp_port') else 'absent'}",
    ]
    summary = (
        f"Transport probe matrix (≤{max_seconds}s; elapsed {elapsed}s):\n"
        + "\n".join(summary_lines)
        + (f"\n  → live_transport={live}" if live else "\n  → NO LIVE TRANSPORT")
    )

    return {
        "probes": probes,
        "live_transport": live,
        "elapsed_seconds": elapsed,
        "summary": summary,
    }


# ---------------------------------------------------------------------------
# Subprocess wrappers (mockable for tests)
# ---------------------------------------------------------------------------


def _post_pr_comment(pr_number: int, body: str, *, repo: Optional[str] = None,
                     timeout: int = 30) -> dict:
    """Post a PR comment via ``gh pr comment``.

    Uses the gh REST path (not GraphQL) per
    ``feedback_2026-07-18_gh_api_rate_limit_workaround.md``. Returns a
    dict with ``ok`` (bool), ``returncode`` (int), and ``stderr`` (str)
    so the caller can log without parsing stdout.

    Failures are NEVER raised — the caller treats ``ok=False`` as
    best-effort and the handler still returns ``outcome=success``.
    """
    cmd = ["gh", "pr", "comment", str(pr_number), "--body", body]
    if repo:
        cmd += ["--repo", repo]
    try:
        res = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return {
            "ok": res.returncode == 0,
            "returncode": res.returncode,
            "stderr": (res.stderr or "")[:500],
            "url": next((token for token in (res.stdout or "").split() if token.startswith("https://github.com/") and "#issuecomment-" in token), None),
        }
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "ok": False,
            "returncode": -1,
            "stderr": f"{type(exc).__name__}: {exc}"[:500],
            "url": None,
        }


def _run_e2e_smoke(timeout: int = 30) -> dict:
    """Run ``e2e_smoke.sh`` to validate that a transport is non-destructively live.

    Per SKILL.md §1: the e2e smoke is a non-destructive probe that prints
    the full matrix and exits 0 even when most rungs are down. We invoke
    it as a pre-flight check; its exit code is informational only —
    the handler treats the smoke's verdict as advisory and continues
    if the underlying probe gate has already confirmed a transport.
    """
    if not _WEB_ADVICE_E2E_SMOKE.exists():
        return {"ok": True, "skipped": True, "returncode": 0,
                "stderr": "e2e_smoke.sh not present (skipped)"}
    try:
        res = subprocess.run(
            [str(_WEB_ADVICE_E2E_SMOKE)],
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return {
            "ok": res.returncode == 0,
            "skipped": False,
            "returncode": res.returncode,
            "stderr": (res.stderr or "")[:500],
        }
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "ok": False,
            "skipped": False,
            "returncode": -1,
            "stderr": f"{type(exc).__name__}: {exc}"[:500],
        }


def _run_web_advice_subprocess(transport: str, prompt: str, *,
                               timeout: int = WEB_ADVICE_SUBPROCESS_TIMEOUT) -> dict:
    """Run the actual /web-advice flow via ``claude --print``.

    The subprocess owns the semantic panel decision and must emit a JSON
    object conforming to the prompt contract. The runner only parses and
    transports that structured result; it does not infer policy from prose.

    Mockable: tests patch this symbol to return a canned result.
    """
    cmd = ["claude", "--print", f"/web-advice {prompt}"]
    try:
        res = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        stdout = res.stdout or ""
        payload = _parse_structured_panel_result(stdout)
        diagnostics = {
            "returncode": res.returncode,
            "stderr": (res.stderr or "")[:500],
            "transport": transport,
            "raw_output": stdout[-2000:] if len(stdout) > 2000 else stdout,
        }
        if payload is None or res.returncode != 0:
            return {
                "ok": False, "verdict": "unknown", "summary": "", "share_urls": [],
                "panel_seats_attempted": [], "panel_seats_live": [],
                "panel_seats_unavailable": [], "panel_seats_unavailable_reasons": {},
                "panel_verdict_summary": {}, "decision": "continue", **diagnostics,
            }
        return {"ok": True, **payload, **diagnostics}
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {
            "ok": False,
            "verdict": "infra_unavailable",
            "summary": "",
            "share_urls": [],
            "returncode": -1,
            "stderr": f"{type(exc).__name__}: {exc}"[:500],
            "transport": transport,
        }


def _parse_structured_panel_result(output: str) -> Optional[dict]:
    """Parse the last JSON object emitted by /web-advice.

    Malformed output is an explicit subprocess failure. We intentionally do
    not recover a verdict from narrative text or keyword matching.
    """
    if not output:
        return None
    decoder = json.JSONDecoder()
    candidates: list[dict] = []
    for idx, char in enumerate(output):
        if char != "{":
            continue
        try:
            value, _ = decoder.raw_decode(output[idx:])
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            candidates.append(value)
    required = {
        "panel_seats_attempted", "panel_seats_live", "panel_seats_unavailable",
        "panel_seats_unavailable_reasons", "panel_verdict_summary", "decision",
    }
    for value in reversed(candidates):
        if required.issubset(value):
            return value
    return None


def _probe_share_url(url: str, *, timeout: float = 10.0) -> dict:
    """Probe a public Share URL without credentials or cookie persistence."""
    # A fresh urllib request has no CookieJar/session attached. Use GET so a
    # provider's HTML auth/consent wall can be identified, while deliberately
    # ignoring benign anonymous tracking/presentation Set-Cookie headers.
    request = urllib.request.Request(url, method="GET", headers={"Accept": "text/html"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            status_value = getattr(response, "status", None)
            if status_value is None:
                status_value = response.getcode()
            status = int(status_value)
            final_url = str(response.geturl())
            reader = getattr(response, "read", None)
            content = reader(65_536) if callable(reader) else b""
    except urllib.error.HTTPError as exc:
        return {"ok": False, "status": int(exc.code), "final_url": str(exc.geturl()), "error": f"HTTP {exc.code}"}
    except (OSError, ValueError, urllib.error.URLError) as exc:
        return {"ok": False, "status": None, "final_url": "", "error": f"{type(exc).__name__}: {exc}"[:300]}
    lowered_url = final_url.lower()
    if any(marker in lowered_url for marker in ("/login", "/signin", "/sign-in", "/auth/", "/account", "consent", "accounts.google.com")):
        return {"ok": False, "status": status, "final_url": final_url, "error": f"login redirect: {final_url}"}
    lowered_content = content.decode("utf-8", errors="ignore").lower()
    wall_markers = (
        "sign in to continue", "log in to continue", "authentication required",
        "consent required", "accounts.google.com/signin", "name=\"password\"",
    )
    if any(marker in lowered_content for marker in wall_markers):
        return {"ok": False, "status": status, "final_url": final_url, "error": "authentication/consent wall in response content"}
    # urllib follows ordinary redirects; a valid probe therefore ends at
    # 200. A bare 3xx is not evidence of a public unauthenticated page.
    if status != 200:
        return {"ok": False, "status": status, "final_url": final_url, "error": f"HTTP {status}"}
    return {"ok": True, "status": status, "final_url": final_url, "error": ""}


def _persist_share_evidence(
    evidence_dir: pathlib.Path,
    seats: dict,
    *,
    pr_url: str = "",
    head_sha: str = "",
) -> pathlib.Path:
    """Atomically persist the per-seat public-share evidence JSON."""
    evidence_dir = pathlib.Path(evidence_dir)
    evidence_dir.mkdir(parents=True, exist_ok=True)
    target = evidence_dir / "web-advice-share-urls.json"
    payload = {"schema_version": 1, "pr_url": pr_url, "head_sha": head_sha, **seats}
    fd, temp_name = tempfile.mkstemp(prefix=f".{target.name}.", suffix=".tmp", dir=evidence_dir)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_name, target)
    except Exception:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise
    return target


def _extract_verdict_token(output: str) -> str:
    """Best-effort verdict extraction from a /web-advice response."""
    if not output:
        return "unknown"
    upper = output.upper()
    for token in ("APPROVE", "NOT MERGE", "INSUFFICIENT", "FAIL", "INCOMPLETE"):
        if token in upper:
            return token.lower().replace(" ", "_")
    return "unknown"


def _extract_share_urls(output: str) -> list[str]:
    """Best-effort share URL extraction from a /web-advice response.

    Conservative: matches only the well-known public-share hosts from
    SKILL.md §1b #3 (g.co/gemini/share, grok.com/share, chatgpt.com/share,
    perplexity.ai/search). Anything else is ignored to avoid fabricating
    a share URL that was never emitted by the skill.
    """
    import re as _re
    if not output:
        return []
    pattern = _re.compile(
        r"https?://(?:g\.co/gemini/share/[A-Za-z0-9_-]+|"
        r"share\.gemini\.google/[A-Za-z0-9_-]+|"
        r"gemini\.google\.com/share/[A-Za-z0-9_-]+|"
        r"grok\.com/share/[A-Za-z0-9_-]+|"
        r"chatgpt\.com/share/[A-Za-z0-9_-]+|"
        r"perplexity\.ai/search/[A-Za-z0-9_-]+)"
    )
    return pattern.findall(output)


def _normalise_panel_result(verdict_data: dict) -> dict:
    """Apply transport-safe shape normalization without semantic triage."""
    attempted = [seat for seat in verdict_data.get("panel_seats_attempted", PANEL_SEATS) if seat in PANEL_SEATS]
    live = [seat for seat in verdict_data.get("panel_seats_live", []) if seat in attempted]
    unavailable = [seat for seat in verdict_data.get("panel_seats_unavailable", []) if seat in attempted and seat not in live]
    unavailable.extend(seat for seat in attempted if seat not in live and seat not in unavailable)
    reasons = dict(verdict_data.get("panel_seats_unavailable_reasons") or {})
    for seat in unavailable:
        reasons.setdefault(seat, "unavailable reason not provided by structured panel result")
    summaries = dict(verdict_data.get("panel_verdict_summary") or {})
    probes: dict[str, dict] = {}
    for seat, summary in summaries.items():
        if not isinstance(summary, dict):
            continue
        url = summary.get("share_url")
        if not url:
            continue
        probe = _probe_share_url(str(url))
        probes[seat] = probe
        summary["share_url_probe"] = probe
        if not probe.get("ok"):
            summary["share_url"] = None

    # Transitional flat URL support still probes every emitted URL, but does
    # not claim those URLs belong to all four seats.
    flat_urls = [str(url) for url in (verdict_data.get("share_urls") or []) if url]
    flat_probes = [_probe_share_url(url) for url in flat_urls]
    return {
        **verdict_data,
        "panel_seats_attempted": attempted,
        "panel_seats_live": live,
        "panel_seats_unavailable": unavailable,
        "panel_seats_unavailable_reasons": reasons,
        "panel_verdict_summary": summaries,
        "share_url_probes": probes,
        "flat_share_url_probes": flat_probes,
        "share_urls": [url for url, probe in zip(flat_urls, flat_probes) if probe.get("ok")],
    }


def _file_followup_bead(*, target_repo: str, pr_number: int, decision: str, summary: str) -> dict:
    """File the model-requested follow-up bead with an unambiguous repo scope."""
    if not target_repo:
        return {"ok": False, "bead_id": None, "error": "missing target_repo"}
    title = f"PR #{pr_number} /web-advice panel follow-up"
    body = f"target_repo: {target_repo}\n\nDecision: {decision}\n\n{summary[:3000]}"
    try:
        res = subprocess.run(
            ["br", "create", "--title", title, "--body", body],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"ok": False, "bead_id": None, "error": f"{type(exc).__name__}: {exc}"[:300]}
    output = (res.stdout or "").strip()
    bead_id = None
    for token in output.split():
        if token.startswith("jleechan-"):
            bead_id = token.strip("` ,.:;()")
            break
    return {"ok": res.returncode == 0, "bead_id": bead_id, "error": (res.stderr or "")[:300], "body": body}


# ---------------------------------------------------------------------------
# PR comment builders
# ---------------------------------------------------------------------------


def _build_verdict_comment(verdict_data: dict, pr_url: str) -> str:
    """Build the §6.1 verdict-captured PR comment body."""
    verdict = verdict_data.get("verdict", "unknown")
    summary = verdict_data.get("summary", "")
    decision = verdict_data.get("decision", "continue")
    panel_live = verdict_data.get("panel_seats_live", []) or []
    panel_attempted = verdict_data.get("panel_seats_attempted", []) or []
    share_urls = verdict_data.get("share_urls", []) or []
    transport = verdict_data.get("transport", "auto")
    lines = [
        "<!-- web-advice-review -->",
        "## /web-advice panel review",
        "",
        f"**Decision:** `{decision}` (verdict captured — fail-open)",
        f"**Panel seats:** {len(panel_live)}/{len(panel_attempted)} live",
        f"**Transport:** `{transport}`",
        f"**PR:** {pr_url}",
        "",
        f"**Verdict:** `{verdict}`",
        "",
        "**Summary:**",
        "",
        "```",
        summary[:1500] if summary else "(no summary returned)",
        "```",
    ]
    if share_urls:
        lines.extend(["", "**Public share URLs:**"])
        for url in share_urls:
            lines.append(f"- {url}")
    summaries = verdict_data.get("panel_verdict_summary") or {}
    if summaries:
        lines.extend(["", "**Per-seat summaries:**"])
        for seat, seat_data in summaries.items():
            if isinstance(seat_data, dict):
                lines.append(f"- `{seat}`: `{seat_data.get('verdict', 'unknown')}`")
    lines.append("")
    lines.append("<!-- /web-advice-review -->")
    return "\n".join(lines)


def _build_infra_disclosure_comment(probe_summary: str, pr_url: str) -> str:
    """Build the §6.2 infra-disclosure PR comment body."""
    return (
        "<!-- web-advice-review -->\n"
        "## /web-advice unavailable on this host\n"
        "\n"
        "**Decision:** `continue_with_bead` — /web-advice could not run (fail-open).\n"
        "\n"
        f"**PR:** {pr_url}\n"
        "\n"
        "### Transport probe matrix (per §3.5)\n"
        "\n"
        f"{probe_summary}\n"
        "\n"
        "This is a host capability gap, not a content defect of this PR. "
        "Per the `/web-advice` skill §1 + §2b and the dark-factory "
        "fail-open contract (see `docs/web-advice-failopen-design.md` §1.2), "
        "no fabricated share URLs are posted and the PR does not block on "
        "transport unavailability. Operator adjudication required if the "
        "host's transport capability needs to be restored.\n"
        "\n"
        "<!-- /web-advice-review -->"
    )


# ---------------------------------------------------------------------------
# Main handler
# ---------------------------------------------------------------------------


def _record_state(ctx: "Context", metadata: dict) -> None:
    """Write structured ``web_advice.*`` keys into ctx.state.

    The Result.metadata carries the same fields for CXDB; ctx.state is
    for downstream graph nodes that might consume
    ``ctx.state["web_advice.verdict"]`` etc.
    """
    ctx.state["web_advice.outcome"] = str(metadata.get("web_advice_outcome", "unknown"))
    ctx.state["web_advice.verdict"] = str(metadata.get("verdict", "unknown"))
    for field in ("panel_seats_live", "panel_seats_attempted", "panel_decision", "bead_id", "pr_comment_url", "share_urls_persisted_to"):
        if field in metadata:
            ctx.state[f"web_advice.{field}"] = metadata[field]
    if "panel_verdict_summary" in metadata:
        ctx.state["web_advice.verdict_summary"] = metadata["panel_verdict_summary"]
    if metadata.get("infrastructure_failure_mode"):
        ctx.state["web_advice.infrastructure_failure_mode"] = str(
            metadata["infrastructure_failure_mode"]
        )
    share_urls = list(metadata.get("share_urls") or [])
    for summary in (metadata.get("panel_verdict_summary") or {}).values():
        if isinstance(summary, dict) and summary.get("share_url"):
            share_urls.append(str(summary["share_url"]))
    if share_urls:
        ctx.state["web_advice.share_url"] = ",".join(share_urls)
    elapsed = metadata.get("elapsed_seconds")
    if elapsed is not None:
        try:
            ctx.state["web_advice.elapsed_seconds"] = str(int(elapsed))
        except (TypeError, ValueError):
            pass


def _web_advice(node: "Node", ctx: "Context") -> "Result":
    """Fail-open /web-advice reviewer node.

    Returns ``Result(outcome="success", ...)`` ALWAYS. Verdict + seat
    accounting are written to ctx.state and Result.metadata (which the
    engine forwards to CXDB via _persist); the PR comment is best-effort
    and never raises.

    The handler is the §1.2 ironclad fail-open contract — a reviewer
    that can never block the .dot graph. See
    ``docs/web-advice-failopen-design.md`` for the design rationale.
    """
    started_ts = time.time()
    metadata: dict[str, Any] = {
        "event_type": "web_advice_review",
        "web_advice_outcome": "skipped_trivial_diff",
        "verdict": "unknown",
        "infrastructure_failure_mode": None,
        "decision": "continue",
        "elapsed_seconds": 0,
        "panel_seats_attempted": [],
        "panel_seats_live": [],
        "panel_seats_unavailable": [],
        "panel_seats_unavailable_reasons": {},
        "panel_verdict_summary": {},
        "panel_decision": "continue",
        "panel_convergence": {},
        "transport_probes": {},
    }

    try:
        return _web_advice_inner(node, ctx, metadata, started_ts)
    except Exception as exc:  # pragma: no cover — defensive outer wrap
        # Per spec: handler MUST never raise. Convert to a fail-open
        # success with the exception captured in metadata.
        elapsed = round(time.time() - started_ts, 3)
        metadata.update({
            "web_advice_outcome": "handler_exception",
            "decision": "continue",
            "elapsed_seconds": elapsed,
            "handler_exception": f"{type(exc).__name__}: {exc}"[:500],
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice handler exception: {type(exc).__name__}: {exc}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": metadata["web_advice_outcome"],
                "web_advice.verdict": metadata["verdict"],
            },
        )


def _web_advice_inner(node: "Node", ctx: "Context", metadata: dict, started_ts: float) -> "Result":
    """Inner handler — the try/except above wraps this so any leak is caught."""

    # ------------------------------------------------------------------
    # 1. Inputs
    # ------------------------------------------------------------------
    pr_url = str(ctx.state.get("pr_url") or "")
    pr_head_sha = str(ctx.state.get("head_sha") or ctx.state.get("pr_head_sha") or "")
    repo_dir_str = str(ctx.state.get("repo_dir") or ctx.workdir or "")
    repo_dir = pathlib.Path(repo_dir_str) if repo_dir_str else ctx.workdir

    min_diff_lines = _coerce_int(node.attrs.get("min_diff_lines"), DEFAULT_MIN_DIFF_LINES)
    transport_pref = str(node.attrs.get("transport", "auto")).strip().lower()

    metadata.update({
        "pr_url": pr_url,
        "pr_head_sha": pr_head_sha,
        "min_diff_lines": min_diff_lines,
        "transport_pref": transport_pref,
    })

    if not pr_url:
        metadata.update({
            "web_advice_outcome": "missing_pr_url",
            "decision": "continue",
            "elapsed_seconds": round(time.time() - started_ts, 3),
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output="web_advice skipped: no pr_url in ctx.state",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={"web_advice.outcome": "missing_pr_url"},
        )

    pr_number = _parse_pr_number(pr_url)
    pr_repo = _extract_pr_repo(pr_url)
    target_repo = str(ctx.state.get("target_repo") or "")
    if pr_number is None:
        metadata.update({
            "web_advice_outcome": "unparseable_pr_url",
            "decision": "continue",
            "elapsed_seconds": round(time.time() - started_ts, 3),
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice skipped: could not parse PR number from {pr_url!r}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={"web_advice.outcome": "unparseable_pr_url"},
        )

    # ------------------------------------------------------------------
    # 2. Skip on small diff
    # ------------------------------------------------------------------
    diff_lines = _compute_diff_lines(repo_dir)
    metadata["diff_lines"] = diff_lines
    if diff_lines is not None and diff_lines < min_diff_lines:
        elapsed = round(time.time() - started_ts, 3)
        metadata.update({
            "web_advice_outcome": "skipped_trivial_diff",
            "decision": "continue",
            "elapsed_seconds": elapsed,
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice skipped: diff {diff_lines} lines < min_diff_lines {min_diff_lines}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "skipped_trivial_diff",
                "web_advice.diff_lines": str(diff_lines),
            },
        )

    # ------------------------------------------------------------------
    # 3. Transport probe (≤10s)
    # ------------------------------------------------------------------
    probe = _run_transport_probe(node, ctx)
    metadata["transport_probes"] = probe["probes"]
    metadata["probe_elapsed_seconds"] = probe["elapsed_seconds"]
    live_transport = probe["live_transport"] if transport_pref in ("auto", "") else transport_pref
    metadata["live_transport"] = live_transport

    if not live_transport:
        # No transport → infra disclosure path. Still always succeed.
        elapsed = round(time.time() - started_ts, 3)
        comment_body = _build_infra_disclosure_comment(probe["summary"], pr_url)
        comment_result = _post_pr_comment(pr_number, comment_body, repo=pr_repo)
        bead_result = _file_followup_bead(
            target_repo=target_repo,
            pr_number=pr_number,
            decision="continue_with_bead",
            summary=probe["summary"],
        ) if target_repo else {"ok": False, "bead_id": None, "error": "missing target_repo"}
        evidence_dir = pathlib.Path(str(ctx.state.get("evidence_dir") or repo_dir / f"docs/pr{pr_number}-evidence"))
        evidence_path = _persist_share_evidence(evidence_dir, {}, pr_url=pr_url, head_sha=pr_head_sha)
        metadata.update({
            "web_advice_outcome": "infrastructure_failure",
            "infrastructure_failure_mode": "all_transports_down",
            "decision": "continue_with_bead" if target_repo else "continue",
            "panel_decision": "continue_with_bead" if target_repo else "continue",
            "bead_id": bead_result.get("bead_id"),
            "bead_filed": bead_result.get("ok", False),
            "share_urls_persisted_to": str(evidence_path),
            "elapsed_seconds": elapsed,
            "pr_comment_posted": comment_result["ok"],
            "pr_comment_rc": comment_result["returncode"],
            "pr_comment_url": comment_result.get("url"),
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=(
                f"web_advice infra_unavailable on this host "
                f"(probe_summary={probe['summary']!r}; "
                f"pr_comment_ok={comment_result['ok']})"
            ),
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "infrastructure_failure",
                "web_advice.infrastructure_failure_mode": "all_transports_down",
            },
        )

    # ------------------------------------------------------------------
    # 4. e2e_smoke pre-flight (best-effort)
    # ------------------------------------------------------------------
    smoke = _run_e2e_smoke()
    metadata["e2e_smoke_ok"] = smoke["ok"]

    # ------------------------------------------------------------------
    # 5. Run the actual /web-advice subprocess
    # ------------------------------------------------------------------
    prompt_text = _render_prompt(node, ctx)
    verdict_data = _run_web_advice_subprocess(
        transport=live_transport or "auto",
        prompt=prompt_text,
    )
    verdict_data = _normalise_panel_result(verdict_data)
    metadata.update({
        "verdict": verdict_data.get("verdict", "unknown"),
        "share_urls": verdict_data.get("share_urls", []) or [],
        "subprocess_rc": verdict_data.get("returncode", -1),
        "panel_seats_attempted": verdict_data.get("panel_seats_attempted", []),
        "panel_seats_live": verdict_data.get("panel_seats_live", []),
        "panel_seats_unavailable": verdict_data.get("panel_seats_unavailable", []),
        "panel_seats_unavailable_reasons": verdict_data.get("panel_seats_unavailable_reasons", {}),
        "panel_verdict_summary": verdict_data.get("panel_verdict_summary", {}),
        "panel_convergence": verdict_data.get("panel_convergence", {}),
        "panel_decision": verdict_data.get("decision", "continue"),
        "share_url_probes": verdict_data.get("share_url_probes", {}),
    })

    # ------------------------------------------------------------------
    # 6. Post verdict PR comment
    # ------------------------------------------------------------------
    comment_body = _build_verdict_comment(verdict_data, pr_url)
    comment_result = _post_pr_comment(pr_number, comment_body, repo=pr_repo)
    metadata["pr_comment_posted"] = comment_result["ok"]
    metadata["pr_comment_rc"] = comment_result["returncode"]
    metadata["pr_comment_url"] = comment_result.get("url")

    elapsed = round(time.time() - started_ts, 3)
    metadata["elapsed_seconds"] = elapsed
    metadata["decision"] = verdict_data.get("decision", "continue")
    if not verdict_data.get("ok"):
        metadata["web_advice_outcome"] = "subprocess_failed"
        metadata["decision"] = "continue"
    else:
        metadata["web_advice_outcome"] = "verdict_captured"

    evidence_dir = pathlib.Path(str(ctx.state.get("evidence_dir") or repo_dir / f"docs/pr{pr_number}-evidence"))
    evidence_path = _persist_share_evidence(
        evidence_dir,
        metadata["panel_verdict_summary"],
        pr_url=pr_url,
        head_sha=pr_head_sha,
    )
    metadata["share_urls_persisted_to"] = str(evidence_path)
    if metadata["decision"] == "continue_with_bead":
        bead_result = _file_followup_bead(
            target_repo=target_repo,
            pr_number=pr_number,
            decision=metadata["decision"],
            summary=json.dumps(metadata["panel_verdict_summary"], ensure_ascii=False),
        ) if target_repo else {"ok": False, "bead_id": None, "error": "missing target_repo"}
        metadata["bead_id"] = bead_result.get("bead_id")
        metadata["bead_filed"] = bead_result.get("ok", False)

    _record_state(ctx, metadata)
    return Result(
        outcome="success",
        output=(
            f"web_advice {metadata['web_advice_outcome']}: "
            f"verdict={metadata['verdict']!r} "
            f"transport={live_transport!r} "
            f"share_urls={len(metadata['share_urls'])} "
            f"pr_comment_ok={comment_result['ok']}"
        ),
        metadata={k: _coerce_meta(v) for k, v in metadata.items()},
        context_updates={
            "web_advice.outcome": metadata["web_advice_outcome"],
            "web_advice.verdict": metadata["verdict"],
        },
    )


def _coerce_meta(value: Any) -> Any:
    """Preserve a metadata value for CXDB's native JSON serialization."""
    # CXDB serializes the complete metadata mapping as JSON. Preserve nested
    # values here so consumers receive the documented schema rather than a
    # JSON string requiring a second decode.
    return value
