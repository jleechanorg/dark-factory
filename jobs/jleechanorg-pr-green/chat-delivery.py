#!/usr/bin/env python3
"""Supported Chat delivery with durable handles and provider-backed receipts."""
from datetime import datetime
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import time
import urllib.request
from urllib.parse import urlsplit
import uuid


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('redirect forbidden')


class API:
    def __init__(self, base):
        url = urlsplit(base)
        if (url.scheme != 'http' or url.hostname not in ('127.0.0.1', 'localhost')
                or url.username or url.password or url.query or url.fragment or url.path not in ('', '/')):
            raise ValueError('loopback endpoint required')
        self.base = base.rstrip('/')
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.base + '/api/v1/' + path, data=data,
                                     headers={'Content-Type': 'application/json'})
        with self.opener.open(req, timeout=10) as reply:
            return json.load(reply)


def row(db, project, session):
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        conn.row_factory = sqlite3.Row
        item = conn.execute('SELECT id,project_id,harness,is_terminated,session_mode,workspace_path,'
                            'provider_conversation_id,controller_generation,prompt,created_at FROM sessions WHERE id=? AND project_id=?',
                            (session, project)).fetchone()
        if not item:
            raise ValueError('session missing')
        return dict(item)


def profile(entry, home, proc=Path('/proc')):
    matches = []
    for process in proc.iterdir():
        if not process.name.isdigit():
            continue
        try:
            before = (process / 'stat').read_text()
            if process.stat().st_uid != os.getuid():
                continue
            env = dict(x.split(b'=', 1) for x in (process / 'environ').read_bytes().split(b'\0') if b'=' in x)
            args = (process / 'cmdline').read_bytes().split(b'\0')
            exe = (process / 'exe').resolve().name
            if (env.get(b'AO_SESSION_ID') != entry['id'].encode()
                    or env.get(b'AO_PROJECT_ID') != entry['project_id'].encode()
                    or env.get(b'CODEX_HOME') != home.encode()
                    or str((process / 'cwd').resolve()) != entry['workspace_path']
                    or b'app-server' not in args or exe != 'codex'):
                continue
            if (process / 'stat').read_text().rsplit(')', 1)[1].split()[19] == before.rsplit(')', 1)[1].split()[19]:
                matches.append(process.name)
        except (OSError, ValueError):
            continue
    if len(matches) != 1:
        raise ValueError('unique scoped Codex app-server unavailable')


def validate(api, db, project, session, home):
    entry = row(db, project, session)
    if (entry['is_terminated'] or entry['session_mode'] != 'chat' or entry['harness'] != 'codex'
            or not entry['provider_conversation_id'] or not entry['controller_generation']):
        raise ValueError('live Chat controller identity unavailable')
    view = api.request('sessions/' + session)['session']
    if view.get('id') != session or view.get('projectId') != project or view.get('mode') != 'chat' or view.get('isTerminated') is not False:
        raise ValueError('public session identity mismatch')
    profile(entry, home)
    snapshot = api.request('sessions/' + session + '/conversation?limit=500')
    if snapshot.get('sessionId') != session or snapshot.get('mode') != 'chat' or snapshot.get('harness') != 'codex':
        raise ValueError('conversation identity mismatch')
    if row(db, project, session) != entry:
        raise ValueError('controller changed during observation')
    return snapshot, entry


def timestamp(value):
    """Compare RFC3339 API and Go SQLite times without dropping nanoseconds."""
    value = re.sub(r' ([+-]\d{2})(\d{2}) [A-Za-z0-9_+:-]+$', r'\1:\2', value)
    match = re.fullmatch(r'(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})', value)
    if not match:
        raise ValueError('invalid timezone-aware timestamp')
    whole = datetime.fromisoformat(match[1] + match[3].replace('Z', '+00:00'))
    return int(whole.timestamp()), int((match[2] or '').ljust(9, '0'))


def acknowledged(snapshot, text, turn_id=None, initial_owner=None):
    turns = {t['id']: t for t in snapshot.get('turns', []) if not t.get('rolledBack')}
    for message in snapshot.get('messages', []):
        turn = turns.get(message.get('turnId'), {})
        if initial_owner is not None:
            try:
                requested = timestamp(turn['requestedAt'])
                admitted = timestamp(initial_owner['created_at'])
                if initial_owner['prompt'] != text or requested < admitted:
                    continue
            except (ValueError, KeyError, TypeError):
                continue
        if (message.get('role') == 'user' and message.get('text') == text
                and (turn_id is None or turn.get('id') == turn_id)
                and turn.get('providerTurnId') and turn.get('state') in ('running', 'completed', 'recovered')):
            return True
    return False


def save(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    tmp = path.with_name(path.name + '.' + uuid.uuid4().hex)
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(data, stream); stream.flush(); os.fsync(stream.fileno())
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def deliver(api, db, project, session, home, path, text, initial=False):
    if initial and not path.exists():
        save(path, {"session": session, "text": text, "initial": True})
    snapshot, owner = validate(api, db, project, session, home)
    pending = None
    if path.exists():
        pending = json.loads(path.read_text())
        if pending.get('session') != session or (pending.get('owner') is not None and pending['owner'] != owner):
            return 4
        if pending.get('owner') is None:
            if not pending.get('initial'):
                return 4
            pending['owner'] = owner; save(path, pending)
        text = pending['text']
        if (pending.get('initial') or pending.get('turnId')) and acknowledged(snapshot, text, pending.get('turnId'), owner if pending.get('initial') else None):
            path.unlink(); return 0
        if pending.get("initial"):
            return 4
        # No replay on timeout, uncertain transport, missing or queued receipt.
        try:
            receipt = api.request('sessions/' + session + '/conversation/steer-or-send',
                                  {'clientMessageId': pending['request'], 'recoverOnly': True})
            if receipt.get('outcome') != 'sent' or not receipt.get('turnId'):
                return 4
            pending['turnId'] = receipt['turnId']; save(path, pending)
        except (OSError, ValueError):
            return 4
    elif initial:
        # Spawn carries the initial prompt. Observe it; never send a second copy.
        if acknowledged(snapshot, text, initial_owner=owner):
            return 0
        pending = {'session': session, 'owner': owner, 'text': text, 'initial': True}
        save(path, pending)
    else:
        if snapshot.get('controller') != 'ready' or any(t.get('state') in ('running', 'queued') for t in snapshot.get('turns', [])):
            return 0
        request = uuid.uuid4().hex
        pending = {'session': session, 'owner': owner, 'text': text, 'request': request}
        save(path, pending)
        if row(db, project, session) != owner:
            return 4
        receipt = api.request('sessions/' + session + '/conversation/steer-or-send',
                              {'text': text, 'clientMessageId': request})
        if receipt.get('outcome') != 'sent' or not receipt.get('turnId'):
            return 4
        pending['turnId'] = receipt['turnId']; save(path, pending)
    for _ in range(10):
        time.sleep(2)
        current, current_owner = validate(api, db, project, session, home)
        if current_owner != owner:
            return 4
        if acknowledged(current, text, pending.get('turnId')):
            path.unlink(); return 0
    return 4


if __name__ == '__main__':
    try:
        base, db, project, session, home, pending, initial = sys.argv[1:]
        if not all(re.fullmatch(r'[A-Za-z0-9._-]+', item) for item in (project, session)):
            raise ValueError('invalid identity')
        # Prompt comes over stdin, avoiding process argument/log disclosure.
        code = deliver(API(base), db, project, session, home, Path(pending), sys.stdin.read(), initial == 'initial')
        sys.exit(code)
    except (OSError, ValueError, KeyError, sqlite3.Error) as error:
        print('Chat delivery unconfirmed: ' + type(error).__name__, file=sys.stderr)
        sys.exit(4)
