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
import ipaddress
import os
import pathlib
import re
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request
import urllib.error
import urllib.parse
from typing import TYPE_CHECKING, Any, Optional

from .handler_core import Result, _target_worktree
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

_SHARE_PATH_PATTERNS = {
    "g.co": re.compile(r"^/gemini/share/[A-Za-z0-9_-]+$"),
    "share.gemini.google": re.compile(r"^/[A-Za-z0-9_-]+$"),
    "gemini.google.com": re.compile(r"^/share/[A-Za-z0-9_-]+$"),
    "grok.com": re.compile(r"^/share/[A-Za-z0-9_-]+$"),
    "chatgpt.com": re.compile(r"^/share/[A-Za-z0-9_-]+$"),
    "perplexity.ai": re.compile(r"^/search/[A-Za-z0-9_-]+$"),
}

_PR_URL_RE = re.compile(
    r"^https://github\.com/(?P<owner>[A-Za-z0-9_.-]+)/(?P<repo>[A-Za-z0-9_.-]+)/pull/(?P<number>[0-9]+)$"
)
_PANEL_CONTRACT_KEYS = {
    "panel_seats_attempted",
    "panel_seats_live",
    "panel_seats_unavailable",
    "panel_seats_unavailable_reasons",
    "panel_verdict_summary",
    "panel_convergence",
    "decision",
}
_PANEL_VERDICTS = {"approve", "not_merge", "insufficient"}
_PANEL_CONFIDENCE = {"low", "medium", "high"}
_PANEL_DECISIONS = {"continue", "continue_with_bead", "continue_with_pr_warning"}
_PANEL_SUMMARY_KEYS = {"verdict", "confidence", "reasoning", "share_url", "share_url_probe"}


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


def _seed_web_advice_state(ctx: "Context") -> dict:
    """Fill missing PR identity from the target worktree without overriding state."""
    worktree = _target_worktree(ctx)
    if not ctx.state.get("repo_dir"):
        ctx.state["repo_dir"] = str(worktree)
    explicit_pr = bool(ctx.state.get("pr_url"))
    explicit_head = str(ctx.state.get("head_sha") or ctx.state.get("pr_head_sha") or "")
    supplied_heads = {
        field: str(ctx.state.get(field)).strip()
        for field in ("head_sha", "pr_head_sha")
        if ctx.state.get(field)
    }
    # Validate explicit identity before any gh lookup.  A malformed PR URL or
    # repository mismatch is an input error, not an invitation to discover a
    # different PR from the checkout (which would create an unintended side
    # effect and defeat the caller's identity binding).
    if explicit_pr:
        explicit_url = str(ctx.state.get("pr_url") or "")
        explicit_repo = _extract_pr_repo(explicit_url)
        if _parse_pr_number(explicit_url) is None or explicit_repo is None:
            return {"ok": False, "mode": "invalid_pr_identity", "error": "pr_url must be an exact GitHub PR URL"}
        requested_repo = str(ctx.state.get("target_repo") or "")
        if requested_repo and requested_repo != explicit_repo:
            return {
                "ok": False,
                "mode": "target_repo_mismatch",
                "error": f"target_repo {requested_repo!r} does not match PR repository {explicit_repo!r}",
            }
    explicit_repo = _extract_pr_repo(str(ctx.state.get("pr_url") or "")) if explicit_pr else None
    local_head = ""
    if explicit_pr:
        try:
            local_head_result = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=str(worktree), capture_output=True, text=True, timeout=10, check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            return {
                "ok": False,
                "mode": "local_head_unavailable",
                "error": f"could not resolve checked-out local HEAD: {type(exc).__name__}: {exc}"[:500],
            }
        local_head = (local_head_result.stdout or "").strip()
        if local_head_result.returncode != 0 or not re.fullmatch(r"[0-9a-fA-F]{40}", local_head):
            return {
                "ok": False,
                "mode": "local_head_unavailable",
                "error": "could not resolve checked-out local HEAD",
            }
    try:
        view_args = ["gh", "pr", "view"]
        if explicit_pr:
            # Bind discovery to the caller's exact URL and repository. Never
            # fall back to whichever PR happens to be associated with HEAD.
            view_args.append(str(ctx.state["pr_url"]))
            if explicit_repo:
                view_args.extend(["--repo", explicit_repo])
        view_args.extend(["--json", "url,headRefOid"])
        result = subprocess.run(
            view_args,
            cwd=str(worktree), capture_output=True, text=True, timeout=10, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"ok": False, "error": f"gh pr view unavailable: {type(exc).__name__}: {exc}"[:500]}
    if result.returncode != 0 or not result.stdout:
        return {"ok": False, "error": (result.stderr or "gh pr view returned no PR")[:500]}
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        return {"ok": False, "error": f"invalid gh pr view JSON: {exc}"[:500]}
    discovered_pr = payload.get("url")
    discovered_head = payload.get("headRefOid")
    if explicit_pr:
        if discovered_pr != str(ctx.state["pr_url"]):
            return {
                "ok": False,
                "mode": "canonical_url_mismatch",
                "error": f"canonical PR URL mismatch: requested={ctx.state['pr_url']!r} gh={discovered_pr!r}",
            }
        if not isinstance(discovered_head, str) or not discovered_head or discovered_head.lower() != local_head.lower():
            return {
                "ok": False,
                "mode": "head_sha_mismatch",
                "error": f"head SHA mismatch: local={local_head} gh={discovered_head}",
            }
        for field, supplied_head in supplied_heads.items():
            if supplied_head.lower() != discovered_head.lower():
                return {
                    "ok": False,
                    "mode": "head_sha_mismatch",
                    "error": f"head SHA mismatch: {field}={supplied_head} gh={discovered_head}",
                }
        ctx.state["pr_head_sha"] = discovered_head
        if not ctx.state.get("target_repo"):
            ctx.state["target_repo"] = explicit_repo or ""
        return {"ok": True, "source": "gh", "canonical_url": discovered_pr}
    if not explicit_pr and isinstance(discovered_pr, str) and _parse_pr_number(discovered_pr):
        ctx.state["pr_url"] = discovered_pr
    if not explicit_head and isinstance(discovered_head, str) and discovered_head:
        ctx.state["pr_head_sha"] = discovered_head
    pr_url = str(ctx.state.get("pr_url") or "")
    if not ctx.state.get("target_repo"):
        ctx.state["target_repo"] = _extract_pr_repo(pr_url) or ""
    if explicit_head and isinstance(discovered_head, str) and discovered_head and explicit_head.lower() != discovered_head.lower():
        return {
            "ok": False,
            "error": f"head SHA mismatch: state={explicit_head} gh={discovered_head}",
            "mode": "head_sha_mismatch",
        }
    return {"ok": True, "source": "gh"}


def _prepare_diff_path(repo_dir: pathlib.Path, pr_number: int) -> dict:
    """Create an owner-only temporary committed diff attachment."""
    try:
        merge_base = subprocess.run(
            ["git", "merge-base", "origin/main", "HEAD"],
            cwd=str(repo_dir), capture_output=True, text=True, timeout=10, check=False,
        )
        base = (merge_base.stdout or "").strip()
        if merge_base.returncode != 0 or not base or any(char not in "0123456789abcdefABCDEF" for char in base):
            return {"ok": False, "error": "could not resolve committed PR merge-base"}
        diff = subprocess.run(
            ["git", "diff", "--binary", f"{base}..HEAD"],
            cwd=str(repo_dir), capture_output=True, text=False, timeout=30, check=False,
        )
        if diff.returncode != 0:
            return {"ok": False, "error": (diff.stderr or b"git diff failed").decode(errors="replace")[:500]}
        fd, name = tempfile.mkstemp(prefix=f"pr_{pr_number}_", suffix="_full_diff.patch")
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(diff.stdout or b"")
            handle.flush()
            os.fsync(handle.fileno())
        return {"ok": True, "path": pathlib.Path(name)}
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"ok": False, "error": f"{type(exc).__name__}: {exc}"[:500]}
def _parse_pr_number(pr_url: str) -> Optional[int]:
    """Extract a PR number from an exact canonical GitHub PR URL.

    URL suffixes, query strings, and Git remote-style references are rejected.
    Returns ``None`` for unparseable or non-numeric inputs.
    """
    if not isinstance(pr_url, str) or not pr_url:
        return None
    match = _PR_URL_RE.fullmatch(pr_url)
    if match is None:
        return None
    try:
        return int(match.group("number"))
    except ValueError:
        return None


def _extract_pr_repo(pr_url: str) -> Optional[str]:
    """Extract ``owner/repo`` from a GitHub PR URL.

    Returns ``None`` for unparseable inputs. Used by `gh pr comment`'s
    ``--repo`` flag for explicit scoping.
    """
    if not isinstance(pr_url, str) or not pr_url:
        return None
    match = _PR_URL_RE.fullmatch(pr_url)
    if match is None:
        return None
    return f"{match.group('owner')}/{match.group('repo')}"


def _target_repo_error(target_repo: str, pr_repo: Optional[str]) -> Optional[str]:
    """Return a side-effect safety error for an unscoped target repository."""
    if not target_repo:
        return None
    if any(ord(char) < 32 for char in target_repo) or not re.fullmatch(
        r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", target_repo
    ):
        return "target_repo must be a single owner/repo value without control characters"
    if not pr_repo or target_repo != pr_repo:
        return f"target_repo {target_repo!r} does not match PR repository {pr_repo!r}"
    return None


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
    body_path: Optional[str] = None
    body_fd: Optional[int] = None
    try:
        body_fd, body_path = tempfile.mkstemp(prefix="web-advice-comment-", suffix=".md")
        os.fchmod(body_fd, 0o600)
        with os.fdopen(body_fd, "w", encoding="utf-8") as handle:
            body_fd = None
            handle.write(body)
            handle.flush()
            os.fsync(handle.fileno())
        cmd = ["gh", "pr", "comment", str(pr_number), "--body-file", body_path]
        if repo:
            cmd += ["--repo", repo]
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
    finally:
        if body_fd is not None:
            try:
                os.close(body_fd)
            except OSError:
                pass
        if body_path is not None:
            try:
                os.unlink(body_path)
            except OSError:
                pass


def _run_e2e_smoke(timeout: int = 30) -> dict:
    """Run the authoritative ``e2e_smoke.sh`` pre-flight check.

    The smoke is non-destructive and prints the full transport matrix. When
    present, a non-zero exit is authoritative: the handler records an
    infrastructure failure and short-circuits before invoking `/web-advice`.
    A missing script is explicitly reported as skipped.
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
    # Keep direct Claude dispatch on the same explicit project account policy
    # as codergen/gate lanes. Resolve through the handlers shim at call time so
    # tests and policy updates can replace the shared helper without import
    # cycles. A missing scope is an infrastructure failure; never inherit the
    # operator's personal Claude environment or invoke a bare subprocess.
    import runner.handlers as _handlers_shim

    try:
        env = _handlers_shim._scoped_claude_env()
    except (AttributeError, ValueError, OSError) as exc:
        return {
            "ok": False,
            "verdict": "infra_unavailable",
            "summary": "",
            "share_urls": [],
            "returncode": -1,
            "stderr": f"Claude scope unavailable: {exc}"[:500],
            "transport": transport,
        }
    cmd = ["claude", "--print", f"/web-advice {prompt}"]
    try:
        res = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=env,
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
    qualifying = [value for value in candidates if set(value) == _PANEL_CONTRACT_KEYS]
    if len(qualifying) != 1:
        return None
    return qualifying[0]


def _validate_share_url(url: str) -> tuple[bool, str]:
    """Validate a share URL before any network request is attempted."""
    valid, _host, _addresses, error = _validated_share_target(url)
    return valid, error


def _validated_share_target(url: str) -> tuple[bool, str, list[str], str]:
    """Validate URL shape and resolve every address before connecting."""
    if not isinstance(url, str) or any(ord(char) < 32 for char in url):
        return False, "", [], "URL is not a safe string"
    try:
        parsed = urllib.parse.urlsplit(url)
        hostname = parsed.hostname.lower() if parsed.hostname else ""
        port = parsed.port
    except ValueError:
        return False, "", [], "URL has invalid credentials or port"
    if parsed.scheme.lower() != "https":
        return False, hostname, [], "share URL must use https"
    if parsed.username is not None or parsed.password is not None:
        return False, hostname, [], "share URL credentials are forbidden"
    if port is not None:
        return False, hostname, [], "custom share URL ports are forbidden"
    pattern = _SHARE_PATH_PATTERNS.get(hostname)
    if pattern is None or not pattern.fullmatch(parsed.path) or parsed.query or parsed.fragment:
        return False, hostname, [], "share URL host or path is not an approved provider shape"
    try:
        addresses = socket.getaddrinfo(hostname, 443, type=socket.SOCK_STREAM)
    except OSError as exc:
        return False, hostname, [], f"share URL DNS resolution failed: {exc}"
    if not addresses:
        return False, hostname, [], "share URL DNS resolution returned no addresses"
    approved: list[str] = []
    for address in addresses:
        try:
            ip = ipaddress.ip_address(address[4][0])
        except (IndexError, ValueError, TypeError):
            return False, hostname, [], "share URL DNS returned an invalid address"
        if (
            not ip.is_global
            or
            ip.is_loopback
            or ip.is_private
            or ip.is_link_local
            or ip.is_reserved
            or ip.is_unspecified
            or ip.is_multicast
        ):
            return False, hostname, [], f"share URL resolves to unsafe private/reserved address {ip}"
        approved.append(str(ip))
    return True, hostname, approved, ""


def _curl_pinned_get(url: str, hostname: str, address: str, timeout: float) -> dict:
    """Fetch one URL with curl pinned to a validated address, no redirects."""
    # A pinned request must not escape through inherited proxy settings. Strip
    # all proxy variables in addition to curl's explicit no-proxy wildcard.
    clean_env = {
        key: value
        for key, value in os.environ.items()
        if "proxy" not in key.lower()
    }
    resolve_address = f"[{address}]" if ":" in address and not address.startswith("[") else address
    cmd = [
        "curl", "--silent", "--show-error", "--proto", "=https",
        "--noproxy", "*",
        "--connect-timeout", str(max(1, min(int(timeout), 10))),
        "--max-time", str(max(1, min(int(timeout), 10))),
        "--max-filesize", "65536",
        "--resolve", f"{hostname}:443:{resolve_address}", "--dump-header", "-", url,
    ]
    try:
        result = subprocess.run(
            cmd, capture_output=True, text=False, timeout=timeout + 1, check=False, env=clean_env,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"ok": False, "status": None, "headers": {}, "body": b"", "error": f"{type(exc).__name__}: {exc}"[:300]}
    raw = result.stdout or b""
    if isinstance(raw, str):
        raw = raw.encode()
    marker = raw.find(b"\r\n\r\n")
    separator_len = 4
    if marker < 0:
        marker = raw.find(b"\n\n")
        separator_len = 2
    header_bytes = raw[:marker] if marker >= 0 else raw
    body = raw[marker + separator_len: marker + separator_len + 65_536] if marker >= 0 else b""
    lines = header_bytes.decode("iso-8859-1", errors="replace").splitlines()
    status = None
    headers: dict[str, str] = {}
    if lines and lines[0].startswith("HTTP/"):
        parts = lines[0].split()
        if len(parts) >= 2 and parts[1].isdigit():
            status = int(parts[1])
        for line in lines[1:]:
            if ":" in line:
                key, value = line.split(":", 1)
                headers[key.strip().lower()] = value.strip()
    if result.returncode != 0:
        stderr = result.stderr or b"curl failed"
        if isinstance(stderr, str):
            stderr = stderr.encode()
        return {"ok": False, "status": status, "headers": headers, "body": body, "error": stderr.decode(errors="replace")[:300]}
    return {"ok": True, "status": status, "headers": headers, "body": body, "error": ""}


def _probe_share_url(url: str, *, timeout: float = 10.0) -> dict:
    """Probe a public Share URL with pinned-IP, manual no-follow requests."""
    valid, hostname, addresses, validation_error = _validated_share_target(url)
    if not valid:
        return {"ok": False, "status": None, "final_url": "", "error": validation_error}
    current_url = url
    for _hop in range(4):
        response = _curl_pinned_get(current_url, hostname, addresses[0], timeout)
        status = response.get("status")
        headers = response.get("headers") or {}
        content = response.get("body") or b""
        if not response.get("ok"):
            return {"ok": False, "status": status, "final_url": current_url, "error": response.get("error", "curl failed")}
        if status in {301, 302, 303, 307, 308}:
            location = headers.get("location")
            if not location:
                return {"ok": False, "status": status, "final_url": current_url, "error": "redirect missing Location"}
            next_url = urllib.parse.urljoin(current_url, location)
            next_valid, next_host, next_addresses, next_error = _validated_share_target(next_url)
            if not next_valid:
                return {"ok": False, "status": status, "final_url": next_url, "error": f"redirect rejected before connection: {next_error}"}
            current_url, hostname, addresses = next_url, next_host, next_addresses
            continue
        lowered_url = current_url.lower()
        if any(marker in lowered_url for marker in ("/login", "/signin", "/sign-in", "/auth/", "/account", "consent", "accounts.google.com")):
            return {"ok": False, "status": status, "final_url": current_url, "error": f"login redirect: {current_url}"}
        lowered_content = content.decode("utf-8", errors="ignore").lower()
        wall_markers = ("sign in to continue", "log in to continue", "authentication required", "consent required", "accounts.google.com/signin", "name=\"password\"")
        if any(marker in lowered_content for marker in wall_markers):
            return {"ok": False, "status": status, "final_url": current_url, "error": "authentication/consent wall in response content"}
        if status != 200:
            return {"ok": False, "status": status, "final_url": current_url, "error": f"HTTP {status}"}
        return {"ok": True, "status": status, "final_url": current_url, "error": ""}
    return {"ok": False, "status": None, "final_url": current_url, "error": "too many redirects"}


def _persist_share_evidence(
    evidence_dir: pathlib.Path,
    seats: dict,
    *,
    root_dir: pathlib.Path,
    pr_url: str = "",
    head_sha: str = "",
) -> pathlib.Path:
    """Persist evidence using held directory fds to prevent symlink TOCTOU."""
    root_dir = pathlib.Path(root_dir)
    evidence_dir = pathlib.Path(evidence_dir)
    if evidence_dir.is_absolute():
        raise ValueError("evidence_dir must be relative to the repository root")
    if not evidence_dir.parts or any(part in ("", ".", "..") for part in evidence_dir.parts):
        raise ValueError("evidence_dir must name a repository-relative directory")
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    directory = getattr(os, "O_DIRECTORY", 0)
    root_fd = os.open(root_dir, os.O_RDONLY | directory | nofollow)
    dir_fd = root_fd
    opened: list[int] = []
    try:
        for part in evidence_dir.parts:
            try:
                next_fd = os.open(part, os.O_RDONLY | directory | nofollow, dir_fd=dir_fd)
            except FileNotFoundError:
                try:
                    os.mkdir(part, 0o755, dir_fd=dir_fd)
                except FileExistsError:
                    pass
                next_fd = os.open(part, os.O_RDONLY | directory | nofollow, dir_fd=dir_fd)
            except (NotADirectoryError, OSError) as exc:
                raise ValueError("evidence_dir contains a symlink or non-directory component") from exc
            dir_fd = next_fd
            opened.append(next_fd)
        payload = {"schema_version": 1, "pr_url": pr_url, "head_sha": head_sha, **seats}
        target_name = "web-advice-share-urls.json"
        temp_name = None
        for attempt in range(20):
            candidate = f".{target_name}.{os.getpid()}.{attempt}.tmp"
            try:
                fd = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL | nofollow, 0o600, dir_fd=dir_fd)
                temp_name = candidate
                break
            except FileExistsError:
                continue
        if temp_name is None:
            raise FileExistsError("could not allocate evidence tempfile")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temp_name, target_name, src_dir_fd=dir_fd, dst_dir_fd=dir_fd)
            temp_name = None
            os.fsync(dir_fd)
        finally:
            if temp_name is not None:
                try:
                    os.unlink(temp_name, dir_fd=dir_fd)
                except OSError:
                    pass
    finally:
        for fd in reversed(opened):
            try:
                os.close(fd)
            except OSError:
                pass
        try:
            os.close(root_fd)
        except OSError:
            pass
    return root_dir.joinpath(*evidence_dir.parts, "web-advice-share-urls.json")


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
    """Validate the exact seven-key panel envelope and its seat summaries."""
    if not isinstance(verdict_data, dict):
        return {"ok": False, "panel_contract_valid": False, "panel_contract_error": "panel result must be an object"}
    envelope = {key: verdict_data.get(key) for key in _PANEL_CONTRACT_KEYS}
    raw_attempted = verdict_data.get("panel_seats_attempted")
    raw_live = verdict_data.get("panel_seats_live")
    raw_unavailable = verdict_data.get("panel_seats_unavailable")
    raw_reasons = verdict_data.get("panel_seats_unavailable_reasons")
    raw_summaries = verdict_data.get("panel_verdict_summary")
    contract_error = ""
    if set(verdict_data) != _PANEL_CONTRACT_KEYS:
        contract_error = "panel envelope must contain exactly the seven canonical keys"
    elif raw_attempted != list(PANEL_SEATS):
        contract_error = "panel_seats_attempted must equal the canonical seat order"
    elif not isinstance(raw_live, list) or not isinstance(raw_unavailable, list):
        contract_error = "panel seat live/unavailable partitions must be lists"
    elif len(set(raw_live)) != len(raw_live) or len(set(raw_unavailable)) != len(raw_unavailable):
        contract_error = "panel seat live/unavailable partitions must be unique"
    elif set(raw_live) | set(raw_unavailable) != set(PANEL_SEATS) or set(raw_live) & set(raw_unavailable):
        contract_error = "panel seat live/unavailable partitions must be an exact disjoint partition"
    elif not isinstance(raw_reasons, dict) or set(raw_reasons) != set(raw_unavailable) or any(
        not isinstance(raw_reasons.get(seat), str) or not raw_reasons[seat].strip()
        for seat in raw_unavailable
    ):
        contract_error = "every unavailable panel seat requires a non-empty reason"
    elif not isinstance(verdict_data.get("panel_convergence"), dict):
        contract_error = "panel_convergence must be an object"
    elif not isinstance(raw_summaries, dict) or set(raw_summaries) != set(raw_live):
        contract_error = "panel verdict summaries must have exactly the live seat keys"

    attempted = [seat for seat in (raw_attempted or []) if seat in PANEL_SEATS]
    live = [seat for seat in (raw_live or []) if seat in attempted]
    unavailable = [seat for seat in (raw_unavailable or []) if seat in attempted and seat not in live]
    reasons = dict(verdict_data.get("panel_seats_unavailable_reasons") or {})
    summaries = dict(verdict_data.get("panel_verdict_summary") or {})
    probes: dict[str, dict] = {}
    if not contract_error:
        for seat, summary in summaries.items():
            if not isinstance(summary, dict) or set(summary) != _PANEL_SUMMARY_KEYS:
                contract_error = "panel verdict summaries must have the exact summary keys"
                break
            if summary.get("verdict") not in _PANEL_VERDICTS:
                contract_error = "panel verdict verdict must be approve, not_merge, or insufficient"
                break
            if summary.get("confidence") not in _PANEL_CONFIDENCE:
                contract_error = "panel confidence must be low, medium, or high"
                break
            if not isinstance(summary.get("reasoning"), str) or not summary["reasoning"].strip():
                contract_error = "panel reasoning must be a non-empty string"
                break
            url = summary.get("share_url")
            if url is not None and not isinstance(url, str):
                contract_error = "panel share_url must be a string or null"
                break
            if not url:
                if summary.get("share_url_probe") is not None and not isinstance(summary.get("share_url_probe"), dict):
                    contract_error = "panel share_url_probe must be an object or null"
                if summary.get("share_url_probe") is not None:
                    # A probe without a URL is semantically meaningless.
                    contract_error = "panel share_url_probe requires share_url"
                continue
            probe = _probe_share_url(str(url))
            probes[seat] = probe
            summary["share_url_probe"] = probe
            if not probe.get("ok"):
                summary["share_url"] = None

    # Transitional flat URL support still probes every emitted URL, but does
    # not claim those URLs belong to all four seats.
    flat_urls = [str(url) for url in (verdict_data.get("share_urls") or []) if url]
    flat_probes = [_probe_share_url(url) for url in flat_urls] if not contract_error else []
    decision = verdict_data.get("decision")
    if decision not in _PANEL_DECISIONS:
        contract_error = contract_error or "decision is not a permitted fail-open value"
    return {
        **envelope,
        **verdict_data,
        "panel_seats_attempted": attempted,
        "panel_seats_live": live,
        "panel_seats_unavailable": unavailable,
        "panel_seats_unavailable_reasons": reasons,
        "panel_verdict_summary": summaries,
        "panel_contract_valid": not bool(contract_error),
        "panel_contract_error": contract_error,
        "share_url_probes": probes,
        "flat_share_url_probes": flat_probes,
        "share_urls": [url for url, probe in zip(flat_urls, flat_probes) if probe.get("ok")],
    }


def _file_followup_bead(*, target_repo: str, pr_number: int, decision: str, summary: str) -> dict:
    """File the model-requested follow-up bead with an unambiguous repo scope."""
    if not target_repo:
        return {"ok": False, "bead_id": None, "error": "missing target_repo"}
    if _target_repo_error(target_repo, target_repo):
        return {"ok": False, "bead_id": None, "error": "invalid target_repo"}
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
    finally:
        temp_path = ctx.state.pop("_web_advice_temp_diff_path", None)
        if temp_path:
            try:
                pathlib.Path(str(temp_path)).unlink(missing_ok=True)
            except OSError:
                pass


def _web_advice_inner(node: "Node", ctx: "Context", metadata: dict, started_ts: float) -> "Result":
    """Inner handler — the try/except above wraps this so any leak is caught."""

    # ------------------------------------------------------------------
    # 1. Inputs
    # ------------------------------------------------------------------
    seed = _seed_web_advice_state(ctx)
    if not seed.get("ok", True) and seed.get("mode") in {
        "invalid_pr_identity",
        "target_repo_mismatch",
        "canonical_url_mismatch",
        "local_head_unavailable",
    }:
        mode = seed.get("mode", "invalid_pr_identity")
        metadata.update({
            "web_advice_outcome": mode,
            "infrastructure_failure_mode": mode,
            "side_effect_error": seed.get("error", mode),
            "decision": "continue",
            "elapsed_seconds": round(time.time() - started_ts, 3),
        })
        if seed.get("mode") == "target_repo_mismatch":
            metadata["target_repo_error"] = seed.get("error", "target repository mismatch")
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice skipped: {seed.get('error', mode)}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={"web_advice.outcome": mode},
        )
    if seed.get("mode") == "head_sha_mismatch":
        metadata.update({
            "web_advice_outcome": "infrastructure_failure",
            "infrastructure_failure_mode": "head_sha_mismatch",
            "side_effect_error": seed.get("error", "head SHA mismatch"),
            "decision": "continue",
            "elapsed_seconds": round(time.time() - started_ts, 3),
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice head_sha_mismatch: {seed.get('error', '')}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "infrastructure_failure",
                "web_advice.infrastructure_failure_mode": "head_sha_mismatch",
            },
        )
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
    target_repo_error = _target_repo_error(target_repo, pr_repo)
    if target_repo_error:
        metadata["target_repo_error"] = target_repo_error
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

    target_repo_valid = bool(target_repo) and not target_repo_error

    def _persist_evidence(seats: dict) -> Optional[pathlib.Path]:
        requested = ctx.state.get("evidence_dir")
        relative_dir = pathlib.Path(str(requested)) if requested else pathlib.Path("docs") / f"pr{pr_number}-evidence"
        try:
            return _persist_share_evidence(
                relative_dir,
                seats,
                root_dir=repo_dir,
                pr_url=pr_url,
                head_sha=pr_head_sha,
            )
        except (OSError, ValueError) as exc:
            metadata["evidence_side_effect_error"] = f"{type(exc).__name__}: {exc}"[:500]
            return None

    def _emit_infra(mode: str, summary: str, *, smoke: Optional[dict] = None) -> dict:
        comment_result = _post_pr_comment(
            pr_number,
            _build_infra_disclosure_comment(summary, pr_url),
            repo=pr_repo,
        )
        decision = "continue_with_bead" if target_repo_valid else "continue"
        bead_result = (
            _file_followup_bead(
                target_repo=target_repo,
                pr_number=pr_number,
                decision=decision,
                summary=summary,
            )
            if target_repo_valid
            else {"ok": False, "bead_id": None, "error": target_repo_error or "missing target_repo"}
        )
        evidence_path = _persist_evidence({})
        metadata.update({
            "web_advice_outcome": "infrastructure_failure",
            "infrastructure_failure_mode": mode,
            "decision": decision,
            "panel_decision": decision,
            "bead_id": bead_result.get("bead_id"),
            "bead_filed": bead_result.get("ok", False),
            "share_urls_persisted_to": str(evidence_path) if evidence_path else None,
            "pr_comment_posted": comment_result["ok"],
            "pr_comment_rc": comment_result["returncode"],
            "pr_comment_url": comment_result.get("url"),
        })
        if smoke is not None:
            metadata.update({
                "e2e_smoke_returncode": smoke.get("returncode"),
                "e2e_smoke_stderr": smoke.get("stderr", ""),
            })
        return comment_result

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
        comment_result = _emit_infra("all_transports_down", probe["summary"])
        metadata["elapsed_seconds"] = elapsed
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

    if not ctx.state.get("diff_path"):
        diff_attachment = _prepare_diff_path(repo_dir, pr_number)
        if diff_attachment.get("ok"):
            ctx.state["diff_path"] = str(diff_attachment["path"])
            ctx.state["_web_advice_temp_diff_path"] = str(diff_attachment["path"])
        else:
            metadata["diff_side_effect_error"] = diff_attachment.get("error", "diff attachment unavailable")

    # ------------------------------------------------------------------
    # 4. e2e_smoke pre-flight (best-effort)
    # ------------------------------------------------------------------
    smoke = _run_e2e_smoke()
    metadata["e2e_smoke_ok"] = smoke["ok"]
    if not smoke.get("ok") and not smoke.get("skipped"):
        elapsed = round(time.time() - started_ts, 3)
        summary = (
            f"Authoritative e2e_smoke failed (returncode={smoke.get('returncode')}, "
            f"stderr={smoke.get('stderr', '')!r})"
        )
        comment_result = _emit_infra("e2e_smoke_failed", summary, smoke=smoke)
        metadata["elapsed_seconds"] = elapsed
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice e2e_smoke_failed: {summary}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "infrastructure_failure",
                "web_advice.infrastructure_failure_mode": "e2e_smoke_failed",
            },
        )

    # ------------------------------------------------------------------
    # 5. Run the actual /web-advice subprocess
    # ------------------------------------------------------------------
    prompt_text = _render_prompt(node, ctx)
    verdict_data = _run_web_advice_subprocess(
        transport=live_transport or "auto",
        prompt=prompt_text,
    )
    # The subprocess wrapper adds transport diagnostics around the exact
    # seven-key panel envelope. Validate only that envelope, then retain the
    # diagnostics for CXDB metadata and comments.
    raw_verdict_data = verdict_data
    verdict_data = {
        **raw_verdict_data,
        **_normalise_panel_result({key: raw_verdict_data.get(key) for key in _PANEL_CONTRACT_KEYS}),
    }
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

    # A failed claude invocation is infrastructure, not a captured verdict.
    # Emit the infra template immediately; do not post a misleading 0/0
    # verdict comment or persist an evidence artifact before this branch.
    if not verdict_data.get("ok"):
        elapsed = round(time.time() - started_ts, 3)
        summary = (
            f"/web-advice subprocess failed (returncode={verdict_data.get('returncode', -1)}, "
            f"stderr={verdict_data.get('stderr', '')!r})"
        )
        comment_result = _emit_infra("subprocess_failed", summary)
        metadata.update({
            "web_advice_outcome": "subprocess_failed",
            "infrastructure_failure_mode": "subprocess_failed",
            "decision": "continue_with_bead" if target_repo_valid else "continue",
            "panel_decision": "continue_with_bead" if target_repo_valid else "continue",
            "elapsed_seconds": elapsed,
        })
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice subprocess_failed: {summary}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "subprocess_failed",
                "web_advice.infrastructure_failure_mode": "subprocess_failed",
            },
        )

    if verdict_data.get("ok") and not verdict_data.get("panel_contract_valid", False):
        elapsed = round(time.time() - started_ts, 3)
        error = verdict_data.get("panel_contract_error") or "invalid panel contract"
        _emit_infra("panel_contract_invalid", f"Structured panel contract invalid: {error}")
        metadata.update({"verdict": "unknown", "elapsed_seconds": elapsed})
        _record_state(ctx, metadata)
        return Result(
            outcome="success",
            output=f"web_advice panel_contract_invalid: {error}",
            metadata={k: _coerce_meta(v) for k, v in metadata.items()},
            context_updates={
                "web_advice.outcome": "infrastructure_failure",
                "web_advice.infrastructure_failure_mode": "panel_contract_invalid",
            },
        )

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

    evidence_path = _persist_evidence(
        metadata["panel_verdict_summary"],
    )
    metadata["share_urls_persisted_to"] = str(evidence_path) if evidence_path else None
    if metadata["decision"] == "continue_with_bead":
        bead_result = _file_followup_bead(
            target_repo=target_repo,
            pr_number=pr_number,
            decision=metadata["decision"],
            summary=json.dumps(metadata["panel_verdict_summary"], ensure_ascii=False),
        ) if target_repo_valid else {"ok": False, "bead_id": None, "error": target_repo_error or "missing target_repo"}
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
