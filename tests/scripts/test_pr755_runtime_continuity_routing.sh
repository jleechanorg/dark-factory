#!/usr/bin/env bash
# Contract test for the PR755 continuity canary's CI routing boundary.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CANARY="$ROOT/tests/scripts/test_pr755_runtime_continuity.sh"
CI_WORKFLOW="$ROOT/.github/workflows/ci.yml"
TMP="$(mktemp -d -t df-pr755-continuity-routing.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT INT TERM

ci_loop="$(sed -n '/for t in tests\/scripts\/test_\*\.sh; do/,/exit \$fail/p' "$CI_WORKFLOW")"
grep -Fq 'if [ "$t" = "tests/scripts/test_pr755_runtime_continuity.sh" ]; then' <<<"$ci_loop" || {
  echo "FAIL: generic CI bash loop does not exclude the live continuity canary" >&2
  exit 1
}
grep -Fq 'SKIP: PR755 continuity canary requires explicit jeff-ubuntu invocation' <<<"$ci_loop" || {
  echo "FAIL: CI exclusion is not loud about the skipped continuity canary" >&2
  exit 1
}

mkdir -p "$TMP/bin"
cat >"$TMP/bin/uname" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' Darwin
EOF
chmod +x "$TMP/bin/uname"

set +e
direct_output="$(PATH="$TMP/bin:$PATH" bash "$CANARY" 2>&1)"
direct_status=$?
set -e
if [ "$direct_status" -eq 0 ] || ! grep -Fq 'FAIL: Linux required' <<<"$direct_output"; then
  echo "FAIL: direct continuity invocation must retain the strict Linux host guard (rc=$direct_status)" >&2
  printf '%s\n' "$direct_output" >&2
  exit 1
fi

echo "PASS: generic CI excludes; direct continuity invocation remains strict"
