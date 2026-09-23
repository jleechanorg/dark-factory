#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SERVICE="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.service"
TIMER="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.timer"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

[[ -f "$TIMER" ]] || { echo 'FAIL: daily repair timer is missing' >&2; exit 1; }
mapfile -t calendar_values < <(awk -F= '$1 == "OnCalendar" { print substr($0, index($0, "=") + 1) }' "$TIMER")
[[ "${#calendar_values[@]}" -eq 1 && "${calendar_values[0]}" == '*-*-* *:00/30:00' ]] || {
  echo 'FAIL: daily repair timer is not the exact wall-clock 30-minute calendar value' >&2
  exit 1
}
if grep -Eq '^(OnBootSec|OnUnitActiveSec)=' "$TIMER"; then
  echo 'FAIL: daily repair timer still relies on monotonic anchors' >&2
  exit 1
fi
systemd-analyze calendar "${calendar_values[0]}" >/dev/null || {
  echo 'FAIL: daily repair calendar expression is not understood by systemd' >&2
  exit 1
}
grep -Fq 'Unit=jleechanorg-pr-green-daily.service' "$TIMER" || { echo 'FAIL: timer does not activate daily repair service' >&2; exit 1; }
grep -Fq 'Environment=CODEX_HOME=%h/.codex-dark-factory' "$SERVICE" || {
  echo 'FAIL: repair service does not use its project-scoped CODEX_HOME' >&2
  exit 1
}
grep -Fq 'ExecStartPre=/usr/bin/test -s %h/.codex-dark-factory/auth.json' "$SERVICE" || {
  echo 'FAIL: repair service does not fail closed when the intended Codex login is absent' >&2
  exit 1
}
default_codex_home_re='^Environment=CODEX_HOME=%h/\.codex/?$'
positive_fixture="$fixture_dir/default-codex-home.service"
negative_fixture="$fixture_dir/scoped-codex-home.service"
printf '%s\n' 'Environment=CODEX_HOME=%h/.codex/' >"$positive_fixture"
printf '%s\n' 'Environment=CODEX_HOME=%h/.codex-dark-factory' >"$negative_fixture"
grep -Eq "$default_codex_home_re" "$positive_fixture" || {
  echo 'FAIL: default CODEX_HOME detector did not match its positive fixture' >&2
  exit 1
}
if grep -Eq "$default_codex_home_re" "$negative_fixture"; then
  echo 'FAIL: default CODEX_HOME detector matched its negative fixture' >&2
  exit 1
fi
if grep -Eq "$default_codex_home_re" "$SERVICE"; then
  echo 'FAIL: repair service still uses operator-default CODEX_HOME' >&2
  exit 1
fi

echo 'jleechanorg-pr-green systemd units: PASS'
