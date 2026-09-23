#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SERVICE="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.service"
TIMER="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.timer"

[[ -f "$TIMER" ]] || { echo 'FAIL: daily repair timer is missing' >&2; exit 1; }
grep -Fq 'OnUnitActiveSec=30min' "$TIMER" || { echo 'FAIL: daily repair timer is not 30-minute periodic' >&2; exit 1; }
grep -Fq 'Unit=jleechanorg-pr-green-daily.service' "$TIMER" || { echo 'FAIL: timer does not activate daily repair service' >&2; exit 1; }
grep -Fq 'Environment=CODEX_HOME=%h/.codex-dark-factory' "$SERVICE" || {
  echo 'FAIL: repair service does not use its project-scoped CODEX_HOME' >&2
  exit 1
}
grep -Fq 'ExecStartPre=/usr/bin/test -s %h/.codex-dark-factory/auth.json' "$SERVICE" || {
  echo 'FAIL: repair service does not fail closed when the intended Codex login is absent' >&2
  exit 1
}
if grep -Fq 'Environment=CODEX_HOME=%h/.codex$' "$SERVICE"; then
  echo 'FAIL: repair service still uses operator-default CODEX_HOME' >&2
  exit 1
fi

echo 'jleechanorg-pr-green systemd units: PASS'
