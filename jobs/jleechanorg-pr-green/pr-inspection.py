#!/usr/bin/env python3
"""Bounded private diagnostics for read-only PR inspection; never contacts AO."""
import datetime
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid

STDERR_LIMIT = 8192
TIMEOUT = 30


def private_directory(path):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError('diagnostic directory must be private and owned')


def initialize(metrics, run_started):
    if not run_started.isdecimal():
        raise ValueError('invalid run timestamp')
    base = Path(metrics) / 'inspection-diagnostics'
    try:
        base.mkdir(mode=0o700)
    except FileExistsError:
        pass
    private_directory(base)
    return tempfile.mkdtemp(prefix=run_started + '-', dir=base)


def sanitized_stderr(value):
    raw = value if isinstance(value, bytes) else value.encode('utf-8', errors='replace')
    text = raw[:65536].decode('utf-8', errors='replace')
    for name, secret in os.environ.items():
        if len(secret) >= 4 and re.search(r'(TOKEN|SECRET|PASSWORD|API_KEY)$', name):
            text = text.replace(secret, '[REDACTED]')
    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
    text = re.sub(r'(?im)^.*(?:authorization|password|secret|api[_-]?key|(?:access[_-]?)?token)\s*["\x27]?\s*[:=].*$', '[REDACTED credential line]', text)
    text = re.sub(r'(?i)\b(?:bearer|basic)\s+\S+', '[REDACTED authorization]', text)
    text = re.sub(r'https?://\S+', '[URL]', text)
    text = re.sub(r'\b(?:github_pat_|gh[pousr]_)[A-Za-z0-9_]+', '[REDACTED token]', text)
    text = re.sub(r'[A-Za-z0-9_+/=-]{32,}', '[REDACTED long value]', text)
    text = ''.join(c for c in text if c in '\n\t' or ord(c) >= 32)
    data = text.encode('utf-8')
    return data[:STDERR_LIMIT].decode('utf-8', errors='ignore'), len(raw) > 65536 or len(data) > STDERR_LIMIT


def classification(stderr, code):
    message = stderr.lower()
    # Denials and rate limits always beat transient-looking text elsewhere.
    if re.search(r'\b(?:401|403)\b|unauthori[sz]ed|forbidden|authentication|not logged|requires? auth|resource not accessible|bad credentials|policy', message):
        return 'authorization', False
    if 'rate limit' in message or re.search(r'\b429\b', message):
        return 'rate_limit', False
    if re.search(r'\bhttp(?:/\S+)?[ :]+(?:502|503|504)\b', message):
        return 'transient_http', True
    if any(s in message for s in ('connection reset by peer', 'tls handshake timeout', 'i/o timeout', 'temporary failure in name resolution')):
        return 'transient_network', True
    return 'unknown', False


def capture(command, payload=None):
    try:
        result = subprocess.run(command, input=payload, capture_output=True, text=True, timeout=TIMEOUT)
        return result.returncode, result.stdout, result.stderr, False
    except subprocess.TimeoutExpired as error:
        stderr = error.stderr or b''
        if isinstance(stderr, bytes):
            stderr = stderr.decode('utf-8', errors='replace')
        return 124, '', stderr + '\nRead-only inspection timed out.', True
    except OSError:
        return 127, '', 'Inspection executable could not be started.', False


def save(directory, record):
    private_directory(directory)
    path = directory / (record['repo'] + '-' + str(record['number']) + '-' + uuid.uuid4().hex + '.json')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(record, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def inspect(url, directory, projection, phase):
    match = re.fullmatch(r'https://github\.com/jleechanorg/([A-Za-z0-9_.-]{1,100})/pull/([1-9][0-9]*)', url)
    if not match or phase not in ('inspection', 'reconciliation'):
        raise ValueError('invalid inspection scope')
    private_directory(directory)
    record = {'repo': match[1], 'number': int(match[2]), 'phase': phase,
              'run_started': int(os.environ['PR_GREEN_INSPECTION_RUN_STARTED']),
              'observed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'gh_executable': shutil.which('gh'),
              'token_environment_present': {k: bool(os.environ.get(k)) for k in ('GH_TOKEN', 'GITHUB_TOKEN')},
              'attempts': []}
    command = ['gh', 'pr', 'view', url, '--json', 'headRefOid,mergeable,mergeStateStatus,statusCheckRollup,baseRefName,headRepository']
    for attempt in range(1, 3):
        code, payload, stderr, timed_out = capture(command)
        kind, retry = classification(stderr, code) if code else ('success', False)
        stage = 'github'
        if timed_out:
            # An opaque timeout alone cannot distinguish auth helpers/policy.
            kind, retry = 'timeout', False
        if code == 0:
            try:
                data = json.loads(payload)
                if (not isinstance(data, dict) or not isinstance(data.get('headRefOid'), str)
                        or not data['headRefOid'] or data.get('mergeable') not in ('MERGEABLE', 'CONFLICTING', 'UNKNOWN')
                        or 'statusCheckRollup' not in data
                        or (data['statusCheckRollup'] is not None and not isinstance(data['statusCheckRollup'], list))):
                    raise ValueError('invalid PR fields')
            except (ValueError, TypeError):
                code, kind, stage, stderr = 65, 'invalid_response', 'response', 'GitHub returned malformed JSON or missing required PR fields.'
            else:
                code, output, projection_stderr, _ = capture(
                    ['bash', '-c', 'source "$1"; pr_green_state_from_pr_json', 'pr-green-inspection', str(projection)], payload)
                if code or not output.strip():
                    code, kind, stage = code or 65, 'projection', 'projection'
                    stderr = projection_stderr or 'PR state projection returned no data.'
                else:
                    sanitized, truncated = sanitized_stderr(stderr)
                    record['attempts'].append({'attempt': attempt, 'stage': stage, 'exit_code': 0,
                                               'classification': 'success', 'stderr': sanitized, 'stderr_truncated': truncated})
                    if attempt > 1 or stderr:
                        record['outcome'] = 'recovered' if attempt > 1 else 'success'
                        save(directory, record)
                    print(output.strip())
                    return 0
        sanitized, truncated = sanitized_stderr(stderr)
        record['attempts'].append({'attempt': attempt, 'stage': stage, 'exit_code': code,
                                   'classification': kind, 'stderr': sanitized, 'stderr_truncated': truncated})
        if retry and attempt == 1:
            time.sleep(1)
            continue
        record['outcome'] = 'failed'
        save(directory, record)
        print('PR inspection failed: ' + kind + '; private diagnostic saved.', file=sys.stderr)
        return 1
    return 1


if __name__ == '__main__':
    try:
        if len(sys.argv) == 4 and sys.argv[1] == '--init':
            print(initialize(sys.argv[2], sys.argv[3]))
        elif len(sys.argv) == 6 and sys.argv[1] == '--inspect':
            sys.exit(inspect(sys.argv[2], Path(sys.argv[3]), Path(sys.argv[4]), sys.argv[5]))
        else:
            raise ValueError('invalid diagnostic command')
    except (OSError, ValueError, KeyError):
        print('Private PR inspection diagnostics unavailable; inspection failed closed.', file=sys.stderr)
        sys.exit(1)
