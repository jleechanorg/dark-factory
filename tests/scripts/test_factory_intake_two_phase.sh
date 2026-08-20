#!/usr/bin/env bash
# test_factory_intake_two_phase.sh — exercise the real GH intake shell script
# with fake gh/br/overlay commands.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INTAKE="$ROOT/daemon/factory-intake-from-gh.sh"
SCRATCH_DIR="$(mktemp -d -t test-factory-intake.XXXXXX)"
LOG="$SCRATCH_DIR/commands.log"
LABELS="$SCRATCH_DIR/labels"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

export PATH="$SCRATCH_DIR/bin:$PATH"
mkdir -p "$SCRATCH_DIR/bin"
export COMMAND_LOG="$LOG"
export LABELS_FILE="$LABELS"
export BR_DB="$SCRATCH_DIR/beads.db"
export CONFIG="$SCRATCH_DIR/daemon.toml"
export H="$SCRATCH_DIR/overlay.sh"
unset TARGET_REPO
cat > "$CONFIG" <<'TOML'
target_repo = "example/target"

[repos."example/other"]
ao_project = "other"
TOML

cat > "$H" <<'OVERLAY'
#!/usr/bin/env bash
printf 'overlay %s\n' "$*" >> "$COMMAND_LOG"
case "${1:-}" in
  init) ;;
  intake-upsert) printf 'created\n' ;;
esac
OVERLAY
chmod +x "$H"

cat > "$SCRATCH_DIR/bin/gh" <<'GH'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "$COMMAND_LOG"
if [ "${1:-}" = issue ] && [ "${2:-}" = list ]; then
  printf '[{"number":9153,"title":"Small PR #9153","body":"factory follow-up"}]\n'
else
  printf '{}\n'
fi
GH
chmod +x "$SCRATCH_DIR/bin/gh"

cat > "$SCRATCH_DIR/bin/br" <<'BR'
#!/usr/bin/env bash
set -euo pipefail
args=("$@")
printf 'br' >> "$COMMAND_LOG"
for arg in "${args[@]}"; do printf ' %s' "$arg" >> "$COMMAND_LOG"; done
printf '\n' >> "$COMMAND_LOG"

while [ "${args[0]:-}" = --db ]; do args=("${args[@]:2}"); done
case "${args[0]:-}" in
  list)
    printf '{"issues":[]}\n'
    ;;
  create)
    for ((i = 0; i < ${#args[@]} - 1; i++)); do
      if [ "${args[i]}" = --labels ] && [[ "${args[i + 1]}" == *factory* ]]; then
        echo "factory label was passed to create" >&2
        exit 91
      fi
    done
    printf 'bead-two-phase\n'
    : > "$LABELS_FILE"
    ;;
  show)
    labels='[]'
    if grep -qx factory "$LABELS_FILE" 2>/dev/null; then labels='["factory"]'; fi
    printf '{"id":"%s","labels":%s}\n' "${args[1]}" "$labels"
    ;;
  update)
    if [ "${args[2]:-}" = --add-label ] && [ "${args[3]:-}" = factory ]; then
      printf 'factory\n' > "$LABELS_FILE"
    fi
    printf '{"id":"%s"}\n' "${args[1]}"
    ;;
  *)
    echo "unexpected br command: ${args[*]}" >&2
    exit 92
    ;;
esac
BR
chmod +x "$SCRATCH_DIR/bin/br"

output="$($INTAKE)"
printf '%s\n' "$output"

mapfile -t br_lines < <(grep '^br ' "$LOG")
[ "${#br_lines[@]}" -eq 6 ] || {
  echo "expected six br calls, got ${#br_lines[@]}" >&2
  cat "$LOG" >&2
  exit 1
}

printf 'verified br command log:\n'
printf '%s\n' "${br_lines[@]}"

case "${br_lines[2]}" in
  *' create '* ) ;;
  *) echo "expected create as third br call" >&2; exit 1 ;;
esac
case "${br_lines[2]}" in
  *' --labels '*factory* ) echo "factory label appeared on create" >&2; exit 1 ;;
esac
case "${br_lines[3]}" in
  *' show bead-two-phase --json' ) ;;
  *) echo "expected show immediately after create" >&2; exit 1 ;;
esac
case "${br_lines[4]}" in
  *' update bead-two-phase --add-label factory --json' ) ;;
  *) echo "expected factory label update after first show" >&2; exit 1 ;;
esac
case "${br_lines[5]}" in
  *' show bead-two-phase --json' ) ;;
  *) echo "expected final show after factory label update" >&2; exit 1 ;;
esac

intake_line="$(grep -n '^overlay intake-upsert ' "$LOG" | cut -d: -f1)"
final_show_line="$(grep -n ' show bead-two-phase --json' "$LOG" | tail -1 | cut -d: -f1)"
[ "$intake_line" -gt "$final_show_line" ] || {
  echo "intake-upsert ran before final label verification" >&2
  cat "$LOG" >&2
  exit 1
}

echo "PASS: real intake performs unlabelled create -> show -> label -> show -> intake-upsert"

: > "$LOG"
rm -f "$LABELS_FILE"
export TARGET_REPO="example/other"
other_output="$($INTAKE)"
case "$other_output" in
  *'"target_repo": "example/other"'*) ;;
  *) echo "configured [repos] target was not selected" >&2; exit 1 ;;
esac

: > "$LOG"
export TARGET_REPO="example/unconfigured"
if "$INTAKE" >"$SCRATCH_DIR/unconfigured.out" 2>"$SCRATCH_DIR/unconfigured.err"; then
  echo "unconfigured TARGET_REPO unexpectedly succeeded" >&2
  exit 1
fi
if grep -q '^br ' "$LOG"; then
  echo "unconfigured TARGET_REPO reached Bead mutation" >&2
  exit 1
fi

echo "PASS: configured [repos] target is accepted and unconfigured target fails before Bead access"
