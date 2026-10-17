#!/usr/bin/env python3
"""Focused tests for the real Chat delivery helper."""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch


HELPER_PATH = Path(__file__).with_name("chat-delivery.py")
_SPEC = importlib.util.spec_from_file_location("chat_delivery", HELPER_PATH)
assert _SPEC and _SPEC.loader
helper = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(helper)


class _APIHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 - stdlib handler API
        if self.path == "/api/v1/sessions/s1":
            body = {
                "session": {
                    "id": "s1",
                    "projectId": "proj",
                    "mode": "chat",
                    "harness": "codex",
                    "isTerminated": True,
                    "status": self.server.session_status,
                    "activity": self.server.session_activity,
                }
            }
        elif self.path == "/api/v1/projects/proj":
            body = {
                "status": "ok",
                "project": {
                    "id": "proj",
                    "path": str(self.server.repo),
                    "config": {"env": {"CODEX_HOME": str(self.server.home)}},
                },
            }
        else:
            self.send_error(404)
            return
        encoded = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, *_args):
        pass


class _Fixture:
    def __init__(self, *, existing=False):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        (self.repo / "README").write_text("fixture\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "README"], check=True)
        subprocess.run(
            [
                "git", "-C", str(self.repo), "-c", "user.name=Chat test",
                "-c", "user.email=chat-test@example.invalid", "commit", "-qm", "fixture",
            ],
            check=True,
        )
        subprocess.run(["git", "-C", str(self.repo), "branch", "ao/s1"], check=True)
        self.home = self.root / "codex-home"
        self.home.mkdir()
        self.ao_data = self.root / "ao" / "data"
        self.ao_data.mkdir(parents=True)
        self.db = self.ao_data / "ao.db"
        self.workspace = self.ao_data / "worktrees" / "proj" / "s1"
        self.workspace.parent.mkdir(parents=True)
        self.empty_proc = self.root / "empty-proc"
        self.empty_proc.mkdir()
        if existing:
            subprocess.run(
                ["git", "-C", str(self.repo), "worktree", "add", str(self.workspace), "ao/s1"],
                check=True,
                stdout=subprocess.DEVNULL,
            )
        with sqlite3.connect(self.db) as conn:
            conn.executescript(
                """
                CREATE TABLE sessions (
                    id TEXT, project_id TEXT, harness TEXT,
                    is_terminated INTEGER, session_mode TEXT,
                    workspace_path TEXT, provider_conversation_id TEXT,
                    branch TEXT, controller_generation TEXT, prompt TEXT,
                    created_at TEXT, activity_state TEXT
                );
                """
            )
            conn.execute(
                "INSERT INTO sessions VALUES (?, 'proj', 'codex', 1, 'chat', ?, 'thread-1', 'ao/s1', ?, 'repair', ?, 'exited')",
                ("s1", str(self.workspace), "generation-1", "2026-10-17T00:00:00Z"),
            )
        with sqlite3.connect(self.home / "state.sqlite") as conn:
            conn.execute("CREATE TABLE threads (id TEXT, cwd TEXT)")
            conn.execute("INSERT INTO threads VALUES (?, ?)", ("thread-1", str(self.workspace)))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _APIHandler)
        self.server.repo = self.repo
        self.server.home = self.home
        self.server.session_status = "terminated"
        self.server.session_activity = "exited"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.api = helper.API(f"http://127.0.0.1:{self.server.server_port}")

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        self.tempdir.cleanup()

    def prepare(self, path=None, proc=None, api=None):
        return helper.prepare_restore(
            api or self.api,
            str(self.db),
            "proj",
            "s1",
            str(self.home),
            path or self.root / "delivery.chat",
            "restore",
            proc or self.empty_proc,
        )


class _UnreadableProc:
    def iterdir(self):
        raise PermissionError("proc hidden")


class _SessionViewAPI:
    def __init__(self, api, view):
        self.api = api
        self.view = view

    def request(self, path):
        if path == "sessions/s1":
            return {"session": self.view}
        return self.api.request(path)


class _MutateBeforeFinalSessionRead:
    def __init__(self, fixture):
        self.fixture = fixture
        self.reads = 0

    def request(self, path):
        if path == "sessions/s1":
            self.reads += 1
            if self.reads == 2:
                with sqlite3.connect(self.fixture.db) as conn:
                    conn.execute(
                        "UPDATE sessions SET branch=?, activity_state=? WHERE id=?",
                        ("main", "running", "s1"),
                    )
        return self.fixture.api.request(path)


class RealHelperPreflightRed(unittest.TestCase):
    def fixture(self, *, existing=False):
        fixture = _Fixture(existing=existing)
        self.addCleanup(fixture.close)
        return fixture

    def test_intact_workspace_uses_normal_restore_reservation(self):
        fixture = self.fixture(existing=True)
        self.assertEqual(fixture.prepare(), 0)
        saved = json.loads((fixture.root / "delivery.chat").read_text())
        self.assertEqual(saved["restore"]["workspace_path"], str(fixture.workspace))

    def test_durable_owner_projection_keeps_legacy_ledger_shape(self):
        fixture = self.fixture()
        owner = helper.row(fixture.db, "proj", "s1")
        self.assertNotIn("branch", owner)
        self.assertNotIn("activity_state", owner)

    def test_public_api_activity_object_matches_ao_terminal_state(self):
        fixture = self.fixture()
        fixture.server.session_activity = {"state": "exited", "lastActivityAt": "2026-10-17T00:00:00Z"}
        self.assertEqual(fixture.prepare(), 0)

    def test_public_api_conflicting_status_and_activity_is_rejected(self):
        fixture = self.fixture()
        fixture.server.session_status = "working"
        fixture.server.session_activity = {"state": "exited"}
        with self.assertRaisesRegex(ValueError, "public session"):
            fixture.prepare()

    def test_missing_workspace_gets_durable_restore_reservation(self):
        fixture = self.fixture()
        self.assertEqual(fixture.prepare(), 0)
        saved = json.loads((fixture.root / "delivery.chat").read_text())
        self.assertEqual(saved["session"], "s1")
        self.assertEqual(saved["restore"]["workspace_path"], str(fixture.workspace))

    def test_pending_restore_ledger_suppresses_second_reservation(self):
        fixture = self.fixture()
        path = fixture.root / "delivery.chat"
        self.assertEqual(fixture.prepare(path), 0)
        before = path.read_bytes()
        self.assertEqual(fixture.prepare(path), 4)
        self.assertEqual(path.read_bytes(), before)

    def test_public_termination_conflict_is_rejected(self):
        fixture = self.fixture()
        view = {
            "id": "s1",
            "projectId": "proj",
            "mode": "chat",
            "harness": "codex",
            "isTerminated": True,
            "status": "exited",
            "activity": "running",
        }
        with self.assertRaisesRegex(ValueError, "terminated owner disagrees with public session"):
            fixture.prepare(api=_SessionViewAPI(fixture.api, view))

    def test_db_termination_conflict_is_rejected(self):
        fixture = self.fixture()
        with sqlite3.connect(fixture.db) as conn:
            conn.execute("UPDATE sessions SET activity_state=?", ("running",))
        with self.assertRaisesRegex(ValueError, "terminated native Chat owner unavailable"):
            fixture.prepare()

    def test_branch_and_activity_change_during_preflight_are_rejected(self):
        fixture = self.fixture()
        with self.assertRaisesRegex(ValueError, "terminated ownership or project changed during preflight"):
            fixture.prepare(api=_MutateBeforeFinalSessionRead(fixture))

    def test_exact_workspace_target_mismatch_is_rejected(self):
        fixture = self.fixture()
        wrong = fixture.root / "other-worktree"
        with sqlite3.connect(fixture.db) as conn:
            conn.execute("UPDATE sessions SET workspace_path=?", (str(wrong),))
        with self.assertRaisesRegex(ValueError, "AO-managed target"):
            fixture.prepare()

    def test_dangling_workspace_symlink_is_not_absence(self):
        fixture = self.fixture()
        fixture.workspace.symlink_to(fixture.root / "no-target")
        with self.assertRaisesRegex(ValueError, "existing owned worktree required"):
            fixture.prepare()

    def test_unreadable_workspace_parent_is_not_absence(self):
        fixture = self.fixture()
        os.chmod(fixture.workspace.parent, 0)
        try:
            with self.assertRaisesRegex(ValueError, "workspace visibility unavailable"):
                fixture.prepare()
        finally:
            os.chmod(fixture.workspace.parent, 0o700)

    def test_process_visibility_failure_is_fail_closed(self):
        fixture = self.fixture()
        with self.assertRaisesRegex(ValueError, "process visibility unavailable"):
            fixture.prepare(proc=_UnreadableProc())

    def test_unrelated_process_with_unreadable_environment_does_not_block_restore(self):
        fixture = self.fixture()
        proc = fixture.root / "proc"
        unrelated = proc / "123"
        unrelated.mkdir(parents=True)
        (unrelated / "cmdline").write_bytes(b"shell\0")
        original = Path.read_bytes

        def read_bytes(path):
            if path == unrelated / "environ":
                raise PermissionError("synthetic unrelated process")
            return original(path)

        with patch.object(Path, "read_bytes", read_bytes):
            self.assertEqual(fixture.prepare(proc=proc), 0)

    def test_same_session_id_in_other_project_or_codex_home_does_not_block_restore(self):
        fixture = self.fixture()
        proc = fixture.root / "proc"
        process = proc / "123"
        process.mkdir(parents=True)
        (process / "cmdline").write_bytes(b"codex\0app-server\0")
        executable = fixture.root / "codex"
        executable.write_text("fixture")
        (process / "exe").symlink_to(executable)
        (process / "environ").write_bytes(
            b"AO_SESSION_ID=s1\0AO_PROJECT_ID=other-project\0CODEX_HOME="
            + str(fixture.home).encode() + b"\0"
        )
        self.assertEqual(fixture.prepare(proc=proc), 0)

    def test_same_session_project_and_codex_home_process_blocks_restore(self):
        fixture = self.fixture()
        proc = fixture.root / "proc"
        process = proc / "123"
        process.mkdir(parents=True)
        (process / "cmdline").write_bytes(b"codex\0app-server\0")
        executable = fixture.root / "codex"
        executable.write_text("fixture")
        (process / "exe").symlink_to(executable)
        (process / "environ").write_bytes(
            b"AO_SESSION_ID=s1\0AO_PROJECT_ID=proj\0CODEX_HOME="
            + str(fixture.home).encode() + b"\0"
        )
        with self.assertRaisesRegex(ValueError, "existing Chat host still owns terminated session"):
            fixture.prepare(proc=proc)

    def test_provider_thread_mismatch_is_not_a_restore_fallback(self):
        fixture = self.fixture()
        with sqlite3.connect(fixture.home / "state.sqlite") as conn:
            conn.execute("UPDATE threads SET cwd=?", (str(fixture.root / "wrong"),))
        with self.assertRaisesRegex(ValueError, "provider workspace mismatch"):
            fixture.prepare()

    def test_prunable_registered_worktree_is_rejected_before_ao_restore(self):
        fixture = self.fixture(existing=True)
        shutil.rmtree(fixture.workspace)
        with self.assertRaisesRegex(ValueError, "prunable worktree registration"):
            fixture.prepare()

    def test_anonymous_spawn_reservation_survives_timeout_receipt(self):
        fixture = self.fixture()
        try:
            path = fixture.root / "delivery.json"
            token = "a" * 32
            receipt = path.with_name(path.name + ".spawn-" + token + ".log")
            receipt.write_text("context deadline exceeded\n")
            receipt.chmod(0o600)
            now = time.time()
            os.utime(receipt, (now - 600, now - 600))
            path.write_text(json.dumps({
                "session": "", "text": "repair", "initial": True, "spawnReceipt": token
            }))
            path.chmod(0o600)

            with self.assertRaisesRegex(ValueError, "ambiguous native spawn outcome"):
                helper.reserved_session(path)

            self.assertTrue(path.exists(), "uncertain spawn must keep its admission reservation")
        finally:
            fixture.close()

    def test_branch_registered_at_another_worktree_is_rejected_before_reservation(self):
        fixture = self.fixture(existing=True)
        subprocess.run(
            ["git", "-C", str(fixture.repo), "worktree", "remove", str(fixture.workspace)],
            check=True,
        )
        other = fixture.root / "other-worktree"
        subprocess.run(
            ["git", "-C", str(fixture.repo), "worktree", "add", str(other), "ao/s1"],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        path = fixture.root / "delivery.chat"
        with self.assertRaisesRegex(ValueError, "session branch is registered at another worktree"):
            fixture.prepare(path)
        self.assertFalse(path.exists(), "branch conflict must be rejected before reservation")

    def test_cli_emits_machine_readable_preflight_error(self):
        fixture = self.fixture()
        wrong = fixture.root / "wrong-worktree"
        with sqlite3.connect(fixture.db) as conn:
            conn.execute("UPDATE sessions SET workspace_path=?", (str(wrong),))
        proc = subprocess.run(
            [
                "python3", str(HELPER_PATH), "--prepare-restore",
                f"http://127.0.0.1:{fixture.server.server_port}", str(fixture.db),
                "proj", "s1", str(fixture.home), str(fixture.root / "delivery.chat"),
            ],
            input="restore", text=True, capture_output=True, check=False,
        )
        self.assertEqual(proc.returncode, 4)
        self.assertIn("CHAT_PREFLIGHT_ERROR=", proc.stderr)
        self.assertIn("workspace target does not match AO-managed target", proc.stderr)


if __name__ == "__main__":
    unittest.main()
