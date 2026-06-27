"""Engine-computed lint facts injected into review prompts (F5, jleechan-zba).

Closes the F5 gap surfaced by the 2026-06-22 /factory-evolve proposals: the
in-pipeline reviewer prompt sees only `${diff}` and the implementing agent's
prompt template. Cold reviewers (codex, Bugbot, CodeRabbit, /reviewdeep) flag
patterns the engine could have detected but didn't — `datetime.utcnow()`
deprecation, `zizmor` template-injection findings, missing `video=` config
in test rigs. This module makes the engine compute those findings up-front
and inject them via `${lint_findings}` so the cold-style reviewer prompt
can grade them on first read.

Output contract:
  * `lint_findings(workdir)` returns `list[dict]`. Each dict has:
      - `pattern_id`  — short stable id (e.g. "py_datetime_utcnow").
      - `severity`    — "warn" | "fail" (matches the gate verdict vocabulary).
      - `file`        — path relative to `workdir` (or absolute for
                        outside-tree matches).
      - `line`        — 1-indexed line number (or 0 if not line-scoped).
      - `snippet`     — one-line excerpt of the matching text.
      - `rationale`   — short human-readable explanation / link.

  * The dict is JSON-serializable so it can be stashed in `ctx.state`
    and rendered as Markdown via the `${lint_findings}` substitution.

  * `findings_to_markdown(findings)` renders a deterministic Markdown
    table (or `(no lint findings)`) for prompt injection.

Patterns covered (the 3 that caught real PRs in the 2026-06-22 window):
  * `py_datetime_utcnow` — `datetime.utcnow()` is deprecated in 3.12+.
  * `gh_zizmor_template` — `template-injection` zizmor finding.
  * `ios_video_config`   — `video=` in iOS simctl config (tutorial recorder).

Benchmark-shape bias guard (jleechan-fpi, audit-2026-06-27 Lane F):
  Patterns that target a *specific benchmark family* (iOS simctl, Android
  emulator, Firebase emulator) MUST NOT fire globally. Without the path
  gate the `ios_video_config` pattern fired on every JSON file containing
  `video=` anywhere in the workdir — leaking the iOS benchmark shape into
  fibonacci / hello / roman / generic reviewer prompts. The fix: a
  pattern entry may declare a `path_filter` regex; the scanner only emits
  a finding when the lowercased file path (absolute, or relative to the
  workdir) contains at least one match. iOS is gated to `(ios|simctl)`;
  global patterns (datetime, zizmor) set `path_filter=None`.

Adding a new pattern = add a `_LINT_PATTERNS` entry + a unit test. No
prompt or runner change required.
"""

from __future__ import annotations

import json
import pathlib
import re
from typing import Optional


# (pattern_id, regex, severity, rationale, file_glob, path_filter)
# `path_filter`: None → fires anywhere. Otherwise the lowercased file path
# (absolute, then relative-to-workdir fallback) must contain at least one
# match of this regex as a substring search — used to scope benchmark-family
# patterns (iOS, Android, etc.) so they don't leak into generic reviews.
_LINT_PATTERNS: list[tuple[str, str, str, str, Optional[str], Optional[str]]] = [
    (
        "py_datetime_utcnow",
        r"\bdatetime\s*\.\s*utcnow\s*\(",
        "fail",
        "datetime.utcnow() is deprecated in Python 3.12+; use "
        "datetime.now(timezone.utc) instead.",
        "*.py",
        None,
    ),
    (
        "gh_zizmor_template",
        r"\$\{\{[^}]+\}\}",
        "fail",
        "GitHub Actions template-injection finding from zizmor; "
        "${{ ... }} interpolates untrusted input directly into shell.",
        "*.yml",
        None,
    ),
    (
        "ios_video_config",
        r"\bvideo\s*=",
        "warn",
        "iOS simctl video= config detected; ensure the recorder is "
        "configured for the right codec/mask before merging.",
        "*.json",
        # Path-gate: only fire when the file path looks iOS / simctl.
        # Without this gate a fibonacci worktree with a stray `video=1280`
        # in fibonacci.json would trip the warning in every reviewer's
        # prompt (jleechan-fpi, audit-2026-06-27).
        r"ios|simctl",
    ),
]


def _path_matches_filter(path: pathlib.Path, workdir: pathlib.Path, path_filter: str) -> bool:
    """Return True iff `path` (absolute or workdir-relative) matches the filter.

    `path_filter` is a regex (e.g. ``r"ios|simctl"``) applied as a case-
    insensitive search against either the absolute path or the workdir-
    relative path. We check both because workdir names like
    `fibonacci-bench-2026-06-26` themselves never contain `ios`/`simctl`
    but the file path inside the workdir does, and conversely a workdir
    named `ios-simulator-bench` should gate matches even if the file is
    at the workdir root.
    """
    compiled = re.compile(path_filter, re.IGNORECASE)
    haystacks = [str(path)]
    try:
        rel = path.relative_to(workdir)
        haystacks.append(str(rel))
    except ValueError:
        pass
    return any(compiled.search(h) for h in haystacks)


def _scan_file(
    path: pathlib.Path,
    pattern: re.Pattern[str],
    glob: Optional[str],
    workdir: pathlib.Path,
    path_filter: Optional[str],
) -> list[dict]:
    """Run a single regex over a single file, returning zero or more findings."""
    if path_filter is not None and not _path_matches_filter(path, workdir, path_filter):
        return []
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except (OSError, UnicodeDecodeError):
        return []
    findings: list[dict] = []
    for match in pattern.finditer(text):
        line_no = text.count("\n", 0, match.start()) + 1
        line_start = text.rfind("\n", 0, match.start()) + 1
        line_end = text.find("\n", match.start())
        if line_end == -1:
            line_end = len(text)
        snippet = text[line_start:line_end].strip()
        findings.append({
            "file": str(path),
            "line": line_no,
            "snippet": snippet[:200],
        })
    return findings


def lint_findings(workdir: pathlib.Path) -> list[dict]:
    """Return a deduplicated list of lint findings for `workdir`.

    Scans files matching each pattern's glob up to 5 levels deep. Designed
    to run in < 1s for a typical slim-graph workdir; if a glob matches more
    than 1000 files we cap the scan and return early with a `truncated=true`
    note (caller can decide whether to surface that to the reviewer).

    Patterns with a non-None `path_filter` only emit findings for files
    whose path contains the filter (case-insensitive substring search
    against absolute or workdir-relative path). This scopes benchmark-
    family patterns (iOS simctl, Android emulator) so they don't leak
    into generic reviewer prompts.
    """
    workdir = pathlib.Path(workdir)
    if not workdir.is_dir():
        return []

    findings: list[dict] = []
    seen: set[tuple[str, str, int]] = set()
    truncated = False
    max_files_per_pattern = 1000

    for pattern_id, regex, severity, rationale, glob, path_filter in _LINT_PATTERNS:
        compiled = re.compile(regex)
        files_scanned = 0
        for path in workdir.rglob(glob or "*"):
            if not path.is_file():
                continue
            files_scanned += 1
            if files_scanned > max_files_per_pattern:
                truncated = True
                break
            for finding in _scan_file(path, compiled, glob, workdir, path_filter):
                key = (pattern_id, finding["file"], finding["line"])
                if key in seen:
                    continue
                seen.add(key)
                findings.append({
                    "pattern_id": pattern_id,
                    "severity": severity,
                    "file": finding["file"],
                    "line": finding["line"],
                    "snippet": finding["snippet"],
                    "rationale": rationale,
                })

    if truncated:
        findings.append({
            "pattern_id": "_scan_truncated",
            "severity": "warn",
            "file": "(scan)",
            "line": 0,
            "snippet": "",
            "rationale": "Lint scan hit the 1000-file-per-pattern cap; "
                         "some findings may be missing. Widen the workdir "
                         "or split the diff before retrying.",
        })
    return findings


def findings_to_markdown(findings: list[dict]) -> str:
    """Render findings as a deterministic Markdown block for prompt injection.

    Stable column order (pattern_id, severity, file:line, snippet, rationale)
    so the reviewer prompt can pin a regex against the rendered text in
    tests without false-positive churn on column reorder.
    """
    if not findings:
        return "## Lint findings (engine-computed, F5)\n\n(none)\n"
    rows = ["| pattern_id | severity | location | snippet | rationale |",
            "|---|---|---|---|---|"]
    for f in findings:
        loc = f"{f['file']}:{f['line']}" if f["line"] else f["file"]
        snippet = f["snippet"].replace("|", "\\|")[:120]
        rationale = f["rationale"].replace("|", "\\|")[:200]
        rows.append(
            f"| `{f['pattern_id']}` | {f['severity']} | `{loc}` | "
            f"`{snippet}` | {rationale} |"
        )
    return "## Lint findings (engine-computed, F5)\n\n" + "\n".join(rows) + "\n"


def findings_to_json(findings: list[dict]) -> str:
    """JSON-encoded findings for `ctx.state` stash / debug round-trip."""
    return json.dumps(findings, indent=2, sort_keys=True)
