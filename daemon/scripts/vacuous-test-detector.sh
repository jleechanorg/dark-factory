#!/usr/bin/env bash
# vacuous-test-detector.sh — CLI wrapper around `daemon::vacuous`.
#
# Detects vacuous test patterns in newly-added/modified Rust test files of
# the PR diff. PR #387 / bead jleechan-ijod / issue #387. Exits:
#   0  — no vacuous findings (clean)
#   1  — at least one finding (gate fail; telemetry: VACUOUS_TEST_DETECTION)
#   2  — internal error (rare; treated as infra)
#
# Usage:
#   vacuous-test-detector.sh [--base <ref>] [--files <path>...]
#                            [--json <out-file>] [--quiet]
#
# When `--base <ref>` is supplied, the wrapper derives the diff via
# `git diff --name-only <ref>...HEAD` and scans every modified/new
# `*.rs` file in the resulting list. When `--files <path>...` is
# supplied, only those paths are scanned (operators can pin the scan).
# When neither is supplied, the wrapper scans the daemon's own
# `daemon/tests/fixtures/vacuous_test_detector/` (smoke-test mode).
#
# --json <out-file> writes the ScanReport as JSON to the given path; the
# auto-merge-guard and other factory tooling consume this file.
#
# The wrapper invokes the Rust library via `cargo run` against a small CLI
# front-end (see `daemon/src/bin/vacuous_test_detector.rs` for the entry
# point).

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
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "vacuous-test-detector: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

# Decide which paths (REPO_ROOT-relative) to scan.
SCAN_LIST=()
if [ -n "$BASE_REF" ]; then
    while IFS= read -r f; do
        case "$f" in
            *.rs) SCAN_LIST+=("$f") ;;
            *)    : ;;
        esac
    done < <(git diff --name-only "$BASE_REF"...HEAD 2>/dev/null || true)
fi
if [ "${#FILES[@]}" -gt 0 ]; then
    SCAN_LIST=("${FILES[@]}")
fi
if [ "${#SCAN_LIST[@]}" -eq 0 ]; then
    # Smoke-test mode: scan the bundled fixtures directory. Paths are
    # relative to REPO_ROOT.
    SCAN_LIST=("daemon/tests/fixtures/vacuous_test_detector/vacuous_examples"
               "daemon/tests/fixtures/vacuous_test_detector/clean_examples")
fi

# Resolve each SCAN_LIST entry to a path that exists relative to REPO_ROOT,
# and emit a daemon/-relative version when applicable. Empty or missing
# paths are dropped quietly.
INVOKE=()
INVOKE+=("--paths")
for p in "${SCAN_LIST[@]}"; do
    if [ -z "$p" ] || [ ! -e "$p" ]; then
        continue
    fi
    case "$p" in
        daemon/*) INVOKE+=("${p#daemon/}") ;;
        *)        INVOKE+=("$p") ;;
    esac
done
if [ "${#INVOKE[@]}" -le 1 ]; then
    [ "$QUIET" = "0" ] && echo "vacuous-test-detector: no scannable paths" >&2
    exit 1
fi
if [ -n "$JSON_OUT" ]; then
    INVOKE+=("--json" "$JSON_OUT")
fi

# Build the CLI binary (cached after first invocation).
( cd daemon && cargo build --quiet --bin vacuous_test_detector )

# Run the detector binary; capture the exit code.
RC=0
( cd daemon && cargo run --quiet --bin vacuous_test_detector -- "${INVOKE[@]}" ) || RC=$?

[ "$QUIET" = "0" ] && [ "$RC" -eq 1 ] && echo "vacuous-test-detector: findings present (gate fail)" >&2
[ "$QUIET" = "0" ] && [ "$RC" -eq 0 ] && echo "vacuous-test-detector: clean (no findings)" >&2
exit $RC
