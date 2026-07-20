#!/usr/bin/env bash
# vacuous-red-green.sh — CLI wrapper around `daemon::vacuous_red_green`.
#
# Runtime complement to vacuous-test-detector.sh: this wrapper captures
# the diff between --base and HEAD, reverts the production files via
# `git apply -R`, runs the new/changed tests against the reverted tree,
# and reports vacuity based on whether ANY test fails on the reverted
# code. Issue #387 / bead jleechan-ijod. Exits:
#
#   0 — at least one new/changed test FAILS on the reverted tree
#       (genuine red-green; gate passes)
#   1 — every new/changed test PASSES on the reverted tree (vacuous;
#       gate fails; telemetry: VACUOUS_TEST_RED_GREEN)
#   2 — internal error (diff capture, git apply, etc.)
#
# Usage:
#   vacuous-red-green.sh --base <ref> [--files <P>...] [--json <out>] [--quiet]
#
# When `--base <ref>` is supplied, the wrapper derives the file list via
# `git diff --name-only <ref>...HEAD`. When `--files <P>...` is supplied,
# only those paths are scanned. `--json <out>` writes the RedGreenReport
# to <out> for downstream consumers (auto-merge-guard, etc.).

set -uo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

BASE_REF=""
FILES=()
JSON_OUT=""
QUIET="0"
while [ $# -gt 0 ]; do
    case "$1" in
        --base)
            BASE_REF="${2:-}"
            shift 2
            ;;
        --files)
            shift
            while [ $# -gt 0 ] && [[ "${1:-}" != --* ]]; do
                FILES+=("$1")
                shift
            done
            ;;
        --json)
            JSON_OUT="${2:-}"
            shift 2
            ;;
        --quiet)
            QUIET="1"
            shift
            ;;
        -h|--help)
            sed -n '2,28p' "$0"
            exit 0
            ;;
        *)
            echo "vacuous-red-green: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [ -z "$BASE_REF" ]; then
    echo "vacuous-red-green: --base <ref> is required" >&2
    exit 2
fi

INVOKE=("--base" "$BASE_REF")
if [ "${#FILES[@]}" -gt 0 ]; then
    INVOKE+=("--files")
    for f in "${FILES[@]}"; do
        INVOKE+=("$f")
    done
fi
if [ -n "$JSON_OUT" ]; then
    INVOKE+=("--json" "$JSON_OUT")
fi

# Build the CLI binary (cached after first invocation).
( cd daemon && cargo build --quiet --bin vacuous_red_green )

# Run the detector; capture the exit code.
RC=0
( cd daemon && cargo run --quiet --bin vacuous_red_green -- "${INVOKE[@]}" ) || RC=$?

[ "$QUIET" = "0" ] && [ "$RC" -eq 0 ] && echo "vacuous-red-green: GENUINE red-green (gate passes)" >&2
[ "$QUIET" = "0" ] && [ "$RC" -eq 1 ] && echo "vacuous-red-green: VACUOUS (gate fail)" >&2
[ "$QUIET" = "0" ] && [ "$RC" -eq 2 ] && echo "vacuous-red-green: ERROR" >&2
exit $RC