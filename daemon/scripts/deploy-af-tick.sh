#!/usr/bin/env bash
# deploy-af-tick.sh — explicit, audited deploy step for the ai.dark-factory.af-tick
# launchd daemon's execution checkout.
#
# Bead jleechan-vxs8: the launchd daemon executes whatever branch happens to
# be checked out in its execution root (default ~/projects/dark-factory, a
# dev working tree also used interactively). Before this bead, "deploying" a
# change to the daemon meant `git checkout <branch>` in that tree — an
# unaudited, undocumented action indistinguishable from a dev poking around.
# On 2026-07-11 the tree sat on a crashing feature branch for hours, then was
# silently switched to a different branch by another session; neither state
# was a deliberate deploy.
#
# This script is the ONE sanctioned way to move the daemon's checkout
# forward. It:
#   1. Refuses to run if the deploy target has uncommitted changes (dirty).
#   2. Refuses to run if the deploy target is not on `main`.
#   3. Fetches origin, fast-forwards main to origin/main.
#   4. Logs the SHA transition (old -> new) to stdout AND to a JSONL audit
#      log under ~/Library/Logs/dark-factory/deploy.jsonl so every deploy is
#      forensically reconstructible.
#   5. Is a no-op (rc=0, logs "no-op") when old == new — repeated runs are
#      safe and cheap (idempotent), matching install-launchagents.sh style.
#
# This does NOT restart the launchd daemon (single-writer rule — restarts
# are the deploy-owner's job, not this script's; see daemon/launchd/README.md
# and CLAUDE.md "single-writer" policy). It only fast-forwards the checkout;
# the running af-tick.sh process picks up the new code on its next
# invocation (each tick execs a fresh script read from disk).
#
# Usage:
#   daemon/scripts/deploy-af-tick.sh [--target-dir <path>] [--dry-run]
#
# Environment:
#   AFD_DEPLOY_TARGET_DIR   overrides the default deploy target
#                           (default: $HOME/projects/dark-factory)
#   AFD_DEPLOY_LOG          overrides the JSONL audit log path
#                           (default: $HOME/Library/Logs/dark-factory/deploy.jsonl)
#
# Exit codes:
#   0  success (deployed or already up to date)
#   2  invalid argument
#   3  target directory missing / not a git repo
#   4  target directory is dirty (uncommitted changes) — refusing
#   5  target directory is not on main — refusing
#   6  fetch or fast-forward failed
#
# Acceptance bead jleechan-goal-unattended-e2e-2026-07-17-bze8.2 — extension:
# the canonical deploy record must include every binary/script/config SHA
# the production daemon depends on, so a forensic replay can reconstruct
# exactly what code was live at deploy time. The standard channel records
# only old_sha -> new_sha on the checkout itself; this extension records
# the SHAs of every file under daemon/{scripts,launchd,contracts}/ plus
# the AO CLI binary (if AO_BIN is set). The extended record lives at
# $HOME/Library/Logs/dark-factory/deploy-bze8.jsonl and is appended on
# every successful deploy (including no-ops). Pass --no-extra-shas to
# suppress this for callers who want the legacy shape only.
set -euo pipefail

# Resolve this script's own checkout (the dark-factory repo root) so
# write_deploy_bze8_record can read artifact SHAs even when $TARGET_DIR is
# a different bare clone. Walk two levels up from the script path:
# .../daemon/scripts/deploy-af-tick.sh -> $REPO
SCRIPT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"

TARGET_DIR="${AFD_DEPLOY_TARGET_DIR:-$HOME/projects/dark-factory}"
DEPLOY_LOG="${AFD_DEPLOY_LOG:-$HOME/Library/Logs/dark-factory/deploy.jsonl}"
# Extended (full-SHA) deploy record. Lives at
# $HOME/Library/Logs/dark-factory/deploy-bze8.jsonl by default; can be
# disabled with --no-extra-shas. Honor AFD_DEPLOY_BZE8_LOG override too.
DEPLOY_BZE8_LOG="${AFD_DEPLOY_BZE8_LOG:-$HOME/Library/Logs/dark-factory/deploy-bze8.jsonl}"
DRY_RUN=0
EXTRA_SHAS=1

i=1
while [ "$i" -le "$#" ]; do
    arg="${@:$i:1}"
    case "$arg" in
        --target-dir)
            i=$((i + 1))
            if [ "$i" -gt "$#" ]; then
                echo "deploy-af-tick: --target-dir requires a value" >&2
                exit 2
            fi
            TARGET_DIR="${@:$i:1}"
            ;;
        --dry-run) DRY_RUN=1 ;;
        --no-extra-shas) EXTRA_SHAS=0 ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--target-dir <path>] [--dry-run] [--no-extra-shas]

Fast-forwards the dark-factory daemon's execution checkout (default:
\$HOME/projects/dark-factory) to origin/main, refusing if the checkout is
dirty or not on main. Logs the SHA transition to stdout and to
$DEPLOY_LOG. When --no-extra-shas is NOT set, also appends a JSONL row
to $DEPLOY_BZE8_LOG with HEAD SHA + the SHAs of every daemon/scripts/*,
daemon/launchd/*, daemon/contracts/* file plus the AO CLI binary
(if AO_BIN is set). Does NOT restart the daemon.
EOF
            exit 0
            ;;
        *) echo "deploy-af-tick: unknown argument: $arg" >&2; exit 2 ;;
    esac
    i=$((i + 1))
done

if [ ! -d "$TARGET_DIR/.git" ]; then
    echo "deploy-af-tick: $TARGET_DIR is not a git repo (missing .git) — refusing" >&2
    exit 3
fi

# ----------------------------------------------------------------------------
# write_deploy_bze8_record <old_sha> <new_sha> <noop_bool>
#
# Append a JSONL row to $DEPLOY_BZE8_LOG capturing every binary/script/config
# SHA the production daemon depends on. This is the canonical "what was
# deployed" record for bead jleechan-goal-unattended-e2e-2026-07-17-bze8.2.
#
# The row shape is a single line of JSON with these keys:
#   ts             ISO-8601 UTC timestamp
#   target_dir     the deploy target directory (operator's repo checkout)
#   repo_old_sha   HEAD before the deploy (== new_sha iff noop)
#   repo_new_sha   HEAD after the deploy
#   noop           true if no advance happened, false if a real deploy
#   head_sha       same as repo_new_sha (kept for grep convenience)
#   ao_bin         absolute path to AO CLI binary (may be empty)
#   ao_bin_sha256  sha256 of $AO_BIN (may be empty)
#   artifacts      array of {path, sha256} objects, all paths relative to the
#                  repo root
#
# Implemented as a separate function (rather than inlined) so the test script
# tests/scripts/test_deploy_af_tick_extra_shas.sh can call it directly with
# a scratch target dir without needing to perform a real git fetch.
# Defined BEFORE main flow so the call sites in the noop / advance branches
# below can resolve it.
# ----------------------------------------------------------------------------
write_deploy_bze8_record() {
    local old="$1" new="$2" noop="$3"
    mkdir -p "$(dirname "$DEPLOY_BZE8_LOG")"
    local ts; ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    # Resolve which dark-factory checkout to read artifact SHAs from.
    # Resolution order (each step explicit so the operator can see exactly
    # what's happening):
    #   1. AFD_DEPLOY_BZE8_REPO_ROOT  — operator-provided override
    #   2. SCRIPT_DIR                 — the dir this script lives in
    #                                       (the dark-factory checkout the
    #                                        deploy script was synced from;
    #                                        canonical source of artifact
    #                                        SHAs even when the target dir
    #                                        is a different bare clone)
    local repo_root="${AFD_DEPLOY_BZE8_REPO_ROOT:-$SCRIPT_DIR}"
    if [ ! -f "$repo_root/daemon/factory-af-tick.sh" ]; then
        # Last-ditch fallback: derive from $TARGET_DIR if it IS a dark-factory
        # checkout (operator deploys from a dev tree they use as target).
        repo_root="$TARGET_DIR"
    fi
    local ao_bin_path="${AO_BIN:-}"
    local ao_bin_sha=""
    if [ -n "$ao_bin_path" ] && [ -x "$ao_bin_path" ]; then
        ao_bin_sha="$(sha256sum "$ao_bin_path" | cut -d' ' -f1)"
    fi
    python3 - "$ts" "$TARGET_DIR" "$old" "$new" "$noop" "$repo_root" "$ao_bin_path" "$ao_bin_sha" "$DEPLOY_BZE8_LOG" <<'PYBZE8'
import json, os, sys, hashlib, datetime
ts, target_dir, old_sha, new_sha, noop, repo_root, ao_bin, ao_bin_sha, log_path = sys.argv[1:10]
def sha256_file(p):
    try:
        with open(p, "rb") as fh:
            return hashlib.sha256(fh.read()).hexdigest()
    except Exception:
        return ""
artifacts = []
candidates = [
    "daemon/factory-af-tick.sh",
    "daemon/factory-overlay.sh",
    "daemon/factory-ao-bin.sh",
    "daemon/factory-ao-remediate.sh",
    "daemon/factory-tick.sh",
    "daemon/scripts/deploy-af-tick.sh",
    "daemon/scripts/auto-merge-guard.sh",
    "daemon/scripts/safe-push-main.sh",
    "daemon/scripts/libnotify-slack.sh",
    "daemon/launchd/launchd-wrapper.sh",
    "daemon/launchd/ai.dark-factory.af-tick.plist.template",
    "daemon/launchd/ai.dark-factory.github-webhook.plist.template",
    "daemon/launchd/ai.dark-factory.status-cron.plist.template",
    "daemon/contracts/schema.sql",
    "daemon/contracts/daemon.toml.example",
    "config/daemon.toml",
]
for rel in candidates:
    fp = os.path.join(repo_root, rel)
    if os.path.isfile(fp):
        artifacts.append({"path": rel, "sha256": sha256_file(fp)})
row = {
    "ts": ts,
    "target_dir": target_dir,
    "repo_old_sha": old_sha,
    "repo_new_sha": new_sha,
    "noop": noop.lower() == "true",
    "head_sha": new_sha,
    "ao_bin": ao_bin,
    "ao_bin_sha256": ao_bin_sha,
    "artifacts": artifacts,
}
with open(log_path, "a") as fh:
    fh.write(json.dumps(row, sort_keys=True) + "\n")
PYBZE8
}

cd "$TARGET_DIR"

current_branch="$(git branch --show-current 2>/dev/null || true)"
if [ "$current_branch" != "main" ]; then
    echo "deploy-af-tick: $TARGET_DIR is on branch '${current_branch:-<detached HEAD>}', not main — refusing to deploy. Switch to main manually first (this script will not do it for you, to avoid silently discarding whatever a human/session put there deliberately)." >&2
    exit 5
fi

if ! git diff --quiet HEAD -- 2>/dev/null || ! git diff --quiet --cached HEAD -- 2>/dev/null; then
    echo "deploy-af-tick: $TARGET_DIR has uncommitted changes — refusing to deploy dirty source. Run 'git status' in $TARGET_DIR and clean the tree first." >&2
    exit 4
fi

old_sha="$(git rev-parse HEAD)"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] would: git fetch origin main && git merge --ff-only origin/main"
    echo "[dry-run] current HEAD: ${old_sha:0:9}"
    exit 0
fi

if ! git fetch origin main --quiet; then
    echo "deploy-af-tick: git fetch origin main failed" >&2
    exit 6
fi

if ! git merge --ff-only origin/main --quiet 2>/dev/null; then
    echo "deploy-af-tick: fast-forward to origin/main failed (local main has diverged commits not on origin/main — this should not happen on a deploy-only checkout; investigate manually, do not force)" >&2
    exit 6
fi

new_sha="$(git rev-parse HEAD)"

mkdir -p "$(dirname "$DEPLOY_LOG")"
deploy_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [ "$old_sha" = "$new_sha" ]; then
    echo "deploy-af-tick: no-op — $TARGET_DIR already at origin/main (${new_sha:0:9})"
    printf '{"ts":"%s","target_dir":"%s","old_sha":"%s","new_sha":"%s","noop":true}\n' \
        "$deploy_ts" "$TARGET_DIR" "$old_sha" "$new_sha" >> "$DEPLOY_LOG"
    if [ "$EXTRA_SHAS" -eq 1 ]; then
        write_deploy_bze8_record "$old_sha" "$new_sha" "true" || true
    fi
    exit 0
fi

echo "deploy-af-tick: DEPLOYED $TARGET_DIR: ${old_sha:0:9} -> ${new_sha:0:9}"
printf '{"ts":"%s","target_dir":"%s","old_sha":"%s","new_sha":"%s","noop":false}\n' \
    "$deploy_ts" "$TARGET_DIR" "$old_sha" "$new_sha" >> "$DEPLOY_LOG"
if [ "$EXTRA_SHAS" -eq 1 ]; then
    write_deploy_bze8_record "$old_sha" "$new_sha" "false" || true
fi

echo
echo "NOTE: the running ai.dark-factory.af-tick launchd job will pick up this"
echo "code on its NEXT tick (each tick execs factory-af-tick.sh fresh from"
echo "disk) — no restart required. If you need to force an immediate pickup,"
echo "that is a separate, deliberate action outside this script's scope"
echo "(single-writer rule: only the designated deploy-owner restarts the"
echo "live launchd job, and never as a side effect of a deploy script)."
