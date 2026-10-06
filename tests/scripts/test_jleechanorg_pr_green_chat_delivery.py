import importlib.util
import fcntl
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('chat_delivery', Path(__file__).resolve().parents[2] / 'jobs/jleechanorg-pr-green/chat-delivery.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)


class ChatDeliveryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'pending'
        self.owner = {'id': 'project.with.dot-1', 'project_id': 'project.with.dot', 'controller_generation': 'g1', 'prompt': 'repair', 'created_at': '2026-10-04T18:00:00Z'}
        self.empty = {'controller': 'ready', 'turns': [], 'messages': []}
        self.args = (None, '/db', 'project.with.dot', 'project.with.dot-1', '/scoped', self.path, 'repair')

    def snapshot(self, state='running', provider='p1', text='repair'):
        return {'controller': 'busy', 'turns': [{'id': 't1', 'providerTurnId': provider, 'state': state, 'requestedAt': '2026-10-04T18:00:01Z'}],
                'messages': [{'role': 'user', 'text': text, 'turnId': 't1'}]}

    def test_running_unlinked_codex_preserves_process_identity_checks(self):
        # Synthetic /proc records reproduce Linux's executable replacement marker.
        root = Path(self.tmp.name); proc = root / 'proc'; d = proc / '123'; d.mkdir(parents=True)
        work = root / 'work'; work.mkdir()
        owner = self.owner | {'workspace_path': str(work)}
        (d / 'stat').write_text('123 (codex) S ' + ' '.join(['0'] * 18 + ['123']))
        (d / 'exe').symlink_to('/bin/codex (deleted)')
        (d / 'cwd').symlink_to(work)
        (d / 'cmdline').write_bytes(b'codex\0app-server\0')
        (d / 'environ').write_bytes(('AO_SESSION_ID=' + owner['id'] + '\0AO_PROJECT_ID=' + owner['project_id'] + '\0CODEX_HOME=/scoped\0').encode())
        m.profile(owner, '/scoped', proc)
        with patch.object(m.os, 'getuid', return_value=d.stat().st_uid + 1), self.assertRaises(ValueError):
            m.profile(owner, '/scoped', proc)
        with self.assertRaises(ValueError): m.profile(owner, '/other-account', proc)
        with self.assertRaises(ValueError): m.profile(owner | {'id': 'other-session'}, '/scoped', proc)
        (d / 'exe').unlink(); (d / 'exe').symlink_to('/bin/unrelated (deleted)')
        with self.assertRaises(ValueError): m.profile(owner, '/scoped', proc)

    def test_restore_preflight_requires_exact_existing_workspace_and_scoped_thread(self):
        # SYNTHETIC AO API; real SQLite and Git registered-worktree inspection.
        root = Path(self.tmp.name); source = root / 'repository'; source.mkdir()
        def git(*args):
            subprocess.run(['git', '-C', str(source), *args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        git('init'); git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--allow-empty', '-m', 'fixture')
        work = root / 'worker'; git('worktree', 'add', '--detach', str(work))
        home = root / 'scope'; home.mkdir(); proc = root / 'proc'; proc.mkdir()
        owner = self.owner | {'harness': 'codex', 'is_terminated': 1, 'session_mode': 'chat',
            'workspace_path': str(work), 'provider_conversation_id': 'native-thread'}
        db = root / 'ao.db'
        with m.sqlite3.connect(db) as conn:
            conn.execute('CREATE TABLE sessions(' + ','.join(owner) + ')')
            conn.execute('INSERT INTO sessions VALUES(' + ','.join('?' for _ in owner) + ')', list(owner.values()))
        with m.sqlite3.connect(home / 'state_5.sqlite') as conn:
            conn.execute('CREATE TABLE threads(id,cwd)'); conn.execute('INSERT INTO threads VALUES(?,?)', ('native-thread', str(work)))
        project_path = [str(source)]
        class API:
            def request(inner, path):
                if path.startswith('projects/'):
                    return {'status': 'ok', 'project': {'id': owner['project_id'], 'path': project_path[0], 'config': {'env': {'CODEX_HOME': str(home)}}}}
                return {'session': {'id': owner['id'], 'projectId': owner['project_id'], 'mode': 'chat', 'harness': 'codex', 'isTerminated': True}}
        args = (API(), str(db), owner['project_id'], owner['id'], str(home), proc)
        m.restore_preflight(*args)
        other = root / 'other-project'; other.mkdir()
        subprocess.run(['git', '-C', str(other), 'init'], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        project_path[0] = str(other)
        with self.assertRaises(ValueError): m.restore_preflight(*args)
        project_path[0] = str(source)
        with m.sqlite3.connect(home / 'state_5.sqlite') as conn: conn.execute("UPDATE threads SET cwd='/unrelated'")
        with self.assertRaises(ValueError): m.restore_preflight(*args)
        with m.sqlite3.connect(home / 'state_5.sqlite') as conn: conn.execute('UPDATE threads SET cwd=?', (str(work),))
        git('worktree', 'remove', str(work))
        with self.assertRaises(ValueError): m.restore_preflight(*args)
        self.assertFalse(work.exists(), 'preflight must never recreate a removed worktree')

    def test_old_pending_receipt_does_not_count_new_prompt_as_delivered(self):
        m.save(self.path, {'session': self.owner['id'], 'initial': True, 'text': 'repair'})
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args[:-1], 'repair at new head'), 6)
        self.assertFalse(self.path.exists())

    def test_restore_reservation_survives_uncertainty_and_sends_once_when_ready(self):
        old = self.owner | {'provider_conversation_id': 'thread', 'workspace_path': '/work', 'harness': 'codex', 'session_mode': 'chat', 'is_terminated': 1}
        with patch.object(m, 'restore_preflight', return_value=old):
            self.assertEqual(m.prepare_restore(None, '/db', old['project_id'], old['id'], '/scoped', self.path, 'repair'), 0)
            before = self.path.read_bytes()
            self.assertEqual(m.prepare_restore(None, '/db', old['project_id'], old['id'], '/scoped', self.path, 'repair'), 4)
        self.assertEqual(self.path.read_bytes(), before)
        current = old | {'is_terminated': 0, 'controller_generation': 'restored-generation'}
        class API:
            calls = []
            def request(inner, path, body):
                inner.calls.append(body)
                return {'outcome': 'sent', 'turnId': 't1'}
        api = API()
        with patch.object(m, 'validate', return_value=(self.empty, current | {'provider_conversation_id': 'wrong-thread'})):
            self.assertEqual(m.deliver(api, *self.args[1:]), 4)
        self.assertEqual(api.calls, [])
        with patch.object(m, 'validate', return_value=(self.snapshot(), current)):
            self.assertEqual(m.deliver(api, *self.args[1:]), 5)
        self.assertEqual(self.path.read_bytes(), before)
        with patch.object(m, 'validate', side_effect=[(self.empty, current), (self.snapshot(), current)]), patch.object(m, 'row', return_value=current), patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(api, *self.args[1:]), 0)
        self.assertEqual(len(api.calls), 1)
        self.assertEqual(api.calls[0]['text'], 'repair')
        self.assertFalse(self.path.exists())

    def test_overlapping_delivery_preserves_the_current_owner(self):
        with self.path.with_name(self.path.name + '.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with patch.object(m, 'validate', side_effect=AssertionError('concurrent delivery must not inspect or send')):
                self.assertEqual(m.deliver(*self.args), 4)
        self.assertFalse(self.path.exists())

    def test_precise_stored_turn_time_resolves_same_second_admission(self):
        db = str(Path(self.tmp.name) / 'ao.db')
        with m.sqlite3.connect(db) as conn:
            conn.execute('CREATE TABLE conversation_turns(id, handled_by_session_id, provider_turn_id, controller_generation, requested_at, rolled_back_at, conversation_id)')
            conn.execute('CREATE TABLE conversations(id, current_session_id, project_id)')
            conn.execute('INSERT INTO conversations VALUES(?,?,?)', ('c1', self.owner['id'], self.owner['project_id']))
            conn.execute("INSERT INTO conversation_turns VALUES(?,?,?,?,?,NULL,'c1')",
                         ('t1', self.owner['id'], 'p1', 'g1', '2026-10-04 18:00:01.200000001 +0000 UTC'))
        owner = self.owner | {'created_at': '2026-10-04 18:00:01.200000000 +0000 UTC'}
        snapshot = self.snapshot()
        snapshot['conversationId'] = 'c1'
        m.precise_turn_times(db, owner, snapshot)
        self.assertTrue(m.acknowledged(snapshot, 'repair', initial_owner=owner))
        self.assertFalse(m.acknowledged(snapshot, 'repair', initial_owner=owner | {'created_at': '2026-10-04 18:00:01.200000002 +0000 UTC'}))
        # Async provisioning records an empty generation and does not rewrite
        # it when the provider is bound. Current conversation ownership still
        # must match before its exact stored time may be used.
        with m.sqlite3.connect(db) as conn:
            conn.execute("UPDATE conversation_turns SET controller_generation='' ")
        snapshot = self.snapshot(); snapshot['conversationId'] = 'c1'
        m.precise_turn_times(db, owner, snapshot)
        self.assertTrue(m.acknowledged(snapshot, 'repair', initial_owner=owner))
        with m.sqlite3.connect(db) as conn:
            conn.execute("UPDATE conversation_turns SET controller_generation='g1' ")
        # A different generation or provider turn cannot lend its timestamp.
        for changed in ({'controller_generation': 'old'}, {'id': 'other-session'}):
            snapshot = self.snapshot(); snapshot['conversationId'] = 'c1'
            m.precise_turn_times(db, owner | changed, snapshot)
            self.assertFalse(m.acknowledged(snapshot, 'repair', initial_owner=owner))
        snapshot = self.snapshot(provider='other-provider-turn'); snapshot['conversationId'] = 'c1'
        m.precise_turn_times(db, owner, snapshot)
        self.assertFalse(m.acknowledged(snapshot, 'repair', initial_owner=owner))
        # A public/store disagreement must fail closed, not rewrite history.
        snapshot = self.snapshot()
        snapshot['conversationId'] = 'c1'
        snapshot['turns'][0]['requestedAt'] = '2026-10-04T18:00:02Z'
        with self.assertRaises(ValueError): m.precise_turn_times(db, owner, snapshot)

    def test_admission_reserves_before_spawn_and_requires_explicit_binding(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        before = self.path.read_bytes()
        self.assertEqual(m.admission(self.path, 'replacement'), 4)
        self.assertEqual(self.path.read_bytes(), before)
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args), 4)
        if not m.json.loads(self.path.read_text())['session']:
            m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        self.assertEqual(m.admission(self.path, 'repair', self.owner['id']), 0)
        self.assertEqual(m.admission(self.path, 'repair', 'different-session'), 4)
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args), 0)
        self.assertFalse(self.path.exists())

    def test_bound_initial_admission_waits_for_provider_without_post(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        if not m.json.loads(self.path.read_text())['session']:
            m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        self.assertEqual(m.admission(self.path, 'repair', self.owner['id']), 0)
        class NoPost:
            def request(self, *args): raise AssertionError('initial admission must only observe')
        with patch.object(m, 'validate', side_effect=[(self.empty, self.owner), (self.snapshot(), self.owner)]) as validate, patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:], initial=True), 0)
        self.assertEqual(validate.call_count, 2)
        self.assertFalse(self.path.exists())

    def test_delayed_initial_receipt_through_http_sqlite_and_process_guard(self):
        # SYNTHETIC CONTRACT FIXTURE: external AO HTTP and /proc records only.
        # Real API decoding, SQLite ownership, process guard, locks and ledger.
        root = Path(self.tmp.name); work = root / 'work'; work.mkdir()
        proc = root / 'proc'; entry = proc / '123'; entry.mkdir(parents=True)
        (entry / 'stat').write_text('123 (codex) S ' + ' '.join(['0'] * 18 + ['123']))
        (entry / 'exe').symlink_to('/bin/codex'); (entry / 'cwd').symlink_to(work)
        (entry / 'cmdline').write_bytes(b'codex\0app-server\0')
        (entry / 'environ').write_bytes(('AO_SESSION_ID=' + self.owner['id'] + '\0AO_PROJECT_ID=' + self.owner['project_id'] + '\0CODEX_HOME=/scoped\0').encode())
        owner = self.owner | {'harness': 'codex', 'is_terminated': 0, 'session_mode': 'chat',
                              'workspace_path': str(work), 'provider_conversation_id': 'provider-conversation',
                              'created_at': '2026-10-04 18:00:01.100000001 +0000 UTC'}
        db = str(root / 'native.db')
        with m.sqlite3.connect(db) as conn:
            conn.execute('CREATE TABLE sessions(' + ','.join(owner) + ')')
            conn.execute('INSERT INTO sessions VALUES(' + ','.join('?' for _ in owner) + ')', list(owner.values()))
            conn.execute('CREATE TABLE conversation_turns(id, handled_by_session_id, provider_turn_id, controller_generation, requested_at, rolled_back_at, conversation_id)')
            conn.execute('CREATE TABLE conversations(id, current_session_id, project_id)')
            conn.execute('INSERT INTO conversations VALUES(?,?,?)', ('c1', owner['id'], owner['project_id']))
            conn.execute("INSERT INTO conversation_turns VALUES(?,?,?,?,?,NULL,'c1')", ('t1', owner['id'], 'p1', 'g1', '2026-10-04 18:00:01.200000001 +0000 UTC'))
        identity = {'conversationId': 'c1', 'sessionId': owner['id'], 'mode': 'chat', 'harness': 'codex'}
        ready = self.snapshot() | identity
        public_session = {'session': {'id': owner['id'], 'projectId': owner['project_id'], 'mode': 'chat', 'isTerminated': False}}
        counts = {'conversation': 0, 'posts': 0}
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                counts['posts'] += 1; self.send_error(500)
            def do_GET(handler):
                if '/conversation?' in handler.path:
                    counts['conversation'] += 1
                    payload = (self.empty | identity) if counts['conversation'] == 1 else ready
                else:
                    payload = public_session
                raw = m.json.dumps(payload).encode()
                handler.send_response(200); handler.send_header('Content-Type', 'application/json')
                handler.send_header('Content-Length', str(len(raw))); handler.end_headers(); handler.wfile.write(raw)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.01}, daemon=True); thread.start()
        def cleanup():
            server.shutdown(); server.server_close(); thread.join(timeout=2)
        self.addCleanup(cleanup)
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        if not m.json.loads(self.path.read_text())['session']:
            m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + owner['id'] + ' (idle)\n')
        self.assertEqual(m.admission(self.path, 'repair', owner['id']), 0)
        profile = m.profile
        with patch.object(m, 'profile', side_effect=lambda value, home: profile(value, home, proc)), patch.object(m.time, 'sleep'):
            rc = m.deliver(m.API('http://127.0.0.1:' + str(server.server_port)), db,
                           owner['project_id'], owner['id'], '/scoped', self.path, 'repair', initial=True)
        self.assertEqual(rc, 0)
        self.assertEqual(counts, {'conversation': 2, 'posts': 0})
        self.assertFalse(self.path.exists())

    def test_initial_poll_keeps_admission_time_and_owner_fences(self):
        old = self.snapshot()
        old['turns'][0]['requestedAt'] = '2026-10-04T17:59:59Z'
        class NoPost:
            def request(self, *args): raise AssertionError('initial admission must only observe')
        m.admission(self.path, 'repair')
        m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        m.admission(self.path, 'repair', self.owner['id'])
        with patch.object(m, 'validate', side_effect=[(self.empty, self.owner)] + [(old, self.owner)] * 10) as validate, patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:], initial=True), 4)
        self.assertEqual(validate.call_count, 11)
        self.assertTrue(self.path.exists())
        with patch.object(m, 'validate', side_effect=[(self.empty, self.owner), (self.snapshot(), self.owner | {'controller_generation': 'new'})]) as validate, patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:]), 4)
        self.assertEqual(validate.call_count, 2)
        self.assertTrue(self.path.exists())

    def test_ambiguous_spawn_identity_does_not_bind_admission(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        before = self.path.read_bytes()
        self.assertEqual(m.admission(self.path, 'repair', 'session-one\nsession-two'), 4)
        self.assertEqual(m.admission(self.path, 'wrong prompt', self.owner['id']), 4)
        self.assertEqual(self.path.read_bytes(), before)

    def test_durable_spawn_receipt_recovers_after_controller_loses_binding(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        receipt = m.spawn_receipt_path(self.path, 'repair')
        self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)
        receipt.write_text('spawned session ' + self.owner['id'] + ' (idle) (claimed PR)\n')
        class NoPost:
            def request(self, *args): raise AssertionError('recovery must never send again')
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:]), 0)
        self.assertFalse(self.path.exists())
        self.assertTrue(receipt.exists(), 'retain original native evidence after acknowledgment')

    def test_spawn_receipt_ambiguity_and_failure_never_release_reservation(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
        receipt = m.spawn_receipt_path(self.path, 'repair')
        original = self.path.read_bytes()
        for output in ('', 'Spawn queue is full\n', 'spawned session one (idle)\nspawned session two (idle)\n',
                       'spawned session ' + self.owner['id'] + ' (idle)'):
            receipt.write_text(output)
            with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
                self.assertEqual(m.deliver(*self.args), 4)
            self.assertEqual(self.path.read_bytes(), original)
            self.assertEqual(m.admission(self.path, 'repair'), 4)
        with self.assertRaises(ValueError): m.spawn_receipt_path(self.path, 'changed prompt')
        receipt.unlink(); receipt.symlink_to(self.path)
        with self.assertRaises(OSError): m.spawn_receipt_path(self.path, 'repair')

    def test_initial_binding_requires_unique_complete_matching_native_receipt(self):
        m.admission(self.path, 'repair')
        receipt = m.spawn_receipt_path(self.path, 'repair')
        for output in ('', 'spawned session ' + self.owner['id'] + ' (idle)',
                       'spawned session another (idle)\n',
                       'spawned session ' + self.owner['id'] + ' (idle)\nspawned session another (idle)\n'):
            receipt.write_text(output)
            self.assertEqual(m.admission(self.path, 'repair', self.owner['id']), 4)
            self.assertEqual(m.json.loads(self.path.read_text())['session'], '')
        receipt.write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        self.assertEqual(m.reserved_session(self.path), self.owner['id'])
        if not m.json.loads(self.path.read_text())['session']:
            m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        self.assertEqual(m.admission(self.path, 'repair', self.owner['id']), 0)

    def test_missing_listing_observation_retains_owner_across_two_sweeps(self):
        m.admission(self.path, 'repair')
        m.spawn_receipt_path(self.path, 'repair').write_text('spawned session ' + self.owner['id'] + ' (idle)\n')
        class NoPost:
            def request(self, *args): raise AssertionError('observation must not send')
        for sweep in range(2):
            with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
                self.assertEqual(m.deliver(NoPost(), *self.args[1:], initial=True, retain_initial=True), 0)
            self.assertEqual(m.reserved_session(self.path), self.owner['id'])
            self.assertEqual(m.admission(self.path, 'repair'), 4)
        # Once normal list-backed observation resumes, ordinary cleanup works.
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:]), 0)
        self.assertFalse(self.path.exists())

    def test_observe_initial_rejects_mixed_ledger_before_any_api_call(self):
        for field in ('restore', 'request', 'turnId'):
            m.save(self.path, {'session': self.owner['id'], 'initial': True, 'text': 'repair', field: {}})
            original = self.path.read_bytes()
            with patch.object(m, 'validate', side_effect=AssertionError('mixed ledger must not contact API')):
                self.assertEqual(m.deliver(*self.args, initial=True, retain_initial=True), 4)
            self.assertEqual(self.path.read_bytes(), original)

    def historical_initial_fixture(self, terminated=1):
        previous = self.owner | {'is_terminated': 0, 'harness': 'codex', 'session_mode': 'chat',
                    'provider_conversation_id': 'provider-thread', 'workspace_path': '/work'}
        current = previous | {'is_terminated': terminated, 'controller_generation': 'g2'}
        db = str(Path(self.tmp.name) / 'historical.db')
        with m.sqlite3.connect(db) as conn:
            conn.execute('CREATE TABLE sessions(' + ','.join(current) + ')')
            conn.execute('INSERT INTO sessions VALUES(' + ','.join('?' for _ in current) + ')', list(current.values()))
            conn.execute('CREATE TABLE conversation_turns(id, handled_by_session_id, provider_turn_id, controller_generation, requested_at, rolled_back_at, conversation_id)')
            conn.execute('CREATE TABLE conversations(id, current_session_id, project_id)')
            conn.execute('INSERT INTO conversations VALUES(?,?,?)', ('c1', current['id'], current['project_id']))
            conn.execute("INSERT INTO conversation_turns VALUES(?,?,?,?,?,NULL,'c1')",
                         ('t1', current['id'], 'p1', previous['controller_generation'], '2026-10-04 18:00:01.123456789 +0000 UTC'))
        snapshot = self.snapshot('completed') | {'sessionId': current['id'], 'mode': 'chat', 'harness': 'codex', 'conversationId': 'c1'}
        m.save(self.path, {'session': current['id'], 'text': 'repair', 'initial': True, 'owner': previous, 'scope': '/scoped',
            'initialTurn': {'id': 't1', 'providerTurnId': 'p1', 'conversationId': 'c1', 'controllerGeneration': 'g1'}})
        class ReadOnly:
            calls = []
            def request(inner, path, body=None):
                self.assertIsNone(body)
                self.assertEqual(path, 'sessions/' + current['id'] + '/conversation?limit=500')
                inner.calls.append(path)
                return snapshot
        return db, previous, current, snapshot, ReadOnly()

    def test_terminated_initial_receipt_reconciles_exact_persisted_provider_turn(self):
        db, previous, current, snapshot, api = self.historical_initial_fixture()
        with patch.object(m, 'restore_preflight', return_value=current) as proof:
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 0)
        proof.assert_called_once()
        self.assertFalse(self.path.exists())
        self.assertEqual(len(api.calls), 1)

    def test_generation_rotation_reconciles_only_unchanged_native_owner(self):
        db, previous, current, snapshot, api = self.historical_initial_fixture(terminated=0)
        with patch.object(m, 'validate', return_value=(snapshot, current)):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 0)
        self.assertFalse(self.path.exists())

    def test_historical_initial_requires_database_binding_and_immutable_owner(self):
        db, previous, current, snapshot, api = self.historical_initial_fixture()
        original = self.path.read_bytes()
        for scope in (None, '/other-account'):
            pending = m.json.loads(original)
            if scope is None: pending.pop('scope')
            else: pending['scope'] = scope
            m.save(self.path, pending)
            with patch.object(m, 'row', side_effect=AssertionError('unproven scope must not inspect')):
                self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)
        self.path.write_bytes(original)
        snapshot['turns'][0]['state'] = 'failed'
        with patch.object(m, 'restore_preflight', return_value=current):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)
        snapshot['turns'][0]['state'] = 'completed'
        with m.sqlite3.connect(db) as conn: conn.execute("UPDATE conversation_turns SET controller_generation='unrelated'")
        with patch.object(m, 'restore_preflight', return_value=current):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)
        self.assertEqual(self.path.read_bytes(), original)
        with m.sqlite3.connect(db) as conn: conn.execute("UPDATE conversation_turns SET controller_generation='g1'")
        for field in ('provider_conversation_id', 'workspace_path', 'created_at'):
            m.save(self.path, {'session': current['id'], 'text': 'repair', 'initial': True, 'owner': previous | {field: 'different'}, 'scope': '/scoped',
                'initialTurn': {'id': 't1', 'providerTurnId': 'p1', 'conversationId': 'c1', 'controllerGeneration': 'g1'}})
            before = self.path.read_bytes()
            with patch.object(m, 'restore_preflight', return_value=current):
                self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)
            self.assertEqual(self.path.read_bytes(), before)
        m.save(self.path, {'session': current['id'], 'text': 'repair', 'initial': True, 'request': 'ordinary'})
        with patch.object(m, 'row', side_effect=AssertionError('mixed ledger must be rejected before inspection')):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)

    def test_historical_matching_import_is_not_original_initial_turn(self):
        db, previous, current, snapshot, api = self.historical_initial_fixture()
        pending = m.json.loads(self.path.read_text())
        pending['initialTurn'] = {'id': 'original-turn', 'providerTurnId': 'original-provider',
                                  'conversationId': 'c1', 'controllerGeneration': 'g1'}
        m.save(self.path, pending)
        with patch.object(m, 'restore_preflight', return_value=current):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 4)
        self.assertTrue(self.path.exists())

    def test_queued_original_turn_is_pinned_before_later_native_termination(self):
        db, previous, current, snapshot, api = self.historical_initial_fixture()
        with m.sqlite3.connect(db) as conn:
            conn.execute("UPDATE sessions SET is_terminated=0, controller_generation='g1'")
            conn.execute("UPDATE conversation_turns SET provider_turn_id=''")
        snapshot['controller'] = 'busy'; snapshot['turns'][0]['state'] = 'queued'; snapshot['turns'][0]['providerTurnId'] = ''
        pending = m.json.loads(self.path.read_text()); del pending['initialTurn']; m.save(self.path, pending)
        with patch.object(m, 'validate', return_value=(snapshot, previous)), patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(api, db, previous['project_id'], previous['id'], '/scoped', self.path, 'repair', initial=True), 4)
        pending = m.json.loads(self.path.read_text())
        self.assertEqual(pending['initialTurn'], {'id': 't1', 'providerTurnId': '', 'conversationId': 'c1', 'controllerGeneration': 'g1'})
        with m.sqlite3.connect(db) as conn:
            conn.execute("UPDATE sessions SET is_terminated=1, controller_generation='g2'")
            conn.execute("UPDATE conversation_turns SET provider_turn_id='p1'")
        snapshot['controller'] = 'terminated'; snapshot['turns'][0]['state'] = 'completed'; snapshot['turns'][0]['providerTurnId'] = 'p1'
        with patch.object(m, 'restore_preflight', return_value=current):
            self.assertEqual(m.reconcile_initial(api, db, current['project_id'], current['id'], '/scoped', self.path), 0)
        self.assertFalse(self.path.exists())

    def test_queued_receipt_is_not_provider_ack(self):
        self.assertFalse(m.acknowledged(self.snapshot('queued'), 'repair'))
        self.assertFalse(m.acknowledged(self.snapshot(provider=''), 'repair'))
        self.assertFalse(m.acknowledged(self.snapshot(text='other'), 'repair'))
        self.assertFalse(m.acknowledged(self.snapshot('failed'), 'repair'))
        self.assertTrue(m.acknowledged(self.snapshot(), 'repair', 't1'))
        self.assertFalse(m.acknowledged(self.snapshot(), 'repair', 'other'))

    def test_initial_ack_requires_admission_prompt_and_new_turn(self):
        self.assertTrue(m.acknowledged(self.snapshot(), 'repair', initial_owner=self.owner))
        self.assertFalse(m.acknowledged(self.snapshot(), 'repair', initial_owner=self.owner | {'prompt': 'different'}))
        self.assertFalse(m.acknowledged(self.snapshot(), 'repair', initial_owner=self.owner | {'created_at': '2026-10-04T19:00:00Z'}))

    def test_initial_ack_accepts_actual_go_sqlite_admission_timestamp(self):
        owner = self.owner | {'created_at': '2026-10-04 18:00:00.162941244 +0000 UTC'}
        self.assertTrue(m.acknowledged(self.snapshot(), 'repair', initial_owner=owner))
        for stamp in ('2026-10-04 18:00:01.000000001 +0000 UTC',
                      '2026-10-04 18:00:02 +0000 UTC', 'invalid',
                      '2026-10-04T18:00:00'):
            with self.subTest(stamp=stamp):
                self.assertFalse(m.acknowledged(self.snapshot(), 'repair', initial_owner=owner | {'created_at': stamp}))

    def test_initial_go_timestamp_receipt_clears_pending_without_post(self):
        owner = self.owner | {'created_at': '2026-10-04 18:00:00.162941244 +0000 UTC'}
        m.save(self.path, {'session': owner['id'], 'initial': True, 'text': 'repair', 'owner': owner})
        class NoPost:
            def request(self, *args): raise AssertionError('must observe without sending')
        with patch.object(m, 'validate', return_value=(self.snapshot(), owner)):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:]), 0)
        self.assertFalse(self.path.exists())

    def test_initial_spawn_failure_persists_no_resend_state(self):
        with patch.object(m, 'validate', side_effect=ValueError('connecting')):
            with self.assertRaises(ValueError): m.deliver(*self.args, initial=True)
        pending = m.json.loads(self.path.read_text())
        self.assertTrue(pending['initial']); self.assertEqual(pending['text'], 'repair')
        class NoPost:
            def request(self, *args): raise AssertionError('initial prompt must never resend')
        with patch.object(m, 'validate', return_value=(self.empty, self.owner)), patch.object(m.time, 'sleep'):
            self.assertEqual(m.deliver(NoPost(), *self.args[1:]), 4)

    def test_timeout_recovery_contacts_no_provider_and_keeps_same_handle(self):
        m.save(self.path, {'session': self.owner['id'], 'owner': self.owner, 'text': 'repair', 'request': 'stable'})
        class Recover:
            def request(inner, path, body):
                self.assertEqual(body, {'clientMessageId': 'stable', 'recoverOnly': True})
                raise OSError('unknown')
        with patch.object(m, 'validate', return_value=(self.empty, self.owner)):
            self.assertEqual(m.deliver(Recover(), *self.args[1:]), 4)
        self.assertEqual(m.json.loads(self.path.read_text())['request'], 'stable')

    def test_old_identical_turn_cannot_ack_a_new_unresolved_request(self):
        m.save(self.path, {'session': self.owner['id'], 'owner': self.owner, 'text': 'repair', 'request': 'new-request'})
        class Recover:
            def request(inner, path, body):
                self.assertEqual(body, {'clientMessageId': 'new-request', 'recoverOnly': True})
                raise OSError('unresolved')
        with patch.object(m, 'validate', return_value=(self.snapshot('completed'), self.owner)):
            self.assertEqual(m.deliver(Recover(), *self.args[1:]), 4)
        self.assertTrue(self.path.exists())

    def test_owner_change_prevents_recovery_or_send(self):
        m.save(self.path, {'session': self.owner['id'], 'owner': self.owner | {'controller_generation': 'old'}, 'text': 'repair', 'request': 'stable'})
        with patch.object(m, 'validate', return_value=(self.empty, self.owner)):
            self.assertEqual(m.deliver(None, *self.args[1:]), 4)

    def test_busy_session_does_not_enqueue_another_repair(self):
        for controller, state in [('busy', 'completed'), ('ready', 'running'), ('ready', 'queued')]:
            with self.subTest(controller=controller, state=state):
                snapshot = self.snapshot(state); snapshot['controller'] = controller
                with patch.object(m, 'validate', return_value=(snapshot, self.owner)):
                    self.assertEqual(m.deliver(*self.args), 5)
                self.assertFalse(self.path.exists())

    def test_unready_controller_is_unconfirmed_not_delivered_or_busy(self):
        for controller in ('connecting', 'error', None):
            with self.subTest(controller=controller):
                snapshot = self.snapshot('completed'); snapshot['controller'] = controller
                with patch.object(m, 'validate', return_value=(snapshot, self.owner)):
                    self.assertEqual(m.deliver(*self.args), 4)
                self.assertFalse(self.path.exists())

    def test_confirmed_initial_provider_turn_clears_pending_without_post(self):
        m.save(self.path, {'session': self.owner['id'], 'initial': True, 'text': 'repair'})
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args), 0)
        self.assertFalse(self.path.exists())

    def test_new_request_persisted_before_network_and_not_replayed(self):
        class FailingPost:
            def request(inner, path, body):
                pending = m.json.loads(self.path.read_text())
                self.assertEqual(pending['request'], body['clientMessageId'])
                raise OSError('response lost')
        with patch.object(m, 'validate', return_value=(self.empty, self.owner)), patch.object(m, 'row', return_value=self.owner):
            with self.assertRaises(OSError): m.deliver(FailingPost(), *self.args[1:])
        self.assertTrue(self.path.exists()); self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)


if __name__ == '__main__': unittest.main()
