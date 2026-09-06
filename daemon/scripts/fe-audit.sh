#!/usr/bin/env bash
# fe-audit.sh — /factory-evolve daemon telemetry audit (Phase 2.5).
#
# Reads ~/Library/Logs/dark-factory/daemon.jsonl (or the path in
# $FE_AUDIT_LOG), runs four numeric G10–G13 checks against the last
# $LOOKBACK_HOURS hours of telemetry, and files a `factory`-labeled bead
# per finding. The bead body MUST start with `target_repo:` so the
# auto-factory daemon picks it up.
#
# Exit codes:
#   0  no findings (or all within threshold)
#   2  invalid arguments
#   9  io error (read/parse failure on daemon.jsonl)
#   10 jq / br not installed
#   11 audit state file lock contention
#
# This script is the feeder for /af: it does NOT call `/af` directly.
# The always-on `ai.dark-factory.daemon.service` reads the filed beads via
# its next factory-af-tick.sh pass and dispatches AO workers.
#
# Why numeric thresholds: classifying free-form error messages would be a
# ZFC violation (keyword intent routing). The four checks here use ONLY
# numeric thresholds (counts, gaps, rates) — never "looks like a stall".

set -euo pipefail

# ---------- config ----------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
QUERY_PY="$SCRIPT_DIR/fe_audit_query.py"

LOG_FILE="${FE_AUDIT_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
STATE_DIR="${FE_AUDIT_STATE_DIR:-$HOME/.dark-factory/fe-audit}"
STATE_FILE="$STATE_DIR/last-fired.json"
LOOKBACK_HOURS="${LOOKBACK_HOURS:-24}"
MAX_TICK_GAP_SEC="${MAX_TICK_GAP_SEC:-1440}"             # G10: 24 min at 4-min cadence is fine; 24h is a stall
ATTESTED_GRACE_HOURS="${ATTESTED_GRACE_HOURS:-1}"         # G11: don't fire in the first hour after ATTESTED
TRANSIENT_BLEED_THRESHOLD="${TRANSIENT_BLEED_THRESHOLD:-5}"  # G12: same bead, same hour, 5+ transient errors
DISPATCH_RATE_PER_HOUR_CAP="${DISPATCH_RATE_PER_HOUR_CAP:-30}"  # G13: >30 dispatches/hour is a cap violation
REFILE_COOLDOWN_HOURS="${REFILE_COOLDOWN_HOURS:-24}"      # dedupe: don't refile same G-code within 24h
BR_DB="${BR_DB:-$REPO_ROOT/.beads/beads.db}"

# ---------- arg parsing ----------
DRY_RUN=0
NO_BEAD=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --no-bead) NO_BEAD=1; shift ;;
        --log) LOG_FILE="$2"; shift 2 ;;
        --lookback) LOOKBACK_HOURS="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,18p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown arg: $1" >&2
            exit 2
            ;;
    esac
done

# ---------- preflight ----------
for bin in python3 date; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "fe-audit: required binary missing: $bin" >&2
        exit 10
    fi
done

if [ ! -r "$QUERY_PY" ]; then
    echo "fe-audit: query helper missing: $QUERY_PY" >&2
    exit 10
fi

if [ ! -r "$LOG_FILE" ]; then
    echo "fe-audit: log not readable: $LOG_FILE" >&2
    exit 9
fi

mkdir -p "$STATE_DIR"

NOW_EPOCH="$(date -u +%s)"
CUTOFF_EPOCH=$(( NOW_EPOCH - LOOKBACK_HOURS * 3600 ))
CUTOFF_ISO="$(date -u -d "@$CUTOFF_EPOCH" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u +%Y-%m-%dT%H:%M:%SZ)"

log() { echo "[fe-audit $(date -u +%H:%M:%SZ)] $*"; }
log "log=$LOG_FILE lookback=${LOOKBACK_HOURS}h cutoff=$CUTOFF_ISO"

# ---------- load prior-fired state (cooldown dedupe) ----------
PRIOR_G10=0; PRIOR_G11=0; PRIOR_G12=0; PRIOR_G13=0
if [ -f "$STATE_FILE" ]; then
    PRIOR_G10="$(jq -r '.last_fired_g10 // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
    PRIOR_G11="$(jq -r '.last_fired_g11 // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
    PRIOR_G12="$(jq -r '.last_fired_g12 // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
    PRIOR_G13="$(jq -r '.last_fired_g13 // 0' "$STATE_FILE" 2>/dev/null || echo 0)"
fi

findings_total=0

file_bead() {
    local g_code="$1" title="$2" body="$3"
    if [ "$NO_BEAD" -eq 1 ]; then
        log "[$g_code] (no-bead) would file: $title"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        log "[$g_code] (dry-run) would file: $title"
        return 0
    fi
    if ! command -v br >/dev/null 2>&1; then
        log "[$g_code] br not on PATH; skipping bead file for: $title"
        return 0
    fi
    BR_DB="$BR_DB" br create "$title" \
        --type task \
        --priority 2 \
        --labels factory,factory-evolve,daemon-health \
        --body "$body" || {
            log "[$g_code] br create failed (rc=$?); continuing"
            return 0
        }
    findings_total=$(( findings_total + 1 ))
}

# ---------- G10: tick liveness ----------
# Use TICK event timestamps only; never parse error strings.
G10_OUT="$(python3 "$QUERY_PY" g10_ticks "$LOG_FILE" "$CUTOFF_ISO" 2>/dev/null)" || G10_OUT=""
G10_OUT="${G10_OUT:-}"

if [ -n "$G10_OUT" ]; then
    LAST_TICK_EPOCH="$(date -u -d "$G10_OUT" +%s 2>/dev/null || echo 0)"
    if [ "$LAST_TICK_EPOCH" -gt 0 ]; then
        GAP=$(( NOW_EPOCH - LAST_TICK_EPOCH ))
        log "G10: last tick $(date -u -d "@$LAST_TICK_EPOCH" +%H:%M:%SZ) gap=${GAP}s (threshold=${MAX_TICK_GAP_SEC}s)"
        if [ "$GAP" -gt "$MAX_TICK_GAP_SEC" ] && [ $(( NOW_EPOCH - PRIOR_G10 )) -gt $(( REFILE_COOLDOWN_HOURS * 3600 )) ]; then
            file_bead "G10" "G10 no-tick-liveness-watchdog: daemon idle ${GAP}s (threshold ${MAX_TICK_GAP_SEC}s)" \
"target_repo: jleechanorg/dark-factory
event_type: G10 no-tick-liveness-watchdog
threshold: ${MAX_TICK_GAP_SEC}s
observed_gap_seconds: ${GAP}
last_tick_iso: ${G10_OUT##*$'\n'}
lookback_hours: ${LOOKBACK_HOURS}
log_source: ${LOG_FILE}
evidence_kind: numeric-timestamp-only
remediation_hint: Add a heartbeat watchdog that compares now() against last_tick_ts and exits non-zero on gap > N * AFD_TICK_INTERVAL_SEC. Restart=on-failure in the unit brings the loop back. See /factory-evolve G10 for context.
linked_beads: jleechan-508
"
        fi
    fi
fi

# ---------- G11: attested-not-dispatched ----------
# Numeric: count ATTESTED bead IDs over the lookback, then count DISPATCHED
# for the SAME set; if ATTESTED > 0 AND DISPATCHED == 0, file.
ATTESTED_IDS="$(python3 "$QUERY_PY" g11_attested "$LOG_FILE" "$CUTOFF_ISO" 2>/dev/null)" || ATTESTED_IDS=""
ATTESTED_IDS="${ATTESTED_IDS:-}"

DISPATCHED_IDS="$(python3 "$QUERY_PY" g11_dispatched "$LOG_FILE" "$CUTOFF_ISO" 2>/dev/null)" || DISPATCHED_IDS=""
DISPATCHED_IDS="${DISPATCHED_IDS:-}"

# Subtract beads that have legitimately escalated to HUMAN_HELD — those
# are parked for operator action (branch-conflict recovery limit, external
# blocker, etc.) per the dispatch-health triage flow, NOT stuck. Without
# this subtraction, every legitimate HOLD fires a phantom factory-labeled
# bead that /af dutifully re-dispatches, reproducing the 2026-08-01
# phantom-dispatch cluster (74wt/lwte/z284/bze8.4).
HUMAN_HELD_IDS="$(python3 "$QUERY_PY" g11_human_held "$LOG_FILE" "$CUTOFF_ISO" 2>/dev/null)" || HUMAN_HELD_IDS=""
HUMAN_HELD_IDS="${HUMAN_HELD_IDS:-}"

# Subtract beads in CANCELLED state (branch-collision dedup) — those are
# closed/superseded, NOT stuck. Without this subtraction, a flood of
# CANCELLED beads from dedup surfaces as a false stuck-bead surge.
CANCELLED_IDS="$(python3 "$QUERY_PY" g11_cancelled "$LOG_FILE" "$CUTOFF_ISO" 2>/dev/null)" || CANCELLED_IDS=""
CANCELLED_IDS="${CANCELLED_IDS:-}"

# Find beads in ATTESTED that have NO dispatch follow-up AND have NOT
# legitimately escalated to HUMAN_HELD or CANCELLED. Sort explicitly because comm
# requires lexically sorted input.
STUCK_BEADS="$(comm -23 <(printf '%s\n' "$ATTESTED_IDS" | sort -u) <(printf '%s\n' "$DISPATCHED_IDS" | sort -u) | comm -23 - <(printf '%s\n' "$HUMAN_HELD_IDS" | sort -u) | comm -23 - <(printf '%s\n' "$CANCELLED_IDS" | sort -u) || true)"
STUCK_COUNT=0
if [ -n "$STUCK_BEADS" ]; then
    STUCK_COUNT="$(echo "$STUCK_BEADS" | wc -l | tr -d ' ')"
fi

log "G11: attested=${STUCK_COUNT} (no DISPATCHED follow-up over ${LOOKBACK_HOURS}h)"
if [ "$STUCK_COUNT" -gt 0 ] && [ $(( NOW_EPOCH - PRIOR_G11 )) -gt $(( REFILE_COOLDOWN_HOURS * 3600 )) ]; then
    # Cap body to first 30 beads so we don't blow past the 4096-char spawn cap.
    SAMPLE="$(echo "$STUCK_BEADS" | head -30)"
    file_bead "G11" "G11 startup-intake-without-forced-dispatch: ${STUCK_COUNT} ATTESTED beads without DISPATCH follow-up" \
"target_repo: jleechanorg/dark-factory
event_type: G11 startup-intake-without-forced-dispatch
threshold: 1 (any attested bead without dispatch follow-up)
observed_attested_no_dispatch_count: ${STUCK_COUNT}
lookback_hours: ${LOOKBACK_HOURS}
attested_beads_sample:
${SAMPLE}
remediation_hint: Intake sweep must enqueue a DISPATCH_REQUEST event whenever STATE=ATTESTED rows accumulate beyond the previous tick's snapshot. Without that, restart cycles leave beads stuck in ATTESTED with no worker spawn. See /factory-evolve G11 for context.
linked_beads: jleechan-509
"
fi

# ---------- G12: retry-backoff bleed ----------
# Numeric: count BEAD_*_TRANSIENT_ERROR per bead in the last hour; if >=
# threshold, file. (No string-matching — just per-bead event counts.)
TRANSIENT_PER_BEAD="$(python3 "$QUERY_PY" g12_transient "$LOG_FILE" "$CUTOFF_ISO" "$TRANSIENT_BLEED_THRESHOLD" 2>/dev/null)" || TRANSIENT_PER_BEAD=""
TRANSIENT_PER_BEAD="${TRANSIENT_PER_BEAD:-}"

if [ -n "$TRANSIENT_PER_BEAD" ]; then
    HOT_BEAD_COUNT="$(echo "$TRANSIENT_PER_BEAD" | wc -l | tr -d ' ')"
    HOT_BEAD_SAMPLE="$(echo "$TRANSIENT_PER_BEAD" | head -10 | awk '{printf "- %s (%s transient errors)\n", $2, $1}')"
    log "G12: ${HOT_BEAD_COUNT} beads hit >=${TRANSIENT_BLEED_THRESHOLD} transient errors in ${LOOKBACK_HOURS}h"
    if [ $(( NOW_EPOCH - PRIOR_G12 )) -gt $(( REFILE_COOLDOWN_HOURS * 3600 )) ]; then
        file_bead "G12" "G12 retry-backoff-bleed-into-global-suppression: ${HOT_BEAD_COUNT} beads with >=${TRANSIENT_BLEED_THRESHOLD} transient errors" \
"target_repo: jleechanorg/dark-factory
event_type: G12 retry-backoff-bleed-into-global-suppression
threshold: ${TRANSIENT_BLEED_THRESHOLD} transient errors per bead in ${LOOKBACK_HOURS}h
hot_bead_count: ${HOT_BEAD_COUNT}
hot_bead_top10:
${HOT_BEAD_SAMPLE}
remediation_hint: Per-bead retry backoff must be isolated to that bead's queue slot. Global tick suppression is a separate, bounded value. Today the two are conflated and one bad bead blocks the whole queue. See /factory-evolve G12 for context.
linked_beads: jleechan-510
"
    fi
fi

# ---------- G13: dispatch rate cap ----------
# Numeric: count TASK_DISPATCHED per hour over the lookback; flag any hour
# window where count > DISPATCH_RATE_PER_HOUR_CAP.
HIGH_RATE_HOURS="$(python3 "$QUERY_PY" g13_dispatch_rate "$LOG_FILE" "$CUTOFF_ISO" "$DISPATCH_RATE_PER_HOUR_CAP" 2>/dev/null)" || HIGH_RATE_HOURS=""
HIGH_RATE_HOURS="${HIGH_RATE_HOURS:-}"

if [ -n "$HIGH_RATE_HOURS" ]; then
    HIGH_HOUR_COUNT="$(echo "$HIGH_RATE_HOURS" | wc -l | tr -d ' ')"
    log "G13: ${HIGH_HOUR_COUNT} hour-windows exceeded ${DISPATCH_RATE_PER_HOUR_CAP}/h dispatch rate"
    if [ $(( NOW_EPOCH - PRIOR_G13 )) -gt $(( REFILE_COOLDOWN_HOURS * 3600 )) ]; then
        file_bead "G13" "G13 missing-dispatch-rate-cap: ${HIGH_HOUR_COUNT} hour-windows above ${DISPATCH_RATE_PER_HOUR_CAP}/h" \
"target_repo: jleechanorg/dark-factory
event_type: G13 missing-dispatch-rate-cap
threshold: ${DISPATCH_RATE_PER_HOUR_CAP} dispatches per hour
high_rate_hour_count: ${HIGH_HOUR_COUNT}
high_rate_hours:
${HIGH_RATE_HOURS}
remediation_hint: MAX_DISPATCH env var must be bounded: default 2, hard-cap min(MAX_DISPATCH, AO_MAX_CONCURRENT_SESSIONS/2). Without the cap, one runaway window can saturate AO session quota. See /factory-evolve G13 for context.
"
    fi
fi

# ---------- persist cooldown state ----------
if [ "$DRY_RUN" -eq 0 ] && [ "$NO_BEAD" -eq 0 ]; then
    cat > "$STATE_FILE" <<EOF
{
  "last_run_iso": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "last_fired_g10": $([ "$(echo "$G10_OUT" | wc -l)" -gt 0 ] && [ "$GAP" -gt "$MAX_TICK_GAP_SEC" ] && echo "$NOW_EPOCH" || echo "$PRIOR_G10"),
  "last_fired_g11": $([ "$STUCK_COUNT" -gt 0 ] && echo "$NOW_EPOCH" || echo "$PRIOR_G11"),
  "last_fired_g12": $([ -n "$TRANSIENT_PER_BEAD" ] && echo "$NOW_EPOCH" || echo "$PRIOR_G12"),
  "last_fired_g13": $([ -n "$HIGH_RATE_HOURS" ] && echo "$NOW_EPOCH" || echo "$PRIOR_G13"),
  "findings_filed": $findings_total
}
EOF
fi

log "done. findings_filed=$findings_total"
exit 0