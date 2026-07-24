from __future__ import annotations
import subprocess
from pathlib import Path
from typing import List
from abc import ABC, abstractmethod

class ScmProvider(ABC):
    @abstractmethod
    def get_diff(self, target: str) -> str:
        """Get the diff for the target. Target can be PR:<num>, COMMIT:<sha>, or BRANCH:<name>."""
        pass

    @abstractmethod
    def get_changed_files(self, target: str) -> List[str]:
        """Get the list of changed files for the target."""
        pass

class LocalGitScm(ScmProvider):
    def __init__(self, workdir: str | Path, base_branch: str = "origin/main"):
        self.workdir = Path(workdir)
        self.base_branch = base_branch

    def _resolve_head(self, target: str) -> str:
        """Resolve target to a local git ref/commit/branch."""
        if target.startswith("PR:"):
            # Target is PR number. Try to find a local ref or fallback to HEAD.
            pr_num = target.split(":", 1)[1]
            # Try origin/pr/num, pr/num, etc. If not found, use HEAD.
            for ref in [f"origin/pr/{pr_num}", f"pr/{pr_num}", f"private/cb-pr{pr_num}", f"private/cb-skeptic"]:
                proc = subprocess.run(
                    ["git", "rev-parse", "--verify", ref],
                    cwd=str(self.workdir),
                    capture_output=True,
                    text=True
                )
                if proc.returncode == 0 and proc.stdout.strip():
                    return ref
            return "HEAD"
        elif target.startswith("COMMIT:") or target.startswith("BRANCH:"):
            return target.split(":", 1)[1]
        return target

    def get_diff(self, target: str) -> str:
        head = self._resolve_head(target)
        # Resolve merge base
        mb_proc = subprocess.run(
            ["git", "merge-base", self.base_branch, head],
            cwd=str(self.workdir),
            capture_output=True,
            text=True,
            check=True
        )
        mb = mb_proc.stdout.strip()
        # Diff content
        diff_proc = subprocess.run(
            ["git", "diff", f"{mb}..{head}"],
            cwd=str(self.workdir),
            capture_output=True,
            text=True,
            check=True
        )
        return diff_proc.stdout

    def get_changed_files(self, target: str) -> List[str]:
        head = self._resolve_head(target)
        # Resolve merge base
        mb_proc = subprocess.run(
            ["git", "merge-base", self.base_branch, head],
            cwd=str(self.workdir),
            capture_output=True,
            text=True,
            check=True
        )
        mb = mb_proc.stdout.strip()
        # Changed files
        files_proc = subprocess.run(
            ["git", "diff", "--name-only", f"{mb}..{head}"],
            cwd=str(self.workdir),
            capture_output=True,
            text=True,
            check=True
        )
        return [line.strip() for line in files_proc.stdout.splitlines() if line.strip()]
