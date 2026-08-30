#!/usr/bin/env bash
# Bead dark-factory-w2fr — pre-push identity guard.
#
# Wraps `git push` with the same worktree / branch / repo identity check
# `af-target-identity-guard.sh` performs, then exec's the underlying
# git push command with the caller's args. Lives at the very edge of
# the worker's tool surface — every push the worker attempts flows
# through this wrapper.
#
# Why a wrapper, not just a hook
# ------------------------------
# `core.hooksPath` / `pre-push` git hooks are the obvious place for
# this check, but the factory dispatch path creates worker sessions
# from a shared checkout — the worker's git config may not have the
# factory's hooks installed, and an operator who runs `git push`
# outside the worker still gets the same drift protection if the
# factory wired `git push` to this script via a PATH shim.
#
# Usage
# -----
#   af-push-identity-guard.sh <remote> <refspec> [-- extra args...]
#
# Exit codes
# ----------
#   0  identity check passed AND the underlying `git push` succeeded.
#      (Same contract as `git push` itself — operators see a familiar
#      success/failure split.)
#   1  identity drift detected — the push is REFUSED. No git push is
#      exec'd, no remote is contacted, no commits leave the worker.
#   2  the underlying `git push` failed (e.g. non-fast-forward rejected).
#      Identity check passed, but the push itself errored. Same
#      contract as `git push` returning non-zero, so callers do not
#      have to special-case this wrapper.
#
# Identity tokens
# ---------------
# Reads AF_TARGET_CHECKOUT / AF_TARGET_BRANCH / AF_TARGET_REPO from
# the worker session's environment (injected by factory-ao-remediate.sh
# at spawn time). Refuses to run if any are missing.
set -uo pipefail

GUARD_DIR="$(cd "$(dirname "$0")" && pwd)"
IDENTITY_GUARD="$GUARD_DIR/af-target-identity-guard.sh"

if [ ! -x "$IDENTITY_GUARD" ]; then
  echo "[af-push-guard] FATAL — identity guard missing or not executable: $IDENTITY_GUARD" >&2
  exit 1
fi

# --- 0. Identity check FIRST. On drift, never exec the push. ---------------
if ! bash "$IDENTITY_GUARD"; then
  echo "[af-push-guard] REFUSING push — target-identity drift detected (see target-drift.json and stderr above)" >&2
  exit 1
fi

# --- 1. Identity OK — exec the underlying git push with the caller's args. -
if [ "$#" -lt 2 ]; then
  echo "[af-push-guard] usage: $0 <remote> <refspec> [-- extra args...]" >&2
  exit 2
fi

exec git push "$@"
