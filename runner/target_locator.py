"""``factory.review-target.v1`` — typed, mechanically-minted review targets.

Implements the target locator schema from
``docs/superpowers/specs/2026-09-01-factory-two-node-redesign-design.md``
(D3-D5): a strict parser + canonicalizer for the 5 v1-resolvable schemes
(``gh-pr``, ``git-range``, ``git-commit``, ``git-worktree``, ``file``), a
deterministic (no-LLM, no-network-by-default) freeform resolver for the CLI
boundary (D5), and mint/re-mint helpers used by ``handler_codergen.py`` to
freeze the post-worker pin chain (D3/D8a).

Fail-closed by design: every public function either returns a canonical
``Locator`` or raises a ``TargetLocatorError`` subclass. There is no
degraded/best-effort return value — callers (CLI, handler) must refuse to
start or advance the run on error, never fall back to unpinned text.
"""

from __future__ import annotations

import hashlib
import pathlib
import re
import subprocess
import urllib.parse
from dataclasses import dataclass
from typing import Optional


class TargetLocatorError(Exception):
    """Base for all target-locator failures."""


class InvalidTarget(TargetLocatorError):
    """Text does not match any defined scheme, or a v1-scheme locator fails
    its own shape/canonicalization rules (traversal, bad digest, ...)."""


class DefinedNotResolvable(TargetLocatorError):
    """Text matches a scheme defined by factory.review-target.v1, but that
    scheme is reserved/non-normative in v1 (D4: "defined, not yet
    resolvable")."""

    def __init__(self, scheme: str):
        super().__init__(f"scheme {scheme!r} is defined but not resolvable in v1")
        self.scheme = scheme


# v1-resolvable schemes (design table, "v1 resolvable" column = yes).
V1_SCHEMES = frozenset({"gh-pr", "git-range", "git-commit", "git-worktree", "file"})

# Defined-but-reserved schemes (design table, "v1 resolvable" column = no).
_RESERVED_SCHEMES = frozenset({
    "gh-issue", "git-repo", "directory", "bead", "url+sha256",
    "release", "factory-run", "evidence", "artifact", "entity",
})

DEFINED_SCHEMES = V1_SCHEMES | _RESERVED_SCHEMES

_SCHEME_RE = re.compile(r"^(?P<scheme>[a-zA-Z][a-zA-Z0-9+.\-]*)://(?P<rest>.*)$", re.DOTALL)

_GH_PR_RE = re.compile(r"^(?P<owner>[^/@]+)/(?P<repo>[^/@]+)/(?P<num>\d+)(?:@(?P<pin>[0-9a-f]{7,40}))?$")
_GIT_RANGE_RE = re.compile(r"^(?P<path>[^@]+)@(?P<base>[0-9a-f]{7,40})\.\.(?P<head>[0-9a-f]{7,40})$")
_GIT_COMMIT_RE = re.compile(r"^(?P<path>[^@]+)@(?P<sha>[0-9a-f]{7,40})$")
_GIT_WORKTREE_RE = re.compile(r"^(?P<path>/[^@]*)(?:@(?P<pin>\S+))?$")
_FILE_RE = re.compile(r"^(?P<path>/[^@]*)@sha256:(?P<digest>[0-9a-fA-F]{64})$")

_BARE_SHA_RE = re.compile(r"^[0-9a-f]{7,40}$", re.IGNORECASE)
_PR_REF_RE = re.compile(r"^(?:pr\s*#?|#)\s*(\d{1,7})$", re.IGNORECASE)


@dataclass(frozen=True, eq=False)
class Locator:
    """A canonicalized ``factory.review-target.v1`` locator.

    Equality/hashing are defined purely on ``canonical`` (design rule:
    "Two locators are equal iff their canonical strings are byte-equal").
    """

    scheme: str
    body: str
    pin: Optional[str]
    canonical: str
    raw: str = ""

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, Locator):
            return NotImplemented
        return self.canonical == other.canonical

    def __hash__(self) -> int:
        return hash(self.canonical)

    def __str__(self) -> str:
        return self.canonical


class RepoContext:
    """Ambient context for freeform resolution (D5). No LLM calls; ``owner``/
    ``repo`` are only required for PR-shaped freeform text."""

    def __init__(self, repo_root: "pathlib.Path | str", owner: str = "", repo: str = ""):
        self.repo_root = pathlib.Path(repo_root)
        self.owner = owner
        self.repo = repo


# ---------------------------------------------------------------------------
# Canonicalization
# ---------------------------------------------------------------------------


def _canon_path(raw_path: str, repo_root: Optional[pathlib.Path]) -> pathlib.Path:
    """Realpath + RFC3986-decode + reject-root-escape (design "Normative v1
    locator semantics: Canonicalization")."""
    decoded = urllib.parse.unquote(raw_path)
    if not decoded:
        raise InvalidTarget("locator path is empty")
    p = pathlib.Path(decoded)
    if not p.is_absolute():
        if repo_root is None:
            raise InvalidTarget(f"relative path {decoded!r} requires a repo_root")
        p = pathlib.Path(repo_root) / p
    try:
        resolved = p.resolve(strict=False)
    except (OSError, RuntimeError) as exc:
        raise InvalidTarget(f"cannot resolve path {decoded!r}: {exc}") from exc
    if repo_root is not None:
        root_resolved = pathlib.Path(repo_root).resolve(strict=False)
        if resolved != root_resolved and root_resolved not in resolved.parents:
            raise InvalidTarget(f"path escapes repository root: {resolved}")
    return resolved


def _encode_path(path: pathlib.Path) -> str:
    return urllib.parse.quote(str(path), safe="/:")


# ---------------------------------------------------------------------------
# Strict parser (D3/D4)
# ---------------------------------------------------------------------------


def parse(text: str, *, repo_root: "pathlib.Path | str | None" = None) -> Locator:
    """Strict parser for ``factory.review-target.v1`` locators.

    v1-resolvable schemes are canonicalized and returned as a ``Locator``.
    Schemes defined by the schema but not yet resolvable raise
    ``DefinedNotResolvable``. Anything else (malformed URI, unknown scheme,
    malformed body for a v1 scheme) raises ``InvalidTarget`` — the CLI
    boundary (D5) then tries ``resolve_freeform``, or refuses to start.
    """
    if not isinstance(text, str) or not text.strip():
        raise InvalidTarget("empty target text")
    m = _SCHEME_RE.match(text.strip())
    if not m:
        raise InvalidTarget(f"not a scheme://locator string: {text!r}")
    scheme = m.group("scheme").lower()
    rest = m.group("rest")
    if scheme not in DEFINED_SCHEMES:
        raise InvalidTarget(f"unknown scheme {scheme!r}")
    if scheme not in V1_SCHEMES:
        raise DefinedNotResolvable(scheme)

    root = pathlib.Path(repo_root) if repo_root is not None else None

    if scheme == "gh-pr":
        gm = _GH_PR_RE.match(rest)
        if not gm:
            raise InvalidTarget(f"gh-pr:// requires owner/repo/N[@sha], got: {rest!r}")
        owner, repo, num = gm.group("owner"), gm.group("repo"), gm.group("num")
        pin = gm.group("pin").lower() if gm.group("pin") else None
        body = f"{owner}/{repo}/{num}"
        canonical = f"gh-pr://{body}" + (f"@{pin}" if pin else "")
        return Locator("gh-pr", body, pin, canonical, raw=text)

    if scheme == "git-range":
        gm = _GIT_RANGE_RE.match(rest)
        if not gm:
            raise InvalidTarget(f"git-range:// requires path@<base-sha>..<head-sha>, got: {rest!r}")
        path = _canon_path(gm.group("path"), root)
        base, head = gm.group("base").lower(), gm.group("head").lower()
        canonical = f"git-range://{_encode_path(path)}@{base}..{head}"
        return Locator("git-range", str(path), f"{base}..{head}", canonical, raw=text)

    if scheme == "git-commit":
        gm = _GIT_COMMIT_RE.match(rest)
        if not gm:
            raise InvalidTarget(f"git-commit:// requires path@sha, got: {rest!r}")
        path = _canon_path(gm.group("path"), root)
        sha = gm.group("sha").lower()
        canonical = f"git-commit://{_encode_path(path)}@{sha}"
        return Locator("git-commit", str(path), sha, canonical, raw=text)

    if scheme == "git-worktree":
        gm = _GIT_WORKTREE_RE.match(rest)
        if not gm:
            raise InvalidTarget(f"git-worktree:// requires an absolute path, got: {rest!r}")
        path = _canon_path(gm.group("path"), root)
        pin = gm.group("pin")
        canonical = f"git-worktree://{_encode_path(path)}" + (f"@{pin}" if pin else "")
        return Locator("git-worktree", str(path), pin, canonical, raw=text)

    if scheme == "file":
        fm = _FILE_RE.match(rest)
        if not fm:
            raise InvalidTarget(f"file:// requires /abs/path@sha256:<64-hex>, got: {rest!r}")
        path = _canon_path(fm.group("path"), root)
        digest = fm.group("digest").lower()
        canonical = f"file://{_encode_path(path)}@sha256:{digest}"
        return Locator("file", str(path), f"sha256:{digest}", canonical, raw=text)

    raise InvalidTarget(f"unhandled v1 scheme {scheme!r}")  # pragma: no cover


# ---------------------------------------------------------------------------
# git helpers (offline, local subprocess only — no network)
# ---------------------------------------------------------------------------


def _run_git(args: list[str], cwd: "pathlib.Path | str", timeout: int = 15) -> Optional[str]:
    try:
        proc = subprocess.run(
            ["git", "-C", str(cwd), *args],
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout.strip()


def _git_head_sha(repo_path: "pathlib.Path | str") -> Optional[str]:
    return _run_git(["rev-parse", "HEAD"], repo_path)


def _worktree_fingerprint(path: "pathlib.Path | str") -> Optional[str]:
    head = _git_head_sha(path)
    if head is None:
        return None
    diff = _run_git(["diff", "--binary"], path, timeout=30) or ""
    staged = _run_git(["diff", "--cached", "--binary"], path, timeout=30) or ""
    dirty_digest = hashlib.sha256((diff + "\0" + staged).encode("utf-8")).hexdigest()
    return f"{head}+{dirty_digest[:16]}"


def _gh_pr_head_sha(owner: str, repo: str, num: str) -> Optional[str]:
    """Ambient-credential ``gh`` CLI lookup (D "Authorization"). No raw
    network fetch; delegates to the operator's authenticated `gh`."""
    try:
        proc = subprocess.run(
            ["gh", "pr", "view", num, "--repo", f"{owner}/{repo}", "--json", "headRefOid", "-q", ".headRefOid"],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    sha = proc.stdout.strip()
    return sha or None


# ---------------------------------------------------------------------------
# Freeform resolution (D5) — deterministic pattern matching, no LLM calls.
# ---------------------------------------------------------------------------


def resolve_freeform(text: str, repo_ctx: "RepoContext") -> Locator:
    """Deterministic pattern-matching resolution for CLI-boundary freeform
    target text (D5). Never calls an LLM. Raises ``InvalidTarget`` /
    ``DefinedNotResolvable`` when the text cannot be resolved mechanically —
    the caller must refuse to start the run rather than degrade to raw text.
    """
    if not isinstance(text, str) or not text.strip():
        raise InvalidTarget("empty freeform target text")
    stripped = text.strip()

    # Already a well-formed scheme://locator — delegate straight to parse().
    if _SCHEME_RE.match(stripped):
        return parse(stripped, repo_root=repo_ctx.repo_root if repo_ctx else None)

    # "PR 811" / "#811" / "PR#811" -> gh-pr, pin resolved via ambient `gh`.
    pm = _PR_REF_RE.match(stripped)
    if pm:
        if not (repo_ctx and repo_ctx.owner and repo_ctx.repo):
            raise InvalidTarget("PR reference requires repo context (owner/repo)")
        num = pm.group(1)
        head_sha = _gh_pr_head_sha(repo_ctx.owner, repo_ctx.repo, num)
        if not head_sha:
            raise InvalidTarget(f"could not resolve head SHA for PR {num}")
        return parse(f"gh-pr://{repo_ctx.owner}/{repo_ctx.repo}/{num}@{head_sha}")

    # Bare full/short SHA -> git-commit, anchored at repo_ctx.repo_root.
    if _BARE_SHA_RE.match(stripped):
        if not (repo_ctx and repo_ctx.repo_root):
            raise InvalidTarget("commit SHA reference requires a repo root")
        return parse(
            f"git-commit://{repo_ctx.repo_root}@{stripped.lower()}",
            repo_root=repo_ctx.repo_root,
        )

    # Bare path -> file:// (existing file) or git-worktree:// (existing dir).
    candidate = pathlib.Path(stripped).expanduser()
    if not candidate.is_absolute() and repo_ctx and repo_ctx.repo_root:
        candidate = repo_ctx.repo_root / candidate
    if candidate.is_file():
        digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        return parse(
            f"file://{candidate}@sha256:{digest}",
            repo_root=repo_ctx.repo_root if repo_ctx else None,
        )
    if candidate.is_dir():
        fingerprint = _worktree_fingerprint(candidate)
        if fingerprint is None:
            raise InvalidTarget(f"path is not a resolvable git worktree: {candidate}")
        return parse(
            f"git-worktree://{candidate}@{fingerprint}",
            repo_root=repo_ctx.repo_root if repo_ctx else None,
        )

    raise InvalidTarget(f"could not resolve freeform target text: {text!r}")


# ---------------------------------------------------------------------------
# Mint / re-mint (D3/D8a) — post-worker pin chain
# ---------------------------------------------------------------------------


def mint_from_workdir(workdir: "pathlib.Path | str", base_sha: Optional[str] = None) -> Locator:
    """Mint a locator freezing the current state of ``workdir`` (D3).

    Git repo -> ``git-range://<repo-root>@<base>..<head>``; ``base`` defaults
    to the current HEAD when not supplied (first mint of a task-mode run: an
    empty range at the pre-worker HEAD). Non-git single file ->
    ``file://<abs path>@sha256:<digest>``.
    """
    path = pathlib.Path(workdir).resolve(strict=False)
    if path.is_dir():
        head = _git_head_sha(path)
        if head is None:
            raise InvalidTarget(f"cannot mint target: {path} is not a git repository")
        base = (base_sha or head).lower()
        return parse(f"git-range://{path}@{base}..{head.lower()}", repo_root=path)
    if path.is_file():
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        return parse(f"file://{path}@sha256:{digest}", repo_root=path.parent)
    raise InvalidTarget(f"cannot mint target: {path} does not exist")


def remint(locator: Locator, workdir: "pathlib.Path | str") -> Locator:
    """Re-mint ``locator`` at the current state of ``workdir``, preserving
    the original base SHA for a git-range pin chain (D8a): reviewer visit N
    always receives the pin minted after worker visit N, never a stale pin.
    """
    if locator.scheme == "git-range":
        base = locator.pin.split("..", 1)[0] if locator.pin else None
        return mint_from_workdir(workdir, base_sha=base)
    if locator.scheme == "file":
        return mint_from_workdir(workdir)
    if locator.scheme == "git-commit":
        head = _git_head_sha(workdir)
        if head is None:
            raise InvalidTarget(f"cannot re-mint git-commit target: {workdir} is not a git repo")
        return parse(f"git-commit://{pathlib.Path(workdir).resolve()}@{head}", repo_root=workdir)
    if locator.scheme == "git-worktree":
        fingerprint = _worktree_fingerprint(workdir)
        if fingerprint is None:
            raise InvalidTarget(f"cannot re-mint git-worktree target: {workdir}")
        return parse(f"git-worktree://{pathlib.Path(workdir).resolve()}@{fingerprint}", repo_root=workdir)
    raise InvalidTarget(f"re-mint not supported for scheme {locator.scheme!r}")
