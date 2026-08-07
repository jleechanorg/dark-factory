#!/usr/bin/env bash
# g11_dispatch_request_consumer.sh — bead jleechan-vhsw (G11).
#
# Consumes the audit's side-channel `dispatch_requests.jsonl` log and
# promotes each bead back into `bead_overlay` via the existing
# `factory-overlay.sh intake-upsert` subcommand (idempotent: existing
# rows are left alone).
#
# Why a side-channel JSONL and not the daemon.jsonl telemetry: the audit
# fires every ~tick and writes only its findings; routing the audit's
# events through the full daemon.jsonl would force every consumer to
# re-parse 400+ MB of telemetry on every tick just to find the one new
# ATTESTED-no-DISPATCH row. The side-channel is bounded by the audit's
# own dedup window (24h REFILE_COOLDOWN_HOURS), so the consumer's parse
# cost stays tiny.
#
# Contract (pinned by tests/scripts/test_g11_dispatch_request_consumer.sh):
#   - Empty log: no-op, no rows touched, no error.
#   - Each entry: invoke `intake-upsert <beadId> "<title>"` against the
#     supplied overlay. The title is fixed ("G11 startup-intake
#     re-dispatch") so the overlay's intake-upsert contract is satisfied
#     without parsing the audit's payload (which has nothing the dispatch
#     loop needs anyway).
#   - Idempotent: re-running against an empty log does NOT create
#     duplicate rows.
#   - Atomic rotation: the log is rotated AFTER all entries are processed
#     so a partial-write mid-tick is recoverable on the next tick (the
#     audit re-emits the same bead id because the audit's snapshot still
#     records the same set).
#   - Failure isolation: one entry's `intake-upsert` failure MUST NOT
#     abort the rest. The failing entry stays in the rotated log so the
#     next tick can retry it; transient downstream failures do not
#     silently drop dispatch requests.
#
# Exit codes:
#   0  success (entries may have been processed or log was empty)
#   2  invalid arguments
#   9  io error (overlay db not readable)
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "g11_dispatch_request_consumer: usage: $0 <dispatch_requests.jsonl> <overlay.sh> <db_path>" >&2
  exit 2
fi

LOG="$1"
OVERLAY="$2"
DB="$3"

# Pre-flight: the side-channel log may not exist on a fresh install, on a
# tick that ran before the audit first fired, or on a tick where the
# audit found nothing to flag. That's a no-op, not an error.
if [ ! -e "$LOG" ]; then
  exit 0
fi

# Pre-flight: the overlay's `intake-upsert` writes to bead_overlay. The
# overlay's own `init` is the authoritative schema bootstrap; if the DB
# does not exist, the first intake-upsert call would fail. We don't try
# to bootstrap the schema here — that's the overlay's job — but we do
# verify that the DB file exists OR the overlay can create it on first
# write. The overlay errors out clearly if sqlite can't open it; we
# just forward that error rather than masking it with a custom message.
if [ ! -e "$DB" ] && [ ! -w "$(dirname "$DB")" ]; then
  echo "g11_dispatch_request_consumer: db parent dir not writable: $(dirname "$DB")" >&2
  exit 9
fi

# Read all entries into an in-memory list. The audit's dedup window
# (24h REFILE_COOLDOWN_HOURS) bounds the size of this log to a few
# hundred lines per tick — well within bash's argv limits even if we
# ever moved off the `while read` loop.
#
# Each line MUST be a JSON object with at least a "beadId" field.
# Malformed lines are skipped (with a warning) — they cannot be matched
# against the bead_overlay PK anyway, and silently dropping one is safer
# than wedging the consumer on a single bad byte.
TMP_NEW="$(mktemp -t g11_cons_new.XXXXXX)"
TMP_FAIL="$(mktemp -t g11_cons_fail.XXXXXX)"
trap 'rm -f "$TMP_NEW" "$TMP_FAIL"' EXIT

processed=0
while IFS= read -r line; do
  [ -z "$line" ] && continue
  bid="$(printf '%s' "$line" | python3 -c 'import json,sys
try:
    d = json.loads(sys.stdin.read())
    bid = d.get("beadId","")
    if isinstance(bid, str) and bid:
        print(bid)
except Exception:
    pass' 2>/dev/null || true)"
  if [ -z "$bid" ]; then
    echo "g11_dispatch_request_consumer: warn: skipping malformed line: $line" >&2
    echo "$line" >> "$TMP_FAIL"
    continue
  fi
  # Call intake-upsert against the overlay. Failures (rc!=0) MUST NOT
  # stop the loop — write the failing line back to $TMP_FAIL so the next
  # tick can retry it. Per the G11 contract, a transient downstream
  # failure must not silently drop dispatch requests.
  if "$OVERLAY" intake-upsert "$bid" "G11 startup-intake re-dispatch" 2>>"$TMP_FAIL.err" \
       >>"$TMP_FAIL.err"; then
    processed=$(( processed + 1 ))
  else
    rc=$?
    echo "g11_dispatch_request_consumer: warn: intake-upsert rc=$rc for $bid (will retry next tick)" >&2
    echo "$line" >> "$TMP_FAIL"
  fi
done < "$LOG"

# Rotate the log. Two cases:
#   - All entries succeeded: log is now empty, but we leave the file in
#     place so subsequent `while read` loops don't reopen overhead.
#     Truncate-to-zero (`: > "$LOG"`) preserves the inode.
#   - Some entries failed: append the failed lines back, plus the
#     stderr traces, so the operator can see what went wrong.
if [ -s "$TMP_FAIL" ]; then
  # Failed entries present — concatenate failures back onto the log
  # so the next tick retries them. stderr traces are emitted to the
  # operator-visible journal; we don't preserve them across ticks.
  cat "$TMP_FAIL" > "$LOG"
  if [ -s "$TMP_FAIL.err" ]; then
    cat "$TMP_FAIL.err" >&2
  fi
else
  : > "$LOG"
fi

echo "g11_dispatch_request_consumer: processed=$processed failures=$(wc -l < "$TMP_FAIL" 2>/dev/null | tr -d ' ')"
exit 0
