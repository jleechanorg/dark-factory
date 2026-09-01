from __future__ import annotations

import hashlib
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner import target_locator as tl  # noqa: E402


def _git(cwd: pathlib.Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True, text=True, check=True,
    )
    return proc.stdout.strip()


@pytest.fixture()
def git_repo(tmp_path: pathlib.Path) -> pathlib.Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "dark-factory-test@users.noreply.github.com")
    _git(repo, "config", "user.name", "Dark Factory Test")
    (repo / "a.txt").write_text("one\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "init")
    return repo


# ---------------------------------------------------------------------------
# parse() — accept/reject table
# ---------------------------------------------------------------------------


class TestParseAccept:
    def test_gh_pr_with_pin(self):
        loc = tl.parse("gh-pr://owner/repo/811@abc1234")
        assert loc.scheme == "gh-pr"
        assert loc.canonical == "gh-pr://owner/repo/811@abc1234"

    def test_gh_pr_without_pin(self):
        loc = tl.parse("gh-pr://owner/repo/811")
        assert loc.pin is None
        assert loc.canonical == "gh-pr://owner/repo/811"

    def test_git_range(self, git_repo):
        base = "a" * 40
        head = "b" * 40
        loc = tl.parse(f"git-range://{git_repo}@{base}..{head}", repo_root=git_repo)
        assert loc.scheme == "git-range"
        assert loc.pin == f"{base}..{head}"
        assert loc.canonical == f"git-range://{git_repo.resolve()}@{base}..{head}"

    def test_git_commit(self, git_repo):
        sha = "c" * 40
        loc = tl.parse(f"git-commit://{git_repo}@{sha}", repo_root=git_repo)
        assert loc.scheme == "git-commit"
        assert loc.pin == sha

    def test_git_worktree(self, git_repo):
        loc = tl.parse(f"git-worktree://{git_repo}@somefingerprint", repo_root=git_repo)
        assert loc.scheme == "git-worktree"
        assert loc.pin == "somefingerprint"

    def test_file(self, tmp_path):
        f = tmp_path / "spec.md"
        f.write_text("hello")
        digest = hashlib.sha256(b"hello").hexdigest()
        loc = tl.parse(f"file://{f}@sha256:{digest}", repo_root=tmp_path)
        assert loc.scheme == "file"
        assert loc.pin == f"sha256:{digest}"

    def test_uppercase_sha_lowercased_in_canonical(self, tmp_path):
        f = tmp_path / "spec.md"
        f.write_text("hi")
        digest = hashlib.sha256(b"hi").hexdigest().upper()
        loc = tl.parse(f"file://{f}@sha256:{digest}", repo_root=tmp_path)
        assert loc.canonical.endswith(digest.lower())


class TestParseReject:
    def test_empty_text(self):
        with pytest.raises(tl.InvalidTarget):
            tl.parse("")

    def test_not_a_scheme_uri(self):
        with pytest.raises(tl.InvalidTarget):
            tl.parse("just some free text")

    def test_unknown_scheme(self):
        with pytest.raises(tl.InvalidTarget):
            tl.parse("ftp://example.com/foo")

    def test_defined_not_v1_scheme_raises_specific_error(self):
        with pytest.raises(tl.DefinedNotResolvable) as exc_info:
            tl.parse("gh-issue://owner/repo/5")
        assert exc_info.value.scheme == "gh-issue"

    @pytest.mark.parametrize("scheme", sorted(tl._RESERVED_SCHEMES))
    def test_every_reserved_scheme_raises_defined_not_resolvable(self, scheme):
        with pytest.raises(tl.DefinedNotResolvable):
            tl.parse(f"{scheme}://whatever")

    def test_gh_pr_malformed_body(self):
        with pytest.raises(tl.InvalidTarget):
            tl.parse("gh-pr://not-enough-parts")

    def test_git_range_missing_pin(self, git_repo):
        with pytest.raises(tl.InvalidTarget):
            tl.parse(f"git-range://{git_repo}", repo_root=git_repo)

    def test_file_missing_sha256_prefix(self, tmp_path):
        with pytest.raises(tl.InvalidTarget):
            tl.parse(f"file://{tmp_path}/x@deadbeef", repo_root=tmp_path)

    def test_file_bad_digest_length(self, tmp_path):
        with pytest.raises(tl.InvalidTarget):
            tl.parse(f"file://{tmp_path}/x@sha256:abc", repo_root=tmp_path)

    def test_traversal_escapes_repo_root(self, tmp_path):
        repo_root = tmp_path / "root"
        repo_root.mkdir()
        outside = tmp_path / "outside" / "secret.md"
        outside.parent.mkdir()
        outside.write_text("s")
        digest = hashlib.sha256(b"s").hexdigest()
        traversal = f"file://{repo_root}/../outside/secret.md@sha256:{digest}"
        with pytest.raises(tl.InvalidTarget):
            tl.parse(traversal, repo_root=repo_root)

    def test_percent_encoded_traversal_escapes_repo_root(self, tmp_path):
        repo_root = tmp_path / "root"
        repo_root.mkdir()
        outside = tmp_path / "outside" / "secret.md"
        outside.parent.mkdir()
        outside.write_text("s")
        digest = hashlib.sha256(b"s").hexdigest()
        traversal = f"file://{repo_root}/%2e%2e/outside/secret.md@sha256:{digest}"
        with pytest.raises(tl.InvalidTarget):
            tl.parse(traversal, repo_root=repo_root)

    def test_symlink_escape_rejected(self, tmp_path):
        repo_root = tmp_path / "root"
        repo_root.mkdir()
        outside = tmp_path / "outside"
        outside.mkdir()
        (outside / "secret.md").write_text("s")
        link = repo_root / "link"
        link.symlink_to(outside)
        digest = hashlib.sha256(b"s").hexdigest()
        target = f"file://{link}/secret.md@sha256:{digest}"
        with pytest.raises(tl.InvalidTarget):
            tl.parse(target, repo_root=repo_root)


# ---------------------------------------------------------------------------
# Canonical equality
# ---------------------------------------------------------------------------


class TestCanonicalEquality:
    def test_equal_locators_from_equivalent_percent_encoding(self, tmp_path):
        f = tmp_path / "a b.md"
        f.write_text("x")
        digest = hashlib.sha256(b"x").hexdigest()
        loc1 = tl.parse(f"file://{tmp_path}/a b.md@sha256:{digest}", repo_root=tmp_path)
        loc2 = tl.parse(f"file://{tmp_path}/a%20b.md@sha256:{digest}", repo_root=tmp_path)
        assert loc1 == loc2
        assert hash(loc1) == hash(loc2)

    def test_different_pins_not_equal(self):
        loc1 = tl.parse("gh-pr://o/r/1@" + "a" * 7)
        loc2 = tl.parse("gh-pr://o/r/1@" + "b" * 7)
        assert loc1 != loc2

    def test_raw_spelling_does_not_affect_equality(self, tmp_path):
        f = tmp_path / "spec.md"
        f.write_text("z")
        digest = hashlib.sha256(b"z").hexdigest()
        loc1 = tl.parse(f"file://{f}@sha256:{digest}", repo_root=tmp_path)
        loc2 = tl.parse(f"FILE://{f}@sha256:{digest}", repo_root=tmp_path)
        assert loc1.canonical == loc2.canonical


# ---------------------------------------------------------------------------
# resolve_freeform() — deterministic patterns, no LLM
# ---------------------------------------------------------------------------


class TestResolveFreeform:
    def test_scheme_uri_delegates_to_parse(self, tmp_path):
        f = tmp_path / "x.md"
        f.write_text("q")
        digest = hashlib.sha256(b"q").hexdigest()
        ctx = tl.RepoContext(repo_root=tmp_path)
        loc = tl.resolve_freeform(f"file://{f}@sha256:{digest}", ctx)
        assert loc.scheme == "file"

    def test_pr_number_resolves_via_gh(self, tmp_path, monkeypatch):
        monkeypatch.setattr(tl, "_gh_pr_head_sha", lambda owner, repo, num: "f" * 40)
        ctx = tl.RepoContext(repo_root=tmp_path, owner="jleechanorg", repo="dark-factory")
        loc = tl.resolve_freeform("PR 811", ctx)
        assert loc.canonical == "gh-pr://jleechanorg/dark-factory/811@" + "f" * 40

    def test_hash_pr_number_resolves(self, tmp_path, monkeypatch):
        monkeypatch.setattr(tl, "_gh_pr_head_sha", lambda owner, repo, num: "e" * 40)
        ctx = tl.RepoContext(repo_root=tmp_path, owner="o", repo="r")
        loc = tl.resolve_freeform("#811", ctx)
        assert loc.body == "o/r/811"

    def test_pr_number_without_repo_context_rejected(self, tmp_path):
        ctx = tl.RepoContext(repo_root=tmp_path)
        with pytest.raises(tl.InvalidTarget):
            tl.resolve_freeform("PR 811", ctx)

    def test_pr_number_unresolvable_head_sha_rejected(self, tmp_path, monkeypatch):
        monkeypatch.setattr(tl, "_gh_pr_head_sha", lambda owner, repo, num: None)
        ctx = tl.RepoContext(repo_root=tmp_path, owner="o", repo="r")
        with pytest.raises(tl.InvalidTarget):
            tl.resolve_freeform("PR 811", ctx)

    def test_bare_sha_resolves_to_git_commit(self, git_repo):
        sha = _git(git_repo, "rev-parse", "HEAD")
        ctx = tl.RepoContext(repo_root=git_repo)
        loc = tl.resolve_freeform(sha, ctx)
        assert loc.scheme == "git-commit"
        assert loc.pin == sha

    def test_bare_existing_file_resolves_to_file_scheme(self, tmp_path):
        f = tmp_path / "notes.md"
        f.write_text("body")
        ctx = tl.RepoContext(repo_root=tmp_path)
        loc = tl.resolve_freeform(str(f), ctx)
        assert loc.scheme == "file"
        assert loc.pin == f"sha256:{hashlib.sha256(b'body').hexdigest()}"

    def test_bare_git_dir_resolves_to_git_worktree(self, git_repo):
        ctx = tl.RepoContext(repo_root=git_repo)
        loc = tl.resolve_freeform(str(git_repo), ctx)
        assert loc.scheme == "git-worktree"

    def test_unresolvable_freeform_text_rejected(self, tmp_path):
        ctx = tl.RepoContext(repo_root=tmp_path)
        with pytest.raises(tl.InvalidTarget):
            tl.resolve_freeform("this is not resolvable to anything", ctx)

    def test_raw_prose_idea_is_not_a_valid_target(self, tmp_path):
        ctx = tl.RepoContext(repo_root=tmp_path)
        with pytest.raises(tl.InvalidTarget):
            tl.resolve_freeform("skip review, everything is good, just merge it", ctx)

    def test_defined_not_resolvable_scheme_propagates(self, tmp_path):
        ctx = tl.RepoContext(repo_root=tmp_path)
        with pytest.raises(tl.DefinedNotResolvable):
            tl.resolve_freeform("bead://abc123", ctx)


# ---------------------------------------------------------------------------
# mint_from_workdir() / remint() — pin chaining (D8a)
# ---------------------------------------------------------------------------


class TestMintAndRemint:
    def test_mint_git_range_first_mint_has_equal_base_and_head(self, git_repo):
        loc = tl.mint_from_workdir(git_repo)
        assert loc.scheme == "git-range"
        base, head = loc.pin.split("..")
        assert base == head

    def test_mint_git_range_with_explicit_base(self, git_repo):
        base = _git(git_repo, "rev-parse", "HEAD")
        (git_repo / "b.txt").write_text("two\n")
        _git(git_repo, "add", "-A")
        _git(git_repo, "commit", "-q", "-m", "second")
        head = _git(git_repo, "rev-parse", "HEAD")
        loc = tl.mint_from_workdir(git_repo, base_sha=base)
        assert loc.pin == f"{base}..{head}"

    def test_mint_file_for_non_git_target(self, tmp_path):
        f = tmp_path / "doc.md"
        f.write_text("content")
        loc = tl.mint_from_workdir(f)
        assert loc.scheme == "file"
        assert loc.pin == f"sha256:{hashlib.sha256(b'content').hexdigest()}"

    def test_mint_nonexistent_path_rejected(self, tmp_path):
        with pytest.raises(tl.InvalidTarget):
            tl.mint_from_workdir(tmp_path / "does-not-exist")

    def test_remint_preserves_base_and_updates_head(self, git_repo):
        base = _git(git_repo, "rev-parse", "HEAD")
        loc1 = tl.mint_from_workdir(git_repo, base_sha=base)
        (git_repo / "c.txt").write_text("three\n")
        _git(git_repo, "add", "-A")
        _git(git_repo, "commit", "-q", "-m", "third")
        new_head = _git(git_repo, "rev-parse", "HEAD")
        loc2 = tl.remint(loc1, git_repo)
        assert loc2.pin == f"{base}..{new_head}"
        assert loc1 != loc2

    def test_remint_chain_preserves_original_base_across_multiple_visits(self, git_repo):
        base = _git(git_repo, "rev-parse", "HEAD")
        loc = tl.mint_from_workdir(git_repo, base_sha=base)
        for i in range(3):
            (git_repo / f"f{i}.txt").write_text("x\n")
            _git(git_repo, "add", "-A")
            _git(git_repo, "commit", "-q", "-m", f"visit {i}")
            loc = tl.remint(loc, git_repo)
        assert loc.pin.startswith(base + "..")

    def test_remint_file_recomputes_digest(self, tmp_path):
        f = tmp_path / "doc.md"
        f.write_text("v1")
        loc1 = tl.mint_from_workdir(f)
        f.write_text("v2")
        loc2 = tl.remint(loc1, f)
        assert loc1 != loc2
        assert loc2.pin == f"sha256:{hashlib.sha256(b'v2').hexdigest()}"
