#!/usr/bin/env bash
# Pre-push guard: a coder that bypasses `br` and edits .beads/issues.jsonl
# directly violates the br-only tracking contract. This guard rejects such
# edits on factory/* branches so the only legitimate way to land a JSONL
# change is via `br sync --flush-only` on the daemon side (or via a main-branch
# hygiene flush by a human operator).
#
# Exceptions (in priority order):
#   1. The push is the first push of a new branch — `origin/<branch>` does
#      not exist yet, so we have no baseline to diff against. Allow.
#   2. The local ref is NOT a factory/* branch — maintainer main-branch flushes
#      and ad-hoc ops branches are explicitly permitted.
#   3. The push is invoked with `git push --no-verify` — bypass is documented
#      (see PR #308 / bead jleechan-cnf9).
#
# Mirrors the LOGIC shape of the misrouted worldai PR #8426 guard, but lives
# in this repo's own .githooks/ chain (dark-factory uses githooks, not Husky).
# Bypass: `git push --no-verify`.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

bad=0

# Read the pre-push stdin once. Format: <local_ref> <local_sha> <remote_ref> <remote_sha>
# on each line, one ref per line, terminated by EOF.
input_lines=()
while IFS= read -r input_lines_line || [[ -n "$input_lines_line" ]]; do
    input_lines+=("$input_lines_line")
done

for line in "${input_lines[@]:-}"; do
    [[ -z "$line" ]] && continue

    set -- $line
    local_ref="$1"
    local_sha="$2"

    # Pre-push contract — every line has exactly 4 whitespace-separated fields.
    if [[ -z "${local_ref:-}" || -z "${local_sha:-}" ]]; then
        continue
    fi

    branch="${local_ref#refs/heads/}"

    # Branch policy: factory/* pushes are gated; everything else (main,
    # fix/*, feat/*, docs/*, etc.) is allowed to flush JSONL freely.
    case "$branch" in
        factory/*) ;;
        *) continue ;;
    esac

    # Determine the upstream tip for this branch so we can detect "did anything
    # actually change relative to origin/HEAD?". `git rev-parse --verify` exits
    # non-zero when the ref does not exist on the remote tracking side — that
    # is the first-push-no-remote-tip case and we permit it.
    upstream=""
    if upstream="$(git rev-parse --verify "origin/${branch}" 2>/dev/null)"; then
        :
    else
        printf 'beads guard: first push of branch %q (no origin/%s tip) — allowing.\n' \
            "$branch" "$branch" >&2
        continue
    fi

    # Diff line-by-line so we can test just the JSONL path even when the
    # branch has a large mixed commit surface.
    jsonl_touched=0
    while IFS= read -r path; do
        [[ -z "$path" ]] && continue
        if [[ "$path" == ".beads/issues.jsonl" ]]; then
            jsonl_touched=1
            break
        fi
    done < <(git diff --name-only "${upstream}..${local_sha}" -- 2>/dev/null || true)

    if [[ "$jsonl_touched" -eq 1 ]]; then
        printf '%s\n' \
            "beads guard: refusing to push .beads/issues.jsonl from factory branch ${branch}." \
            "  .beads/issues.jsonl must be edited only via \`br\` (br create, br update," \
            "  br sync --flush-only) and never with a direct file edit or sed/awk." \
            "  Bypass: \`git push --no-verify\` (documented for emergency JSONL" \
            "  repairs only — open a follow-up bead to fix the underlying br" \
            "  failure if this becomes routine)." >&2
        bad=1
    fi
done

exit "$bad"
