#!/usr/bin/env bash
# Pre-push guard: reject factory-branch pushes that directly modify
# .beads/issues.jsonl.
#
# df-160 (2026-07-17) bypassed a `br` Duplicate-external_ref error by hand-
# editing /home/jleechan/projects/dark-factory/.beads/issues.jsonl. Direct
# edits silently diverge from the SQLite DB and corrupt the next br flush.
#
# This guard enforces the br-only tracking contract on factory/* branches:
# for each ref about to push, if the local ref matches factory/* AND
# origin/<ref> exists, diff <upstream>..<local_sha> for .beads/issues.jsonl
# and reject with exit 1 if touched. First push (no remote tip), non-factory
# branches, and main-branch JSONL flushes are explicitly allowed.
#
# Bypass for emergencies: `git push --no-verify` (mirrors sibling guards).
#
# Reads pre-push stdin: <local_ref> <local_sha> <remote_ref> <remote_sha> per
# line. We loop, not `read` once, so multi-ref batched pushes are handled.

set -euo pipefail

# Run from the caller's cwd — git pre-push always invokes hooks in the
# pushing repo, so stdin SHAs and `git diff` queries resolve against the
# correct object database. Do NOT cd to a different toplevel.

rejected=0

while IFS= read -r line; do
    # Skip blank lines and comments.
    [[ -z "$line" ]] && continue
    [[ "$line" == \#* ]] && continue

    # Parse the four fields pre-push hands us. Older git (2.20-) emits three.
    set -- $line
    local_ref="${1:-}"
    local_sha="${2:-}"
    remote_ref="${3:-}"
    remote_sha="${4:-}"

    [[ -z "$local_ref" ]] && continue

    # We only police factory/* branches — that's where df-160 happened and
    # where coder agents are dispatched.
    case "$local_ref" in
        refs/heads/factory/*)
            branch="${local_ref#refs/heads/}"
            upstream_sha="$remote_sha"

            # First push has no remote tip — let it through.
            if ! git rev-parse --verify --quiet "$upstream_sha^{commit}" >/dev/null 2>&1; then
                continue
            fi

            # Local tip is what we'd push; use it as the "new" side.
            if git diff --name-only "$upstream_sha" "$local_sha" -- '.beads/issues.jsonl' | grep -q '^'; then
                printf 'beads guard: refusing to push .beads/issues.jsonl from factory branch %s.\n' "$branch" >&2
                printf '  .beads/issues.jsonl must be edited only via `br` (br create, br update,\n' >&2
                printf '  br sync --flush-only) and never with a direct file edit or sed/awk.\n' >&2
                printf '  Bypass: `git push --no-verify` (documented for emergency JSONL\n' >&2
                printf '  repairs only — open a follow-up bead to fix the underlying br\n' >&2
                printf '  failure if this becomes routine).\n' >&2
                rejected=1
            fi
            ;;
    esac
done

exit "$rejected"