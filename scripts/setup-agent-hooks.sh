#!/usr/bin/env bash
# Install per-CLI hook configs that wire Codex, Cursor, Gemini, and OpenCode
# sessions into the shared merge_train conflict check at
# ~/.local/bin/conflict-warn-pre-tool.sh.
#
# Background:
#   Each agent CLI uses its own settings schema, so a single config file
#   can't cover all four. This script generates:
#       .codex/hooks.json     (Codex: PreToolUse)
#       .cursor/hooks.json    (Cursor: preToolUse)
#       .gemini/settings.json (Gemini: BeforeTool)
#       .opencode.json        (OpenCode: instructions — JSON config has no hooks)
#
#   Codex, Cursor, and Gemini funnel into the same hook script. OpenCode's JSON
#   config has no hook API, so it gets an instruction reminder instead. Edit
#   HOOK_PATH below (or pass --hook-path) if your script lives elsewhere.
#
# Idempotent: re-running overwrites with the latest template. The four
# generated files are per-machine (hardcoded home path, per-CLI schema) and
# MUST be gitignored — see the .gitignore entries this repo ships with.
#
# Usage:
#   setup-agent-hooks.sh                 # install all four
#   setup-agent-hooks.sh --only codex,cursor
#   setup-agent-hooks.sh --hook-path /custom/path/conflict-warn-pre-tool.sh
#   setup-agent-hooks.sh --dry-run       # show what would be written, no I/O
#   setup-agent-hooks.sh --check         # show current state, exit 0/1
#   setup-agent-hooks.sh --uninstall     # remove the four generated files
#   setup-agent-hooks.sh --force         # install even if HOOK_PATH is missing
#   setup-agent-hooks.sh --help

set -euo pipefail

# ---------- config ----------

SCRIPT_NAME="$(basename "$0")"
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"

# CLI registry: name -> relative output path
declare -A CLI_PATHS=(
  [codex]="$REPO_ROOT/.codex/hooks.json"
  [cursor]="$REPO_ROOT/.cursor/hooks.json"
  [gemini]="$REPO_ROOT/.gemini/settings.json"
  [opencode]="$REPO_ROOT/.opencode.json"
)

HOOK_PATH="${MERGE_TRAIN_HOOK:-$HOME/.local/bin/conflict-warn-pre-tool.sh}"
ACTION="install"      # install | check | uninstall
DRY_RUN=0             # independent flag; applies to install + uninstall
ONLY=""               # empty = all CLIs
FORCE=0

# ---------- arg parse ----------

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --help|-h) usage ;;
    --dry-run) DRY_RUN=1; shift ;;
    --check)   ACTION="check"; shift ;;
    --uninstall) ACTION="uninstall"; shift ;;
    --force)   FORCE=1; shift ;;
    --hook-path)
      [ $# -ge 2 ] || { echo "$SCRIPT_NAME: --hook-path requires a value" >&2; exit 2; }
      HOOK_PATH="$2"; shift 2 ;;
    --only)
      [ $# -ge 2 ] || { echo "$SCRIPT_NAME: --only requires a value (codex,cursor,gemini,opencode)" >&2; exit 2; }
      ONLY="$2"; shift 2 ;;
    --only=*)
      ONLY="${1#--only=}"
      [ -n "$ONLY" ] || { echo "$SCRIPT_NAME: --only requires a non-empty value" >&2; exit 2; }
      shift ;;
    *) echo "$SCRIPT_NAME: unknown flag: $1" >&2; exit 2 ;;
  esac
done

# Resolve --only into a list. Empty = all.
if [ -n "$ONLY" ]; then
  REQUESTED=()
  IFS=',' read -ra REQUESTED <<< "$ONLY"
  # Validate every requested name, and rebuild the registry subset.
  VALIDATED=()
  for name in "${REQUESTED[@]}"; do
    name="$(echo "$name" | tr -d ' ')"
    if [ -z "${CLI_PATHS[$name]+_}" ]; then
      echo "$SCRIPT_NAME: unknown CLI '$name' (valid: ${!CLI_PATHS[*]})" >&2
      exit 2
    fi
    VALIDATED+=("$name")
  done
  # Rebuild CLI_PATHS keeping only requested names.
  for key in "${!CLI_PATHS[@]}"; do
    keep=0
    for v in "${VALIDATED[@]}"; do [ "$key" = "$v" ] && keep=1; done
    [ "$keep" = "1" ] || unset "CLI_PATHS[$key]"
  done
fi

# Refuse to run outside a git repo (these configs are repo-cwd-relative).
if [ -z "$REPO_ROOT" ]; then
  echo "$SCRIPT_NAME: not inside a git repository (no toplevel)" >&2
  exit 2
fi

# ---------- template generators ----------
# Each emits the file body to stdout.

json_string() {
  python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$1"
}

hook_command_json() {
  local quoted
  printf -v quoted '%q' "$HOOK_PATH"
  json_string "bash $quoted"
}

instruction_json() {
  json_string "IMPORTANT: Before starting any coding task, run: predict-conflicts --from-prs <PR_NUMBER> to check for symbol-level conflicts with other open PRs. If conflicts are detected, pick a different task or coordinate with the conflicting PR's author."
}

gen_codex() {
  local command_json
  command_json="$(hook_command_json)"
  cat <<EOF
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": $command_json,
            "timeoutSec": 15,
            "statusMessage": "merge_train: checking conflicts..."
          }
        ]
      }
    ]
  }
}
EOF
}

gen_cursor() {
  local command_json
  command_json="$(hook_command_json)"
  cat <<EOF
{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "command": $command_json
      }
    ]
  }
}
EOF
}

gen_gemini() {
  local command_json
  command_json="$(hook_command_json)"
  cat <<EOF
{
  "hooks": {
    "BeforeTool": [
      {
        "matcher": ".*",
        "hooks": [
          {
            "type": "command",
            "command": $command_json,
            "timeout": 15000
          }
        ]
      }
    ]
  }
}
EOF
}

gen_opencode() {
  local instructions_json
  instructions_json="$(instruction_json)"
  cat <<EOF
{
  "\$schema": "https://opencode.ai/config.json",
  "instructions": $instructions_json
}
EOF
}

# Map CLI name -> generator fn (declared so ${!GENERATORS[*]} resolves).
declare -A GENERATORS=(
  [codex]=gen_codex
  [cursor]=gen_cursor
  [gemini]=gen_gemini
  [opencode]=gen_opencode
)

# Sentinel marker used by --check to recognize "ours" vs user-customized.
# Each hook-based CLI uses a different event name, so we look for THAT —
# not for the script path, which is identical across codex/cursor/opencode.
sentinel_for() {
  case "$1" in
    codex)    echo '"PreToolUse"' ;;
    cursor)   echo '"preToolUse"' ;;
    gemini)   echo '"BeforeTool"' ;;
    opencode) echo "predict-conflicts --from-prs" ;;
    *) echo "$SCRIPT_NAME: unknown CLI '$1'" >&2; return 2 ;;
  esac
}

jq_check_expr_for() {
  case "$1" in
    codex) echo '.hooks.PreToolUse | type == "array"' ;;
    cursor) echo '.version == 1 and (.hooks.preToolUse | type == "array")' ;;
    gemini) echo '(.hooks.BeforeTool | type == "array") and (.hooks.BeforeTool[0].hooks[0].timeout == 15000)' ;;
    opencode) echo '.["\u0024schema"] == "https://opencode.ai/config.json" and (.instructions | contains("predict-conflicts --from-prs")) and (.hooks? == null)' ;;
    *) echo "$SCRIPT_NAME: unknown CLI '$1'" >&2; return 2 ;;
  esac
}

# ---------- preflight ----------

check_hook_script() {
  if [ -x "$HOOK_PATH" ]; then
    return 0
  fi
  echo "$SCRIPT_NAME: warning: hook script not found or not executable at $HOOK_PATH" >&2
  if [ "$FORCE" != "1" ] && [ "$ACTION" = "install" ]; then
    echo "  Re-run with --force to install anyway, or fix the path with --hook-path." >&2
    return 1
  fi
  return 0
}

# ---------- mode dispatch ----------

run_install() {
  check_hook_script || exit 1
  local rc=0
  for name in "${!CLI_PATHS[@]}"; do
    out="${CLI_PATHS[$name]}"
    new_body="$("${GENERATORS[$name]}")"
    existing=""
    [ -f "$out" ] && existing="$(cat "$out")"

    if [ "$DRY_RUN" = "1" ]; then
      if [ "$existing" = "$new_body" ]; then
        echo "[dry-run] $name: $out (unchanged)"
      elif [ -n "$existing" ]; then
        echo "[dry-run] $name: $out (would modify)"
      else
        echo "[dry-run] $name: $out (would create)"
      fi
      continue
    fi

    mkdir -p "$(dirname "$out")"
    if [ "$existing" = "$new_body" ]; then
      echo "[ok]      $name: $out (unchanged)"
    elif [ -n "$existing" ]; then
      printf '%s\n' "$new_body" > "$out"
      echo "[ok]      $name: $out (modified)"
    else
      printf '%s\n' "$new_body" > "$out"
      echo "[ok]      $name: $out (created)"
    fi
  done
  return $rc
}

run_check() {
  if ! command -v jq >/dev/null 2>&1; then
    echo "$SCRIPT_NAME: jq is required for --check" >&2
    return 1
  fi
  local rc=0
  for name in "${!CLI_PATHS[@]}"; do
    out="${CLI_PATHS[$name]}"
    sentinel="$(sentinel_for "$name")"
    jq_expr="$(jq_check_expr_for "$name")"
    if [ ! -f "$out" ]; then
      echo "[missing] $name: $out"
      rc=1
      continue
    fi
    if ! jq -e . "$out" >/dev/null 2>&1; then
      echo "[invalid] $name: $out (not valid JSON)"
      rc=1
      continue
    fi
    if grep -qF "$sentinel" "$out"; then
      if jq -e "$jq_expr" "$out" >/dev/null 2>&1; then
        echo "[ok]      $name: $out"
      else
        echo "[wrong]   $name: $out (valid JSON but wrong schema)"
        rc=1
      fi
    else
      echo "[custom]  $name: $out (exists but does not reference $sentinel)"
      rc=1
    fi
  done
  return $rc
}

run_uninstall() {
  for name in "${!CLI_PATHS[@]}"; do
    out="${CLI_PATHS[$name]}"
    if [ ! -e "$out" ]; then
      echo "[skip]    $name: $out (not present)"
      continue
    fi
    if [ "$DRY_RUN" = "1" ]; then
      echo "[dry-run] $name: would remove $out"
      continue
    fi
    rm -f "$out"
    # Remove the parent dir if empty (e.g. .codex/) — only if our config
    # was the only thing in it.
    parent="$(dirname "$out")"
    if [ "$parent" != "$REPO_ROOT" ] && [ -d "$parent" ] && [ -z "$(ls -A "$parent" 2>/dev/null)" ]; then
      rmdir "$parent" 2>/dev/null || true
    fi
    echo "[ok]      $name: removed $out"
  done
}

# ---------- main ----------

case "$ACTION" in
  install)   run_install ;;
  check)     run_check || exit 1 ;;
  uninstall) run_uninstall ;;
esac
