import unittest
from unittest.mock import patch, MagicMock
from runner.scm_provider import LocalGitScm

class TestScmProvider(unittest.TestCase):
    @patch("subprocess.run")
    def test_local_git_diff(self, mock_run):
        mock_run.side_effect = [
            MagicMock(returncode=0, stdout="origin/pr/228\n"),
            MagicMock(returncode=0, stdout="main_sha\n"),
            MagicMock(returncode=0, stdout="diff content\n")
        ]
        scm = LocalGitScm(workdir="/fake")
        diff = scm.get_diff("PR:228")
        self.assertEqual(diff, "diff content\n")

    @patch("subprocess.run")
    def test_local_git_changed_files(self, mock_run):
        mock_run.side_effect = [
            MagicMock(returncode=1, stdout=""),
            MagicMock(returncode=1, stdout=""),
            MagicMock(code=1, stdout=""),
            MagicMock(code=1, stdout=""),
            MagicMock(returncode=0, stdout="main_sha\n"),
            MagicMock(returncode=0, stdout="file1.py\nfile2.py\n")
        ]
        scm = LocalGitScm(workdir="/fake")
        files = scm.get_changed_files("PR:228")
        self.assertEqual(files, ["file1.py", "file2.py"])
