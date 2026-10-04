import importlib.util
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

    def test_initial_spawn_failure_persists_no_resend_state(self):
        with patch.object(m, 'validate', side_effect=ValueError('connecting')):
            with self.assertRaises(ValueError): m.deliver(*self.args, initial=True)
        pending = m.json.loads(self.path.read_text())
        self.assertTrue(pending['initial']); self.assertEqual(pending['text'], 'repair')
        class NoPost:
            def request(self, *args): raise AssertionError('initial prompt must never resend')
        with patch.object(m, 'validate', return_value=(self.empty, self.owner)):
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
        with patch.object(m, 'validate', return_value=(self.snapshot(), self.owner)):
            self.assertEqual(m.deliver(*self.args), 0)
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
