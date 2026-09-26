#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../../jobs/jleechanorg-pr-green/session-reuse.sh
source "$ROOT/jobs/jleechanorg-pr-green/session-reuse.sh"

fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
codex_home="$fixture_dir/codex"
mkdir -p "$codex_home"
printf '%s\n' '{"tokens":{}}' >"$codex_home/auth.json"
export CODEX_HOME="$codex_home"

project_config_json='{"defaultBranch":"main","sessionPrefix":"wa","env":{"OTHER":"preserve"},"worker":{"agent":"codex"}}'
set_config_json=''
verify_array=0

ao() {
  case "$1 $2" in
    "project get")
      if [[ "$verify_array" == 1 && -n "$set_config_json" ]]; then
        printf '%s\n' "{\"status\":\"ok\",\"project\":{\"id\":\"worldarchitect.ai\",\"config\":{\"defaultBranch\":\"main\",\"sessionPrefix\":\"wa\",\"env\":[\"CODEX_HOME=$CODEX_HOME\",\"OTHER=preserve\"],\"worker\":{\"agent\":\"codex\"}}}}"
      else
        printf '{"status":"ok","project":{"id":"worldarchitect.ai","config":%s}}\n' "$project_config_json"
      fi
      ;;
    "project set-config")
      [[ -n "$5" ]] || {
        echo 'FAIL: missing complete config JSON' >&2
        return 1
      }
      set_config_json="$5"
      project_config_json="{\"defaultBranch\":\"main\",\"sessionPrefix\":\"wa\",\"env\":[\"CODEX_HOME=$CODEX_HOME\",\"OTHER=preserve\"],\"worker\":{\"agent\":\"codex\"}}"
      printf '%s\n' '{"status":"ok"}'
      ;;
    *)
      echo "FAIL: unexpected ao invocation: $*" >&2
      return 1
      ;;
  esac
}

pr_green_ensure_codex_scope worldarchitect.ai
[[ -n "$set_config_json" ]] || {
  echo 'FAIL: account scope was not persisted to the AO project config' >&2
  exit 1
}
[[ "$(jq -r '.env.CODEX_HOME' <<<"$set_config_json")" == "$CODEX_HOME" ]] || {
  echo 'FAIL: persisted CODEX_HOME does not match authenticated scope' >&2
  exit 1
}
[[ "$(jq -r '.env.OTHER' <<<"$set_config_json")" == preserve ]] || {
  echo 'FAIL: set-config did not preserve unrelated project environment' >&2
  exit 1
}
[[ "$(jq -r '.worker.agent' <<<"$set_config_json")" == codex ]] || {
  echo 'FAIL: set-config did not preserve unrelated project config' >&2
  exit 1
}

verify_array=1
pr_green_ensure_codex_scope worldarchitect.ai

echo 'jleechanorg-pr-green Codex scope: PASS'
