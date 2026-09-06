#!/usr/bin/env bash
# check_auto_script_configs.sh — CI gate for the empty-by-default fail-closed
# config convention documented in docs/automation-config-convention.md.
#
# ROOT-CAUSE (bead rev-r8vb1): the fail-closed pattern shipped for
# config/auto_merge_repo_allowlist.json in PR #735 (after the 2026-08-23
# PR-merge-storm production incident) was never codified as a standing rule.
# daemon/scripts/ has a growing set of automation scripts; nothing stops the
# next auto-*.sh / *-merge-*.sh author from shipping a permissive or
# hardcoded default and reopening the same class of risk.
#
# RULE ENFORCED: every daemon/scripts/auto-*.sh or daemon/scripts/*-merge-*.sh
# script that performs an irreversible/outward-facing action (merge, push,
# delete, publish) must reference a config/*.json file, and every list-typed
# value committed in that config must default to empty ([]) — fail closed,
# not fail open. A script may be explicitly exempted via
# config/auto_script_config_check_exceptions.json when it delegates the
# actual policy decision to a caller that is already gated (documented
# per-entry; see that file for the current exemption and why).
#
# Exit code is the gate: this FAILS (non-zero) rather than warns, so it
# actually blocks CI — a warning-only check is trivially ignored. Operator
# choice, recorded here per the bead's acceptance criteria.
#
# Usage: scripts/check_auto_script_configs.sh [SCRIPTS_DIR] [REPO_ROOT]
#   SCRIPTS_DIR  defaults to REPO_ROOT/daemon/scripts
#   REPO_ROOT    defaults to `git rev-parse --show-toplevel`
set -uo pipefail

REPO_ROOT="${2:-$(git rev-parse --show-toplevel 2>/dev/null)}"
if [ -z "$REPO_ROOT" ]; then
  echo "check_auto_script_configs: cannot resolve repo root (not in a git checkout and no REPO_ROOT arg given)" >&2
  exit 2
fi
SCRIPTS_DIR="${1:-$REPO_ROOT/daemon/scripts}"
EXCEPTIONS_FILE="$REPO_ROOT/config/auto_script_config_check_exceptions.json"

if [ ! -d "$SCRIPTS_DIR" ]; then
  echo "check_auto_script_configs: scripts dir not found: $SCRIPTS_DIR" >&2
  exit 2
fi

_is_exempt() {
  local base="$1"
  [ -f "$EXCEPTIONS_FILE" ] || return 1
  python3 - "$EXCEPTIONS_FILE" "$base" <<'PYEOF'
import json, sys
cfg_path, base = sys.argv[1], sys.argv[2]
try:
    with open(cfg_path) as fh:
        cfg = json.load(fh)
    names = {e.get("script") for e in cfg.get("exempt_scripts", [])}
except Exception:
    sys.exit(1)
sys.exit(0 if base in names else 1)
PYEOF
}

# Prints OK / PARSE_ERROR:<msg> / NON_EMPTY:<keys> to stdout; exit 0 == pass.
_check_cfg_empty_default() {
  local cfg_path="$1"
  python3 - "$cfg_path" <<'PYEOF'
import json, sys
path = sys.argv[1]
try:
    with open(path) as fh:
        cfg = json.load(fh)
except Exception as e:
    print(f"PARSE_ERROR:{e}")
    sys.exit(1)
if not isinstance(cfg, dict):
    print("PARSE_ERROR:top-level JSON must be an object")
    sys.exit(1)
non_empty = [k for k, v in cfg.items() if isinstance(v, list) and len(v) > 0]
if non_empty:
    print("NON_EMPTY:" + ",".join(non_empty))
    sys.exit(1)
print("OK")
sys.exit(0)
PYEOF
}

fail=0
checked=0

while IFS= read -r -d '' f; do
  base="$(basename "$f")"
  checked=$((checked + 1))

  if _is_exempt "$base"; then
    echo "SKIP (exempt, see config/auto_script_config_check_exceptions.json): $base"
    continue
  fi

  # The allowlist path must be a literal/greppable config/*.json reference
  # (matches the existing convention in auto-merge-guard.sh), not a
  # dynamically constructed path — that's what makes this check possible
  # without executing the script.
  mapfile -t refs < <(grep -oE 'config/[A-Za-z0-9_./-]+\.json' "$f" | sort -u)

  if [ "${#refs[@]}" -eq 0 ]; then
    echo "FAIL: $base — no config/*.json allowlist reference found (see docs/automation-config-convention.md)" >&2
    fail=1
    continue
  fi

  script_ok=1
  for rel in "${refs[@]}"; do
    cfg_path="$REPO_ROOT/$rel"
    if [ ! -f "$cfg_path" ]; then
      echo "FAIL: $base — references $rel but that config file does not exist in the repo" >&2
      fail=1
      script_ok=0
      continue
    fi
    result="$(_check_cfg_empty_default "$cfg_path")"
    case "$result" in
      OK) : ;;
      PARSE_ERROR:*)
        echo "FAIL: $base — $rel: ${result#PARSE_ERROR:}" >&2
        fail=1; script_ok=0 ;;
      NON_EMPTY:*)
        echo "FAIL: $base — $rel ships a non-empty default list (${result#NON_EMPTY:}) — allowlists must default to empty (fail closed)" >&2
        fail=1; script_ok=0 ;;
      *)
        echo "FAIL: $base — could not evaluate $rel (unexpected output: $result)" >&2
        fail=1; script_ok=0 ;;
    esac
  done

  if [ "$script_ok" -eq 1 ]; then
    echo "PASS: $base -> ${refs[*]}"
  fi
done < <(find "$SCRIPTS_DIR" -maxdepth 1 -type f \( -name 'auto-*.sh' -o -name '*-merge-*.sh' \) -print0 | sort -z)

echo "check_auto_script_configs: checked $checked script(s) in $SCRIPTS_DIR"
exit $fail
