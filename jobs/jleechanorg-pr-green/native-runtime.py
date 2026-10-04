#!/usr/bin/env python3
"""Read-only, generation-fenced Linux Codex identity for native AO runtimes."""
import json
import os
from pathlib import Path
import re
import sqlite3
import sys
import urllib.request
from urllib.parse import urlsplit


def snapshot(db, project, session):
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute(
            'SELECT id,project_id,is_terminated,workspace_path,runtime_handle_id,'
            'runtime_launch_id,agent_session_id,agent_session_id_launch_id,session_mode '
            'FROM sessions WHERE id=? AND project_id=?', (session, project)).fetchone()
        return dict(row) if row else None


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('AO observation redirects are forbidden')


def session_mode(db, project, session):
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as conn:
        cols = {row[1] for row in conn.execute('PRAGMA table_info(sessions)')}
        field = 'session_mode' if 'session_mode' in cols else "'tui'"
        row = conn.execute('SELECT '+field+' FROM sessions WHERE id=? AND project_id=?', (session, project)).fetchone()
        if not row or row[0] not in ('chat', 'tui'):
            raise ValueError('session mode unavailable')
        return row[0]


def starttime(entry):
    stat = (entry / 'stat').read_text()
    return stat[stat.rfind(')') + 2:].split()[19]


def view(api, session):
    url = urlsplit(api)
    if url.scheme != 'http' or url.hostname not in ('127.0.0.1', 'localhost') or url.username or url.password or url.query or url.fragment or url.path not in ('', '/'):
        raise ValueError('AO observation requires a loopback HTTP endpoint')
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(api.rstrip('/') + '/api/v1/sessions/' + session, timeout=5) as reply:
        data = json.load(reply)
    return data['session']


def validate(row, observed):
    if not row or row['is_terminated'] or row['session_mode'] != 'tui':
        raise ValueError('session is unavailable or not a live TUI')
    generation = row['runtime_launch_id']
    if not generation or not row['runtime_handle_id'].startswith('ptyhost-v1:'):
        raise ValueError('native runtime generation unavailable')
    if row['agent_session_id'] and row['agent_session_id_launch_id'] != generation:
        raise ValueError('native identity belongs to an earlier generation')
    if (observed.get('id') != row['id'] or observed.get('projectId') != row['project_id']
            or observed.get('isTerminated') is not False or observed.get('mode') != 'tui'
            or observed.get('terminalGeneration') != generation
            or observed.get('terminalHandleId') != row['runtime_handle_id']):
        raise ValueError('daemon and database identities disagree')


def process_home(row, proc=Path('/proc')):
    matches = []
    for entry in proc.iterdir():
        if not entry.name.isdigit():
            continue
        try:
            before = starttime(entry)
            env = dict(part.split(b'=', 1) for part in (entry / 'environ').read_bytes().split(b'\0') if b'=' in part)
            if (env.get(b'AO_SESSION_ID') != row['id'].encode()
                    or env.get(b'AO_PROJECT_ID') != row['project_id'].encode()
                    or env.get(b'AO_RUNTIME_LAUNCH_ID') != row['runtime_launch_id'].encode()
                    or str((entry / 'cwd').resolve()) != row['workspace_path']):
                continue
            exe = (entry / 'exe').resolve().name
            args = (entry / 'cmdline').read_bytes().split(b'\0')
            if not (exe == 'codex' or exe.startswith('codex-') or
                    (exe == 'node' and any(part.endswith(b'/codex.js') for part in args))):
                continue
            home = env.get(b'CODEX_HOME', b'').decode()
            if home and before == starttime(entry):
                matches.append(home)
        except (OSError, ValueError, UnicodeError):
            continue
    if len(matches) != 1:
        raise ValueError('exact unique Codex process unavailable')
    return matches[0]


def observe(db, api, project, session):
    if not all(re.fullmatch(r'[A-Za-z0-9._-]+', x) for x in (project, session)):
        raise ValueError('invalid generated identity')
    row = snapshot(db, project, session)
    validate(row, view(api, session))
    home = process_home(row)
    if snapshot(db, project, session) != row:
        raise ValueError('runtime changed during observation')
    current = view(api, session)
    validate(row, current)
    return {'home': home, 'launchId': row['runtime_launch_id'],
            'nativeId': row['agent_session_id'], 'activity': current.get('activity', {}).get('state', '')}


if __name__ == '__main__':
    try:
        if len(sys.argv) == 5 and sys.argv[1] == '--mode':
            print(session_mode(*sys.argv[2:])); sys.exit(0)
        if len(sys.argv) != 5:
            raise ValueError('expected database, loopback API, project, session')
        print(json.dumps(observe(*sys.argv[1:])))
    except (OSError, ValueError, KeyError, sqlite3.Error) as error:
        print('native runtime identity unavailable: ' + str(error), file=sys.stderr)
        sys.exit(1)
