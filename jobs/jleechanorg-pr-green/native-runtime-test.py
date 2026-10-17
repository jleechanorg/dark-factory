#!/usr/bin/env python3
"""Compatibility tests for AO session-mode discovery."""
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest


HELPER_PATH = Path(__file__).with_name("native-runtime.py")
_SPEC = importlib.util.spec_from_file_location("native_runtime", HELPER_PATH)
assert _SPEC and _SPEC.loader
native = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(native)


class SessionModeCompatibilityTest(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.db = Path(self.tempdir.name) / "ao.db"
        with sqlite3.connect(self.db) as conn:
            conn.execute("CREATE TABLE sessions (id TEXT, project_id TEXT)")
            conn.execute("INSERT INTO sessions VALUES ('old-session', 'project')")

    def tearDown(self):
        self.tempdir.cleanup()

    def test_pre_chat_schema_implies_tui(self):
        self.assertEqual(native.session_mode(self.db, "project", "old-session"), "tui")

    def test_chat_mode_is_read_from_current_schema(self):
        with sqlite3.connect(self.db) as conn:
            conn.execute("ALTER TABLE sessions ADD COLUMN session_mode TEXT NOT NULL DEFAULT 'tui'")
            conn.execute("UPDATE sessions SET session_mode='chat'")
        self.assertEqual(native.session_mode(self.db, "project", "old-session"), "chat")

    def test_missing_session_and_unknown_mode_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "session mode unavailable"):
            native.session_mode(self.db, "project", "missing")
        with sqlite3.connect(self.db) as conn:
            conn.execute("ALTER TABLE sessions ADD COLUMN session_mode TEXT")
            conn.execute("UPDATE sessions SET session_mode='unknown'")
        with self.assertRaisesRegex(ValueError, "session mode unavailable"):
            native.session_mode(self.db, "project", "old-session")


if __name__ == "__main__":
    unittest.main()
