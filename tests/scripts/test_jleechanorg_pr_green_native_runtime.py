import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('native_runtime', Path(__file__).resolve().parents[2] / 'jobs/jleechanorg-pr-green/native-runtime.py')
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class NativeRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.row = dict(id='wa-1', project_id='wa', is_terminated=0, session_mode='tui',
                        workspace_path='/work', runtime_handle_id='ptyhost-v1:opaque',
                        runtime_launch_id='launch-1', agent_session_id='native-1',
                        agent_session_id_launch_id='launch-1')
        self.view = dict(id='wa-1', projectId='wa', isTerminated=False, mode='tui',
                         terminalGeneration='launch-1', terminalHandleId='ptyhost-v1:opaque', activity={'state': 'idle'})

    def test_named_socket_requires_verified_packaged_daemon(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root); proc = root / 'proc'; entry = proc / '123'; entry.mkdir(parents=True)
            exe = root / 'resources/daemon/ao'; exe.parent.mkdir(parents=True); exe.touch()
            (entry / 'exe').symlink_to(exe)
            (entry / 'cmdline').write_bytes(b'ao\0daemon\0')
            (entry / 'stat').write_text('123 (ao) ' + ' '.join(['0'] * 20))
            (entry / 'environ').write_bytes(b'AO_TMUX_SOCKET_NAME=ao\0SECRET=do-not-output\0')
            run = root / 'running.json'; run.write_text('{"pid":123}')
            self.assertEqual(module.daemon_tmux_socket(run, proc), 'ao')
            with patch.object(module, 'starttime', side_effect=['first', 'second']), self.assertRaises(ValueError):
                module.daemon_tmux_socket(run, proc)
            (entry / 'cmdline').write_bytes(b'ao\0spawn\0')
            with self.assertRaises(ValueError): module.daemon_tmux_socket(run, proc)

    def test_exact_generation_and_tui_required(self):
        module.validate(self.row, self.view)
        for change in ({'terminalGeneration': 'old-launch'}, {'id': 'reused-id'},
                       {'projectId': 'other'}, {'terminalHandleId': 'stale-handle'}, {'mode': 'chat'}, {'isTerminated': True}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                module.validate(self.row, self.view | change)

    def test_old_native_identity_is_not_current_acceptance(self):
        with self.assertRaises(ValueError):
            module.validate(self.row | {'agent_session_id_launch_id': ''}, self.view)

    def test_missing_generation_or_legacy_handle_refused(self):
        for change in ({'runtime_launch_id': ''}, {'runtime_handle_id': 'legacy-tmux'}, {'is_terminated': 1}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                module.validate(self.row | change, self.view)

    def test_unique_exact_process_profile(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            workspace = root / 'work'; workspace.mkdir()
            row = self.row | {'workspace_path': str(workspace)}
            def process(pid, launch='launch-1'):
                d = root / str(pid); d.mkdir()
                (d / 'stat').write_text(str(pid)+' (codex) S '+ ' '.join(['0']*18+['123']))
                (d / 'exe').symlink_to('/bin/codex')
                (d / 'cwd').symlink_to(workspace)
                (d / 'cmdline').write_bytes(b'codex\0')
                env = {'AO_SESSION_ID': 'wa-1', 'AO_PROJECT_ID': 'wa', 'AO_RUNTIME_LAUNCH_ID': launch, 'CODEX_HOME': '/scoped-codex'}
                (d / 'environ').write_bytes(b'\0'.join((k+'='+v).encode() for k,v in env.items()))
            process(123)
            self.assertEqual(module.process_home(row, root), '/scoped-codex')
            with patch.object(module.os, 'getuid', return_value=(root / '123').stat().st_uid + 1), self.assertRaises(ValueError):
                module.process_home(row, root)
            process(124, 'stale')
            self.assertEqual(module.process_home(row, root), '/scoped-codex')
            (root / '124/environ').write_bytes((root / '123/environ').read_bytes())
            with self.assertRaises(ValueError): module.process_home(row, root)

    def test_running_unlinked_codex_preserves_native_launch_identity(self):
        # Synthetic /proc records; replacing a binary must not discard its owner.
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); d = root / '123'; d.mkdir(); work = root / 'work'; work.mkdir()
            row = self.row | {'workspace_path': str(work)}
            (d / 'stat').write_text('123 (codex) S ' + ' '.join(['0'] * 18 + ['123']))
            (d / 'exe').symlink_to('/bin/codex (deleted)'); (d / 'cwd').symlink_to(work)
            (d / 'cmdline').write_bytes(b'codex\0')
            (d / 'environ').write_bytes(b'AO_SESSION_ID=wa-1\0AO_PROJECT_ID=wa\0AO_RUNTIME_LAUNCH_ID=launch-1\0CODEX_HOME=/scope')
            self.assertEqual(module.process_home(row, root), '/scope')
            with patch.object(module.os, 'getuid', return_value=d.stat().st_uid + 1), self.assertRaises(ValueError):
                module.process_home(row, root)
            with self.assertRaises(ValueError): module.process_home(row | {'runtime_launch_id': 'other'}, root)
            (d / 'exe').unlink(); (d / 'exe').symlink_to('/bin/unrelated (deleted)')
            with self.assertRaises(ValueError): module.process_home(row, root)

    def test_database_generation_change_during_observation_refused(self):
        with patch.object(module, 'snapshot', side_effect=[self.row, self.row | {'runtime_launch_id': 'replacement'}]), patch.object(module, 'view', return_value=self.view), patch.object(module, 'process_home', return_value='/scoped'):
            with self.assertRaises(ValueError): module.observe('/db', 'http://127.0.0.1:3001', 'wa', 'wa-1')

    def test_pid_reuse_refused(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); d=root/'123'; d.mkdir(); work=root/'work'; work.mkdir()
            (d/'exe').symlink_to('/bin/codex'); (d/'cwd').symlink_to(work)
            (d/'cmdline').write_bytes(b'codex\0')
            (d/'environ').write_bytes(b'AO_SESSION_ID=wa-1\0AO_PROJECT_ID=wa\0AO_RUNTIME_LAUNCH_ID=launch-1\0CODEX_HOME=/scope')
            with patch.object(module, 'starttime', side_effect=['123', '456']), self.assertRaises(ValueError):
                module.process_home(self.row | {'workspace_path':str(work)}, root)

    def test_redirects_refused(self):
        with self.assertRaises(ValueError):
            module.NoRedirect().redirect_request(None, None, 302, '', {}, 'http://example.com')

    def test_only_loopback_observation_allowed(self):
        for base in ('https://example.com', 'http://example.com', 'http://user:password@127.0.0.1', 'http://127.0.0.1/path'):
            with self.subTest(base=base), self.assertRaises(ValueError): module.view(base, 'wa-1')


if __name__ == '__main__':
    unittest.main()
