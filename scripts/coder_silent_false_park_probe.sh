#!/usr/bin/env bash
# coder_silent_false_park_probe.sh
#
# jleechan-coder-silent-false-parks-h92r — cross-repo regression probe.
#
# 2026-07-17: all 6 af-drive lanes (df-149..154) parked PARKED_HUMAN_HELD
# reason=coder_silent while their coders were ACTIVELY WORKING. The silence
# heuristic in jleechanorg/dark-factory's daemon (`tick.rs` Dispatched
# autonomy check, fixed in merged commit ce0e72a7 on `main`) only polled
# the REMOTE branch tip via `gh api .../branches/<branch>`. Long-running
# Claude coder sessions buffer locally between `git push` cycles (5-15 min
# cadence, sometimes much longer for non-trivial diffs), so the remote can
# appear stale for >30 min while the coder's own Claude transcript
# directory under ~/.claude/projects/<slug>/ is actively growing.
#
# The actual algorithmic fix (merged into dark-factory `main` as
# ce0e72a7 — supersedes the original
# `fix/jleechan-coder-silent-false-parks-h92r` branch which has since been
# deleted post-merge) consults Claude transcript MTIME (file mtime in the
# coder's own `~/.claude/projects/<slug>/` transcript dir) as a second,
# independent liveness signal. A coder is treated as LIVE if EITHER the
# remote branch tip OR the transcript directory shows fresh activity; only
# a coder with NEITHER signal having moved in 30 minutes is parked.
#
# This script runs the two new regression tests from ce0e72a7 against a
# dark-factory worktree so any worldarchitect.ai operator can verify the
# fix is live without re-reading the dark-factory diff.
#
# Usage:
#   ./scripts/coder_silent_false_park_probe.sh
#
# Exit codes:
#   0 — both new regression tests appear AND pass (fix is live)
#   1 — one or more new tests missing or failing (fix is missing/broken)
#   2 — prerequisite missing (rust toolchain or dark-factory checkout,
#       or checkout does not contain merged ce0e72a7)
#
# The probe is intentionally a SHELL wrapper, not a Python/JS check —
# it must work on a clean install with only `bash`, `git`, `cargo`, and a
# jleechanorg/dark-factory worktree mounted at $DARK_FACTORY_HOME.
set -euo pipefail

DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/.worktrees/dark-factory/jleechan-x8tf-r1}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "FAIL: cargo not on PATH — install rust toolchain first" >&2
    exit 2
fi

# Verify merged commit ancestry rather than checking for an ephemeral
# branch. ce0e72a7 is the merge commit on `main` for the transcript-mtime
# fix; the original feature branch `fix/jleechan-coder-silent-false-parks-h92r`
# was deleted post-merge, so a `git branch --show-current` style guard
# would produce a false NEGATIVE on a healthy repo. Instead, require the
# merge-commit SHA to be reachable from the dark-factory HEAD (i.e. the
# dark-factory checkout contains the fix).
EXPECTED_DF_COMMIT="ce0e72a7650d5c43e6021602b291f162ec8cec81"

if [[ ! -d "$DARK_FACTORY_HOME/daemon" ]]; then
    echo "FAIL: dark-factory checkout not found at $DARK_FACTORY_HOME/daemon" >&2
    echo "       clone jleechanorg/dark-factory and ensure ${EXPECTED_DF_COMMIT} is reachable" >&2
    exit 2
fi

if ! git -C "$DARK_FACTORY_HOME" merge-base --is-ancestor "$EXPECTED_DF_COMMIT" HEAD; then
    ACTUAL_DF_HEAD="$(git -C "$DARK_FACTORY_HOME" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    echo "FAIL: dark-factory HEAD ($ACTUAL_DF_HEAD) does not contain required merge commit ${EXPECTED_DF_COMMIT}" >&2
    echo "       fetch origin and rebase onto/main, or set DARK_FACTORY_HOME to a checkout that does" >&2
    exit 2
fi

# The two regression tests introduced by ce0e72a7 — operators must see
# BOTH names appear in the cargo output AND both must show PASS for the
# probe to return success. A bare substring match on "coder_silent" is
# NOT sufficient: the pre-existing `test_wedge_detection_dispatched_coder_silent`
# in the same file also matches that substring, so a stale checkout
# missing the new tests could still exit 0 by passing only the original
# test. This list-assert is what makes the probe a true regression gate.
REQUIRED_TESTS=(
    "test_wedge_detection_dispatched_coder_silent_saved_by_transcript_activity"
    "test_wedge_detection_dispatched_coder_silent_stale_transcript_still_parks"
)

echo "==> Running dark-factory coder_silent regression suite"
echo "    dark-factory checkout: $DARK_FACTORY_HOME"
echo "    required merge commit: $EXPECTED_DF_COMMIT (transcript-mtime fix)"
echo "    required tests: ${REQUIRED_TESTS[*]}"
echo

LOG_FILE="${CODER_SILENT_PROBE_LOG:-/tmp/coder_silent_probe.log}"
cd "$DARK_FACTORY_HOME/daemon"
CARGO_EXIT=0
cargo test --test tick_integration coder_silent 2>&1 | tee "$LOG_FILE" | tail -25 || CARGO_EXIT=$?

if [[ "$CARGO_EXIT" -ne 0 ]]; then
    echo
    echo "FAIL: regression suite returned non-zero ($CARGO_EXIT) — silence-watcher is misparking active coders" >&2
    echo "      full log at $LOG_FILE" >&2
    exit 1
fi

# Cargo's substring filter `coder_silent` matches the pre-existing
# `test_wedge_detection_dispatched_coder_silent` even on a stale checkout,
# so we MUST additionally assert both new test names appear in the log
# AND pass for the probe to return success. This is the regression gate
# requested in the PR #8421 codex blocker comment.
MISSING=()
for t in "${REQUIRED_TESTS[@]}"; do
    # `cargo test` prints one summary line per test like:
    #   test test_wedge_detection_... ... ok
    # or  test test_wedge_detection_... ... FAILED
    if ! grep -Eq "^test ${t} +\.\.\. (ok|ignored)" "$LOG_FILE"; then
        MISSING+=("$t")
    fi
done

if [[ ${#MISSING[@]} -gt 0 ]]; then
    echo
    echo "FAIL: required regression tests not found (or did not pass) in log:" >&2
    for t in "${MISSING[@]}"; do
        echo "      - $t" >&2
    done
    echo "      full log at $LOG_FILE" >&2
    exit 1
fi

echo
echo "PASS: silence-watcher now consults Claude transcript MTIME in addition to remote branch tip"
echo "      - ${REQUIRED_TESTS[0]} (fresh transcript mtime → NOT parked)"
echo "      - ${REQUIRED_TESTS[1]} (stale transcript mtime → still parked, fail-closed preserved)"
exit 0
