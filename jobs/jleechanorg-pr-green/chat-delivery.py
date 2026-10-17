#!/usr/bin/env python3
"""Supported Chat delivery with durable handles and provider-backed receipts."""
from contextlib import contextmanager
from datetime import datetime
import fcntl
import json
import os
from pathlib import Path
import re
import sqlite3
import stat
import subprocess
import sys
import time
import urllib.request
from urllib.parse import urlsplit
import uuid


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('redirect forbidden')


def _ao_worktree_target(db, project, session, project_config):
    """Return the managed root used by the running Go AO daemon.

    This job talks to AO's Go API, whose workspace adapter is rooted at
    ``<dataDir>/worktrees``. A project-level worktreeDir belongs to other AO
    adapters and is not authoritative for this restore endpoint.
    """
    del project_config
    root = Path(db).resolve().parent / 'worktrees'
    if not root.is_absolute():
        raise ValueError('AO worktree root is not absolute')
    return (root / project / session).absolute()


def _definitely_absent(path):
    """Prove a workspace path is absent without treating unreadable as absent."""
    try:
        os.lstat(path)
    except FileNotFoundError:
        parent = path.parent
        try:
            info = os.stat(parent)
        except FileNotFoundError:
            raise ValueError('workspace parent is missing')
        except OSError as error:
            raise ValueError('workspace visibility unavailable') from error
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError('workspace parent is not a directory')
        if info.st_uid != os.getuid():
            raise ValueError('workspace parent is not operator-owned')
        if not os.access(parent, os.R_OK | os.X_OK):
            raise ValueError('workspace visibility unavailable')
        return True
    except OSError as error:
        raise ValueError('workspace visibility unavailable') from error
    return False


def _worktree_entry(output, workspace):
    target = os.path.normpath(str(workspace))
    for block in output.split('\n\n'):
        fields = block.splitlines()
        path = next((line[len('worktree '):] for line in fields if line.startswith('worktree ')), None)
        if path is not None and os.path.normpath(path) == target:
            branch = next((line[len('branch refs/heads/'):]
                           for line in fields if line.startswith('branch refs/heads/')), None)
            return branch, any(line == 'prunable' or line.startswith('prunable ')
                               for line in fields)
    return None


def _registered_branch_elsewhere(output, branch, workspace):
    """Detect a Git registration that would prevent AO from adding this branch."""
    target = os.path.normpath(str(workspace))
    for block in output.split('\n\n'):
        fields = block.splitlines()
        path = next((line[len('worktree '):] for line in fields if line.startswith('worktree ')), None)
        registered = next((line[len('branch refs/heads/'):]
                           for line in fields if line.startswith('branch refs/heads/')), None)
        if registered == branch and path is not None and os.path.normpath(path) != target:
            return path
    return None


def _processes(proc):
    try:
        return list(proc.iterdir())
    except OSError as error:
        raise ValueError('process visibility unavailable') from error


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
                            'provider_conversation_id,controller_generation,prompt,created_at '
                            'FROM sessions WHERE id=? AND project_id=?',
                            (session, project)).fetchone()
        if not item:
            raise ValueError('session missing')
        return dict(item)


def restore_state(db, project, session):
    """Read restore-only metadata outside the durable delivery-owner shape."""
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        item = conn.execute('SELECT branch,activity_state FROM sessions WHERE id=? AND project_id=?',
                            (session, project)).fetchone()
        if item is None:
            raise ValueError('session restore metadata missing')
        return item[0], item[1]


def profile(entry, home, proc=Path('/proc')):
    matches = []
    for process in _processes(proc):
        if not process.name.isdigit():
            continue
        try:
            before = (process / 'stat').read_text()
            if process.stat().st_uid != os.getuid():
                continue
            args = (process / 'cmdline').read_bytes().split(b'\0')
            if b'app-server' not in args:
                continue
            # Ignore unrelated processes before reading their environment;
            # same-user GUI/service processes may hide /proc/*/environ.
            exe = (process / 'exe').resolve().name.removesuffix(' (deleted)')
            if exe != 'codex':
                continue
            env = dict(x.split(b'=', 1) for x in (process / 'environ').read_bytes().split(b'\0') if b'=' in x)
            if (env.get(b'AO_SESSION_ID') != entry['id'].encode()
                    or env.get(b'AO_PROJECT_ID') != entry['project_id'].encode()
                    or env.get(b'CODEX_HOME') != home.encode()
                    or str((process / 'cwd').resolve()) != entry['workspace_path']):
                continue
            if (process / 'stat').read_text().rsplit(')', 1)[1].split()[19] == before.rsplit(')', 1)[1].split()[19]:
                matches.append(process.name)
        except FileNotFoundError:
            continue
        except OSError as error:
            raise ValueError('process visibility unavailable') from error
        except ValueError:
            raise ValueError('process visibility unavailable')
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
    precise_turn_times(db, entry, snapshot)
    if row(db, project, session) != entry:
        raise ValueError('controller changed during observation')
    return snapshot, entry


def restore_preflight(api, db, project, session, home, proc=Path('/proc')):
    """Read-only proof before AO may resume an existing terminated Chat owner."""
    owner = row(db, project, session)
    # An exited owner whose row is not yet flagged terminated remains
    # restorable, but a live DB activity state conflicts with termination.
    branch, activity_state = restore_state(db, project, session)
    terminal_activity = {'exited', 'terminated'}
    if (activity_state not in (None, *terminal_activity)
            or (owner['is_terminated'] != 1 and activity_state not in terminal_activity)
            or owner['session_mode'] != 'chat'
            or owner['harness'] != 'codex' or not owner['provider_conversation_id']):
        raise ValueError('terminated native Chat owner unavailable')

    terminal_public_states = {'exited', 'terminated', 'killed', 'done', 'cleanup', 'errored', 'merged'}

    def public_matches():
        view = api.request('sessions/' + session)['session']
        if (view.get('id') != session or view.get('projectId') != project
                or view.get('mode') != 'chat' or view.get('harness') != 'codex'):
            return False
        status = view.get('status')
        activity = view.get('activity')
        if isinstance(activity, dict):
            activity_state = activity.get('state')
        else:
            activity_state = activity
        status_terminal = isinstance(status, str) and status in terminal_public_states
        activity_terminal = isinstance(activity_state, str) and activity_state in terminal_public_states
        if (status is not None and not status_terminal) or (activity is not None and not activity_terminal):
            return False
        is_terminated = view.get('isTerminated')
        if is_terminated is False and (status_terminal or activity_terminal):
            return False
        return is_terminated is True or status_terminal or activity_terminal
    if not public_matches():
        raise ValueError('terminated owner disagrees with public session')
    project_view = api.request('projects/' + project)
    config = project_view.get('project', {})
    if project_view.get('status') != 'ok' or config.get('id') != project:
        raise ValueError('trusted project repository unavailable')
    repository = Path(config.get('path', ''))
    if not repository.is_absolute() or not repository.is_dir():
        raise ValueError('existing configured repository required')
    workspace = Path(owner['workspace_path'])
    managed_workspace = _ao_worktree_target(db, project, session, config)
    if (not workspace.is_absolute()
            or os.path.normpath(str(workspace)) != os.path.normpath(str(managed_workspace))):
        raise ValueError('workspace target does not match AO-managed target')
    configured = config.get('config', {}).get('env', {})
    scopes = ([configured.get('CODEX_HOME')] if isinstance(configured, dict) else
              [item[len('CODEX_HOME='):] for item in configured if isinstance(item, str) and item.startswith('CODEX_HOME=')])
    if scopes != [home]:
        raise ValueError('configured Chat account scope mismatch')
    def common_git(path):
        value = subprocess.check_output(['git', '-C', str(path), 'rev-parse', '--path-format=absolute', '--git-common-dir'], text=True).strip()
        return Path(value).resolve()
    if not isinstance(branch, str) or not branch:
        raise ValueError('session branch unavailable')
    if subprocess.run(['git', '-C', str(repository), 'check-ref-format', '--branch', branch],
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0:
        raise ValueError('session branch invalid')
    if subprocess.run(['git', '-C', str(repository), 'show-ref', '--verify', '--quiet',
                       'refs/heads/' + branch]).returncode != 0:
        raise ValueError('session branch unavailable in repository')
    registered = subprocess.check_output(['git', '-C', str(repository), 'worktree', 'list', '--porcelain'], text=True)
    entry = _worktree_entry(registered, workspace)
    absent = _definitely_absent(workspace)
    if absent:
        conflict = _registered_branch_elsewhere(registered, branch, workspace)
        if conflict is not None:
            raise ValueError('session branch is registered at another worktree')
        if entry is not None:
            # Go AO's Restore returns a registered record as-is, even when Git
            # marks its missing path prunable. It will not recreate that path.
            raise ValueError('missing workspace has a prunable worktree registration')
    else:
        if (workspace.is_symlink() or not workspace.is_dir()
                or workspace.stat().st_uid != os.getuid() or not (workspace / '.git').is_file()):
            raise ValueError('existing owned worktree required')
        if common_git(workspace) != common_git(repository):
            raise ValueError('worktree belongs to a different project repository')
        if entry is None or entry[0] != branch:
            raise ValueError('workspace is not the registered session branch')
    matched = False
    for name in ('state_5.sqlite', 'state.sqlite'):
        path = Path(home) / name
        if not path.is_file():
            continue
        with sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True) as conn:
            thread = conn.execute('SELECT cwd FROM threads WHERE id=?', (owner['provider_conversation_id'],)).fetchone()
        if thread is not None:
            if thread[0] != str(workspace):
                raise ValueError('scoped provider workspace mismatch')
            matched = True
    if not matched:
        raise ValueError('exact provider thread unavailable in intended account scope')
    for process in _processes(proc):
        if not process.name.isdigit():
            continue
        try:
            if process.stat().st_uid != os.getuid():
                continue
            args = (process / 'cmdline').read_bytes().split(b'\0')
            if b'app-server' not in args:
                continue
            # Only Codex app-server candidates can own this Chat session.
            # Fail closed if candidate identity is unreadable below.
            exe = (process / 'exe').resolve().name.removesuffix(' (deleted)')
            if exe != 'codex':
                continue
            env = dict(x.split(b'=', 1) for x in (process / 'environ').read_bytes().split(b'\0') if b'=' in x)
            if env.get(b'AO_SESSION_ID') == session.encode():
                process_project = env.get(b'AO_PROJECT_ID')
                process_home = env.get(b'CODEX_HOME')
                if not process_project or not process_home:
                    raise ValueError('matching Chat process identity is incomplete')
                if process_project == project.encode() and process_home == home.encode():
                    raise ValueError('existing Chat host still owns terminated session')
        except FileNotFoundError:
            continue
        except OSError as error:
            raise ValueError('process visibility unavailable') from error
    if not public_matches() or api.request('projects/' + project) != project_view:
        raise ValueError('terminated ownership or project changed during preflight')
    latest_owner = row(db, project, session)
    latest_branch, latest_activity = restore_state(db, project, session)
    if latest_owner != owner or latest_branch != branch or latest_activity != activity_state:
        raise ValueError('terminated ownership or project changed during preflight')
    return owner


def timestamp(value):
    """Compare RFC3339 API and Go SQLite times without dropping nanoseconds."""
    value = re.sub(r' ([+-]\d{2})(\d{2}) [A-Za-z0-9_+:-]+$', r'\1:\2', value)
    match = re.fullmatch(r'(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})', value)
    if not match:
        raise ValueError('invalid timezone-aware timestamp')
    whole = datetime.fromisoformat(match[1] + match[3].replace('Z', '+00:00'))
    return int(whole.timestamp()), int((match[2] or '').ljust(9, '0'))


def precise_turn_times(db, owner, snapshot, only_bound=False):
    # v0.13.3's public DTO truncates requestedAt to seconds. Recover precision
    # only from the exact persisted turn, bound to this session/controller and
    # provider turn. Async admission may have an empty stored generation;
    # current conversation ownership is still mandatory. Never round admission
    # backward to make a receipt pass.
    bound = set()
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        for turn in snapshot.get('turns', []):
            if not turn.get('providerTurnId'):
                continue
            stored = conn.execute(
                'SELECT t.requested_at FROM conversation_turns t '
                'JOIN conversations c ON c.id=t.conversation_id '
                'WHERE t.id=? AND t.handled_by_session_id=? '
                "AND (t.controller_generation=? OR t.controller_generation='') "
                'AND t.provider_turn_id=? AND t.rolled_back_at IS NULL '
                'AND c.id=? AND c.current_session_id=? AND c.project_id=?',
                (turn['id'], owner['id'], owner['controller_generation'], turn['providerTurnId'],
                 snapshot.get('conversationId'), owner['id'], owner['project_id'])).fetchone()
            if stored is None:
                continue
            if timestamp(stored[0])[0] != timestamp(turn['requestedAt'])[0]:
                raise ValueError('public and persisted turn timestamps disagree')
            turn['requestedAt'] = stored[0]
            bound.add(turn['id'])
    if only_bound:
        snapshot['turns'] = [turn for turn in snapshot.get('turns', []) if turn['id'] in bound]


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
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        tmp.unlink(missing_ok=True)


@contextmanager
def delivery_lock(path):
    # Keep the inode after use; concurrent callers must lock the same file.
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(path.with_name(path.name + '.lock'), os.O_WRONLY | os.O_CREAT, 0o600)
    with os.fdopen(fd, 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def _spawn_receipt(path, pending):
    token = pending.get('spawnReceipt')
    if not isinstance(token, str) or not re.fullmatch(r'[a-f0-9]{32}', token):
        raise ValueError('native spawn receipt unavailable')
    receipt = path.with_name(path.name + '.spawn-' + token + '.log')
    fd = os.open(receipt, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'r') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or stat.S_IMODE(info.st_mode) != 0o600 or info.st_size > 1024 * 1024):
            raise ValueError('native spawn receipt identity mismatch')
        os.fsync(stream.fileno())
        output = stream.read()
    return receipt, output


def spawn_receipt_path(path, text):
    with delivery_lock(path):
        pending = json.loads(path.read_text())
        if pending.get('initial') is not True or pending.get('session') != '' or pending.get('text') != text:
            raise ValueError('spawn admission identity mismatch')
        return _spawn_receipt(path, pending)[0]


def _spawn_receipt_session(path, pending):
    _, output = _spawn_receipt(path, pending)
    ids = set(re.findall(r'^spawned session ([A-Za-z0-9._-]+) [^\n]*\n', output, re.M))
    return next(iter(ids)) if len(ids) == 1 else None


def _recover_spawn_reservation(path, pending):
    if pending.get('initial') is True and pending.get('session') == '' and pending.get('spawnReceipt'):
        session = _spawn_receipt_session(path, pending)
        if session:
            pending['session'] = session
            save(path, pending)
        else:
            # Transport errors and age do not prove AO failed to create a
            # session. Keep the reservation until its exact receipt names the
            # owner; retrying admission could create a duplicate worker.
            raise ValueError('ambiguous native spawn outcome; admission remains reserved')


def reserved_session(path):
    with delivery_lock(path):
        pending = json.loads(path.read_text())
        if pending.get('initial') is not True:
            raise ValueError('initial spawn reservation required')
        _recover_spawn_reservation(path, pending)
        session = pending.get('session')
        if not isinstance(session, str) or not re.fullmatch(r'[A-Za-z0-9._-]+', session):
            raise ValueError('exact native spawn identity unavailable')
        return session


def admission(path, text, session=None):
    """Reserve before spawn; bind only to the explicit native spawn receipt.

    Anonymous reservations deliberately do not expire: a failed CLI transport
    does not prove AO failed to create the session. Recovery may repeat binding
    with the original exact native spawn receipt and unchanged prompt. The
    private per-attempt CLI output remains on disk; delivery can recover its
    exact session ID after a controller crash, then validate the native owner
    and provider turn. Completely missing receipts still require investigation.
    Never delete/retry solely because the CLI failed or a session listing is empty.
    """
    try:
        with delivery_lock(path):
            if session is None:
                if path.exists():
                    return 4
                token = uuid.uuid4().hex
                receipt = path.with_name(path.name + '.spawn-' + token + '.log')
                fd = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                try:
                    os.fsync(fd)
                finally:
                    os.close(fd)
                # save() fsyncs the containing directory before any spawn.
                save(path, {'session': '', 'text': text, 'initial': True, 'spawnReceipt': token})
            else:
                if not re.fullmatch(r'[A-Za-z0-9._-]+', session):
                    return 4
                pending = json.loads(path.read_text())
                if (pending.get('initial') is not True or pending.get('text') != text
                        or pending.get('session') not in ('', session)):
                    return 4
                if pending.get('spawnReceipt') and _spawn_receipt_session(path, pending) != session:
                    return 4
                pending['session'] = session
                save(path, pending)
            return 0
    except BlockingIOError:
        return 4


def pin_initial_turn(db, owner, snapshot, path, pending, home):
    """Pin queued initial identity while its original native owner is verified."""
    if (not pending.get('initial') or pending.get('initialTurn')
            or not snapshot.get('conversationId') or pending.get('text') != owner['prompt']):
        return
    turns = {t['id']: t for t in snapshot.get('turns', []) if not t.get('rolledBack')}
    candidates = {message.get('turnId') for message in snapshot.get('messages', [])
                  if message.get('role') == 'user' and message.get('text') == pending['text']}
    matches = []
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        for turn_id in candidates & turns.keys():
            turn = turns[turn_id]
            stored = conn.execute(
                'SELECT t.provider_turn_id,t.requested_at FROM conversation_turns t '
                'JOIN conversations c ON c.id=t.conversation_id '
                'WHERE t.id=? AND t.handled_by_session_id=? '
                "AND (t.controller_generation=? OR t.controller_generation='') "
                'AND t.rolled_back_at IS NULL AND c.id=? AND c.current_session_id=? AND c.project_id=?',
                (turn_id, owner['id'], owner['controller_generation'], snapshot['conversationId'], owner['id'], owner['project_id'])).fetchone()
            if (stored is None or timestamp(stored[1]) < timestamp(owner['created_at'])
                    or timestamp(stored[1])[0] != timestamp(turn['requestedAt'])[0]
                    or (turn.get('providerTurnId') and turn['providerTurnId'] != stored[0])):
                continue
            matches.append({'id': turn_id, 'providerTurnId': stored[0],
                            'conversationId': snapshot['conversationId'], 'controllerGeneration': owner['controller_generation']})
    if len(matches) == 1 and row(db, owner['project_id'], owner['id']) == owner:
        pending['initialTurn'] = matches[0]
        pending['scope'] = home
        save(path, pending)


def reconcile_initial(api, db, project, session, home, path):
    """Read-only native evidence can retire an initial receipt after an epoch change.

    Provider messages are never sent here. Preserve the original generation for
    stored-turn proof, independently verify current immutable owner/account, and
    reject failed, rolled-back or unbound historical turns. Ordinary pending
    sends keep their existing generation fence and are never handled here.
    """
    try:
        with delivery_lock(path):
            pending = json.loads(path.read_text())
            if (pending.get('initial') is not True or pending.get('session') != session
                    or pending.get('scope') != home
                    or not set(pending).issubset({'session', 'text', 'initial', 'spawnReceipt', 'owner', 'scope', 'initialTurn'})):
                return 4
            provenance = pending.get('initialTurn')
            if (not isinstance(provenance, dict)
                    or set(provenance) != {'id', 'providerTurnId', 'conversationId', 'controllerGeneration'}
                    or not isinstance(pending.get('owner'), dict)
                    or not all(isinstance(provenance[k], str) and provenance[k] for k in ('id', 'conversationId', 'controllerGeneration'))
                    or (provenance['providerTurnId'] is not None and not isinstance(provenance['providerTurnId'], str))
                    or provenance['controllerGeneration'] != pending['owner'].get('controller_generation')):
                return 4
            current = row(db, project, session)
            if current['is_terminated']:
                owner = restore_preflight(api, db, project, session, home)
                snapshot = api.request('sessions/' + session + '/conversation?limit=500')
            else:
                snapshot, owner = validate(api, db, project, session, home)
            if (snapshot.get('sessionId') != session or snapshot.get('mode') != 'chat'
                    or snapshot.get('harness') != 'codex'):
                return 4
            original = pending.get('owner', owner)
            immutable = set(owner) - {'is_terminated', 'controller_generation'}
            if (not isinstance(original, dict) or set(original) != set(owner)
                    or any(original[key] != owner[key] for key in immutable)):
                return 4
            # Native import timestamps are import time, not send time. Only the
            # original turn pinned before epoch change may acknowledge this ledger.
            if snapshot.get('conversationId') != provenance['conversationId']:
                return 4
            snapshot['turns'] = [turn for turn in snapshot.get('turns', [])
                                 if turn['id'] == provenance['id']
                                 and (not provenance['providerTurnId']
                                      or turn.get('providerTurnId') == provenance['providerTurnId'])]
            precise_turn_times(db, original, snapshot, only_bound=True)
            if not acknowledged(snapshot, pending.get('text'), provenance['id'], initial_owner=original):
                return 4
            if row(db, project, session) != owner:
                return 4
            path.unlink()
            return 0
    except BlockingIOError:
        return 4


def prepare_restore(api, db, project, session, home, path, text, proc=Path('/proc')):
    """Reserve the existing owner before the non-idempotent native restore."""
    try:
        with delivery_lock(path):
            if path.exists():
                return 4
            owner = restore_preflight(api, db, project, session, home, proc)
            identity = {key: owner[key] for key in ('id', 'project_id', 'workspace_path',
                        'provider_conversation_id', 'harness', 'session_mode')}
            save(path, {'session': session, 'restore': identity, 'scope': home, 'text': text,
                        'request': uuid.uuid4().hex})
            return 0
    except BlockingIOError:
        return 4


def deliver(api, db, project, session, home, path, text, initial=False, retain_initial=False):
    try:
        with delivery_lock(path):
            return _deliver_locked(api, db, project, session, home, path, text, initial, retain_initial)
    except BlockingIOError:
        return 4


def _deliver_locked(api, db, project, session, home, path, text, initial=False, retain_initial=False):
    if retain_initial:
        observed = json.loads(path.read_text()) if path.exists() else {}
        if (observed.get('initial') is not True
                or not set(observed).issubset({'session', 'text', 'initial', 'spawnReceipt', 'owner', 'scope', 'initialTurn'})):
            return 4
    if initial and not path.exists():
        save(path, {"session": session, "text": text, "initial": True})
    snapshot, owner = validate(api, db, project, session, home)
    requested_text = text
    pending = None
    if path.exists():
        pending = json.loads(path.read_text())
        _recover_spawn_reservation(path, pending)
        if pending.get('session') != session or pending.get('scope', home) != home or (pending.get('owner') is not None and pending['owner'] != owner):
            return 4
        if pending.get('restore'):
            required = {'id', 'project_id', 'workspace_path', 'provider_conversation_id', 'harness', 'session_mode'}
            if not isinstance(pending['restore'], dict) or set(pending['restore']) != required:
                return 4
            if any(owner.get(key) != value for key, value in pending['restore'].items()):
                return 4
            if snapshot.get('controller') == 'busy' or any(t.get('state') in ('running', 'queued') for t in snapshot.get('turns', [])):
                return 5
            if snapshot.get('controller') != 'ready':
                return 4
            # Successful native restore rotates the controller generation, but
            # must preserve the exact provider conversation/workspace/account.
            # Persist the ordinary pending request before its one initial POST.
            text = pending['text']
            pending = {'session': session, 'owner': owner, 'scope': home, 'text': text, 'request': pending['request']}
            save(path, pending)
            if row(db, project, session) != owner:
                return 4
            receipt = api.request('sessions/' + session + '/conversation/steer-or-send',
                                  {'text': text, 'clientMessageId': pending['request']})
            if receipt.get('outcome') != 'sent' or not receipt.get('turnId'):
                return 4
            pending['turnId'] = receipt['turnId']; save(path, pending)
        else:
            if pending.get('owner') is None:
                if not pending.get('initial'):
                    return 4
                pending['owner'] = owner; pending['scope'] = home; save(path, pending)
            text = pending['text']
            pin_initial_turn(db, owner, snapshot, path, pending, home)
            if (pending.get('initial') or pending.get('turnId')) and acknowledged(snapshot, text, pending.get('turnId') or pending.get('initialTurn', {}).get('id'), owner if pending.get('initial') else None):
                if not retain_initial:
                    path.unlink()
                return 0 if text == requested_text else 6
            if not pending.get("initial"):
                # No replay on timeout, uncertain transport, missing or queued receipt.
                try:
                    receipt = api.request('sessions/' + session + '/conversation/steer-or-send',
                                          {'clientMessageId': pending['request'], 'recoverOnly': True})
                    if receipt.get('outcome') != 'sent' or not receipt.get('turnId'):
                        return 4
                    pending['turnId'] = receipt['turnId']; save(path, pending)
                except (OSError, ValueError):
                    return 4
            # A bound initial admission only observes below. Provisioning may not
            # have attached the provider turn when the CLI first returns its ID.
    elif initial:
        # Spawn carries the initial prompt. Observe it; never send a second copy.
        if acknowledged(snapshot, text, initial_owner=owner):
            return 0
        pending = {'session': session, 'owner': owner, 'text': text, 'initial': True}
        save(path, pending)
    else:
        if snapshot.get('controller') == 'busy' or any(t.get('state') in ('running', 'queued') for t in snapshot.get('turns', [])):
            return 5
        if snapshot.get('controller') != 'ready':
            return 4
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
        pin_initial_turn(db, owner, current, path, pending, home)
        if acknowledged(current, text, pending.get('turnId') or pending.get('initialTurn', {}).get('id'), owner if pending.get('initial') else None):
            if not retain_initial:
                path.unlink()
            return 0 if text == requested_text else 6
    return 4


if __name__ == '__main__':
    try:
        if len(sys.argv) in (3, 4) and sys.argv[1] == '--admission':
            code = admission(Path(sys.argv[2]), sys.stdin.read(), sys.argv[3] if len(sys.argv) == 4 else None)
            sys.exit(code)
        if len(sys.argv) == 3 and sys.argv[1] == '--reserved-session':
            print(reserved_session(Path(sys.argv[2])))
            sys.exit(0)
        if len(sys.argv) == 3 and sys.argv[1] == '--spawn-receipt-path':
            print(spawn_receipt_path(Path(sys.argv[2]), sys.stdin.read()))
            sys.exit(0)
        if len(sys.argv) == 8 and sys.argv[1] == '--reconcile-initial':
            base, db, project, session, home, pending = sys.argv[2:]
            if not all(re.fullmatch(r'[A-Za-z0-9._-]+', item) for item in (project, session)):
                raise ValueError('invalid identity')
            sys.exit(reconcile_initial(API(base), db, project, session, home, Path(pending)))
        if len(sys.argv) == 8 and sys.argv[1] == '--prepare-restore':
            base, db, project, session, home, pending = sys.argv[2:]
            if not all(re.fullmatch(r'[A-Za-z0-9._-]+', item) for item in (project, session)):
                raise ValueError('invalid identity')
            sys.exit(prepare_restore(API(base), db, project, session, home, Path(pending), sys.stdin.read()))
        base, db, project, session, home, pending, initial = sys.argv[1:]
        if not all(re.fullmatch(r'[A-Za-z0-9._-]+', item) for item in (project, session)):
            raise ValueError('invalid identity')
        # Prompt comes over stdin, avoiding process argument/log disclosure.
        code = deliver(API(base), db, project, session, home, Path(pending), sys.stdin.read(),
                       initial in ('initial', 'observe-initial'), initial == 'observe-initial')
        sys.exit(code)
    except (OSError, ValueError, KeyError, sqlite3.Error, subprocess.CalledProcessError) as error:
        detail = json.dumps({'type': 'chat_preflight_error',
                             'error': type(error).__name__,
                             'message': str(error)}, separators=(',', ':'))
        print('Chat delivery unconfirmed: ' + type(error).__name__ +
              '; CHAT_PREFLIGHT_ERROR=' + detail, file=sys.stderr)
        sys.exit(4)
