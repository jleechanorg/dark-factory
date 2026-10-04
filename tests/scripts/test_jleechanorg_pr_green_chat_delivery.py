import importlib.util
import fcntl
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
        self.assertEqual(m.admission(self.path, 'repair', self.owner['id']), 0)
        self.assertEqual(m.admission(self.path, 'repair', 'different-session'), 4)
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args), 0)
        self.assertFalse(self.path.exists())

    def test_bound_initial_admission_waits_for_provider_without_post(self):
        self.assertEqual(m.admission(self.path, 'repair'), 0)
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
