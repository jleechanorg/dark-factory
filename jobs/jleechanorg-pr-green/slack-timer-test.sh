#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
timer="$job_dir/jleechanorg-pr-green-slack-report.timer"

[[ -f "$timer" ]] || { echo 'FAIL: Slack report timer is missing' >&2; exit 1; }
grep -Fq 'OnCalendar=*-*-* *:00/5:00' "$timer" || {
  echo 'FAIL: Slack report timer is not a five-minute wall-clock periodic timer' >&2
  exit 1
}
if grep -Eq '^(OnBootSec|OnUnitActiveSec)=' "$timer"; then
  echo 'FAIL: Slack report timer still relies on a monotonic anchor' >&2
  exit 1
fi
grep -Fq 'Persistent=true' "$timer" || {
  echo 'FAIL: Slack report timer is not persistent' >&2
  exit 1
}
grep -Fq 'Unit=jleechanorg-pr-green-slack-report.service' "$timer" || {
  echo 'FAIL: Slack report timer does not activate the report service' >&2
  exit 1
}
systemd-analyze calendar '*-*-* *:00/5:00' >/dev/null || {
  echo 'FAIL: Slack report calendar expression is not understood by systemd' >&2
  exit 1
}

echo 'jleechanorg-pr-green Slack timer: PASS'
