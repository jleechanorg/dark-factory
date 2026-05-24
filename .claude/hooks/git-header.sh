#!/usr/bin/env bash
# Git header generator script (ENHANCED VERSION WITH GIT STATUS + CONTEXT %)
# Usage: ./git-header.sh or git header (if aliased)
# Works from any directory within a git repository or worktree

# Parse context percentage from Claude Code JSON input (if available)
# Cache is scoped per repo+branch: /tmp/claude_ctx/<repo>/<branch>
_ctx_repo=$(basename "$(git rev-parse --show-toplevel 2>/dev/null)" 2>/dev/null)
_ctx_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null | tr '/' '_')
_ctx_cache="/tmp/claude_ctx/${_ctx_repo}/${_ctx_branch}"

CONTEXT_PCT=""
if [ -t 0 ]; then
    # stdin is a terminal — read cached value written by Stop hook
    [ -r "$_ctx_cache" ] && CONTEXT_PCT=$(cat "$_ctx_cache" 2>/dev/null)
else
    # stdin has data (or is an empty pipe) — try to parse JSON
    json_input=$(cat)
    if command -v jq >/dev/null 2>&1 && [ -n "$json_input" ]; then
        CONTEXT_PCT=$(printf '%s' "$json_input" | jq -r '.context_window.used_percentage // empty' 2>/dev/null)
        # Cache for manual invocations that lack stdin
        if [ -n "$CONTEXT_PCT" ]; then
            mkdir -p "/tmp/claude_ctx/${_ctx_repo}"
            printf '%s' "$CONTEXT_PCT" > "$_ctx_cache"
        fi
    fi
    # Fallback: stdin was empty pipe (manual Bash tool invocation) — use cache
    [ -z "$CONTEXT_PCT" ] && [ -r "$_ctx_cache" ] && CONTEXT_PCT=$(cat "$_ctx_cache" 2>/dev/null)
fi

# Source cross-platform timeout utilities
SCRIPT_DIR="$(dirname "${BASH_SOURCE[0]}")"
if [ -r "$SCRIPT_DIR/timeout-utils.sh" ]; then
    # shellcheck source=/dev/null
    source "$SCRIPT_DIR/timeout-utils.sh"
else
    gh_with_timeout() {
        local _timeout="$1"
        shift
        gh "$@"
    }
fi

# Find the git directory (works in worktrees and submodules)
git_dir=$(git rev-parse --git-dir 2>/dev/null)
if [ $? -ne 0 ]; then
    echo "[Not in a git repository]"
    exit 0
fi

# Get the common git directory (for worktrees, refs are stored in the shared directory)
# In a normal repo, this equals git_dir. In a worktree, it points to the main repo's .git
git_common_dir=$(git rev-parse --git-common-dir 2>/dev/null || echo "$git_dir")

# Get the root of the working tree
git_root=$(git rev-parse --show-toplevel 2>/dev/null)
if [ $? -ne 0 ]; then
    echo "[Unable to find git root]"
    exit 0
fi

# Find the git root and change to it
script_dir="$git_root"
cd "$git_root" || { echo "[Unable to change to git root]"; exit 0; }

# Get working directory for context
working_dir="$(basename "$git_root")"

local_branch=$(git branch --show-current)
remote=$(git rev-parse --abbrev-ref --symbolic-full-name '@{u}' 2>/dev/null || echo "no upstream")

# Parse a remote URL into owner/repo format (standalone function for reuse)
parse_repo_from_url() {
    local parsed_url="$1"

    # Match HTTP/HTTPS GitHub format (supports any domain, including enterprise): https://[domain]/owner/repo.git
    if [[ "$parsed_url" =~ https?://([^@/]+@)?([^/]+)/([^/]+)/([^/]+)(\.git)?/?$ ]]; then
        local domain="${BASH_REMATCH[2]}"
        local owner="${BASH_REMATCH[3]}"
        local repo="${BASH_REMATCH[4]}"
        repo="${repo%.git}"
        echo "${domain}/${owner}/${repo}"
        return 0
    fi

    # Match SSH format (supports any domain, including enterprise): git@[domain]:owner/repo.git
    if [[ "$parsed_url" =~ git@([^:]+):([^/]+)/([^/]+)(\.git)?/?$ ]]; then
        local domain="${BASH_REMATCH[1]}"
        local owner="${BASH_REMATCH[2]}"
        local repo="${BASH_REMATCH[3]}"
        repo="${repo%.git}"
        echo "${domain}/${owner}/${repo}"
        return 0
    fi

    # Match ssh:// protocol format: ssh://[user@]host[:port]/owner/repo.git
    if [[ "$parsed_url" =~ ssh://([^@/]+@)?([^/:]+)(:[0-9]+)?/([^/]+)/([^/]+)(\.git)?/?$ ]]; then
        local domain="${BASH_REMATCH[2]}"
        local owner="${BASH_REMATCH[4]}"
        local repo="${BASH_REMATCH[5]}"
        repo="${repo%.git}"
        echo "${domain}/${owner}/${repo}"
        return 0
    fi

    # Match local proxy format: http://local_proxy@127.0.0.1:PORT/git/owner/repo
    if [[ "$parsed_url" =~ /git/([^/]+)/([^/]+)(\.git)?/?$ ]]; then
        local owner="${BASH_REMATCH[1]}"
        local repo="${BASH_REMATCH[2]}"
        repo="${repo%.git}"
        echo "${owner}/${repo}"
        return 0
    fi

    return 1
}

# Extract repository (owner/repo) from git remote
# Prefers upstream remote (for fork workflows) over origin
get_repo_from_remote() {
    local url

    # In fork workflows, upstream points to the main repo where PRs exist
    # Prefer upstream; only fall back to origin when it's the sole remote
    if url=$(git remote get-url upstream 2>/dev/null); then
        parse_repo_from_url "$url" && return 0
    fi

    # If there's only one remote, it's safe to use it as the gh repo target
    local remote_count
    remote_count=$(git remote 2>/dev/null | wc -l | tr -d '[:space:]')
    if [ "$remote_count" = "1" ]; then
        local remote_name
        remote_name=$(git remote 2>/dev/null | head -n 1)
        if [ -n "$remote_name" ] && url=$(git remote get-url "$remote_name" 2>/dev/null); then
            parse_repo_from_url "$url" && return 0
        fi
    fi

    return 1
}

# Get the repository for gh commands
repo_name=$(get_repo_from_remote)

# Get sync status and unpushed changes
local_status=""

# Check for uncommitted changes (always check, regardless of remote)
# Check both modified tracked files AND untracked files
# Handle fresh repos without HEAD gracefully
if git rev-parse --verify HEAD >/dev/null 2>&1; then
    # Normal repo with commits: check diff-index and untracked files
    if ! git diff-index --quiet HEAD -- 2>/dev/null || [ -n "$(git ls-files --others --exclude-standard 2>/dev/null)" ]; then
        uncommitted=" +uncommitted"
    else
        uncommitted=""
    fi
else
    # Fresh repo without commits: only check for any files
    if [ -n "$(git ls-files --others --exclude-standard 2>/dev/null)" ]; then
        uncommitted=" +uncommitted"
    else
        uncommitted=""
    fi
fi

if [ "$remote" != "no upstream" ]; then
    # Count commits ahead and behind
    ahead_count=$(git rev-list --count "$remote"..HEAD 2>/dev/null || echo "0")
    behind_count=$(git rev-list --count HEAD.."$remote" 2>/dev/null || echo "0")

    if [ "$ahead_count" -eq 0 ] && [ "$behind_count" -eq 0 ]; then
        local_status=" (synced$uncommitted)"
    elif [ "$ahead_count" -gt 0 ] && [ "$behind_count" -eq 0 ]; then
        local_status=" (ahead $ahead_count$uncommitted)"
    elif [ "$ahead_count" -eq 0 ] && [ "$behind_count" -gt 0 ]; then
        local_status=" (behind $behind_count$uncommitted)"
    else
        local_status=" (diverged +$ahead_count -$behind_count$uncommitted)"
    fi
else
    local_status=" (no remote$uncommitted)"
fi

# Smart PR detection with fast-first fallback strategy
# 1. Branch name patterns (instant)
# 2. Cached network lookup (fast)
# 3. Real-time network lookup with timeout (fallback)

# Fast detection from branch naming patterns
pr_text="none"
# Skip PR inference for AO-managed session/worktree branches (no PR context)
if [[ "$local_branch" =~ ^(session|ao|jc|wa|cc|ra|wc)- ]]; then
    # AO-managed branch — skip regex inference, rely on commit-based lookup below
    pr_text="none"
elif [[ "$local_branch" =~ pr-([0-9]+) ]]; then
    pr_text="#${BASH_REMATCH[1]} (inferred)"
elif [[ "$local_branch" =~ /pr-([0-9]+) ]]; then
    pr_text="#${BASH_REMATCH[1]} (inferred)"
else
    # Enhanced PR cache with smart invalidation
    # - "none" results cached for CACHE_TTL_NONE seconds (PRs may be created soon after push)
    # - Real PR numbers cached for CACHE_TTL_PR seconds (stable)
    # - Bypass cache entirely if last push was < RECENT_PUSH_WINDOW seconds ago
    readonly CACHE_TTL_NONE=30      # 30 seconds for "none" results
    readonly CACHE_TTL_PR=300       # 5 minutes for PR numbers
    readonly RECENT_PUSH_WINDOW=60  # 60 seconds for recent push detection
    current_commit=$(git rev-parse HEAD 2>/dev/null)
    cache_file="/tmp/git-header-pr-${current_commit:0:8}"
    cache_valid=false

    # Helper function to get file mtime (POSIX-compatible)
    get_mtime() {
        local file="$1"
        if command -v perl >/dev/null 2>&1; then
            perl -e 'print((stat($ARGV[0]))[9])' "$file" 2>/dev/null || echo "0"
        else
            stat -f%m "$file" 2>/dev/null || stat -c%Y "$file" 2>/dev/null || echo "0"
        fi
    }

    current_time=$(date +%s)

    # Check if there was a recent push (within RECENT_PUSH_WINDOW seconds)
    # Detect by checking mtime of remote tracking ref (updated on push)
    recent_push=false
    if [ "$remote" != "no upstream" ]; then
        # Parse remote name and branch from upstream ref (e.g., "origin/main" or "upstream/feature")
        # Handle any remote name, not just "origin"
        remote_name="${remote%%/*}"
        remote_branch="${remote#*/}"
        ref_file="$git_common_dir/refs/remotes/$remote_name/$remote_branch"

        # Check unpacked ref file first
        if [ -f "$ref_file" ]; then
            remote_ref_mtime=$(get_mtime "$ref_file")
            # Explicitly check for mtime extraction failure (returns "0" or empty)
            # "0" means epoch time which indicates extraction failed
            if [ -n "$remote_ref_mtime" ] && [ "$remote_ref_mtime" != "0" ]; then
                push_age=$((current_time - remote_ref_mtime))
                if [ "$push_age" -lt "$RECENT_PUSH_WINDOW" ]; then
                    recent_push=true
                fi
            fi
        fi

        # Fallback: check packed-refs if unpacked ref not found or mtime extraction failed
        # Modern git may pack refs into .git/packed-refs without updating individual file mtimes
        if [ "$recent_push" = false ]; then
            packed_refs_file="$git_common_dir/packed-refs"
            if [ -f "$packed_refs_file" ]; then
                packed_refs_mtime=$(get_mtime "$packed_refs_file")
                if [ -n "$packed_refs_mtime" ] && [ "$packed_refs_mtime" != "0" ]; then
                    packed_push_age=$((current_time - packed_refs_mtime))
                    if [ "$packed_push_age" -lt "$RECENT_PUSH_WINDOW" ]; then
                        recent_push=true
                    fi
                fi
            fi
        fi
    fi

    # Check if cache exists and is valid
    # Skip cache check entirely if recent push detected (user likely just created PR)
    if [ -f "$cache_file" ] && [ -n "$current_commit" ] && [ "$recent_push" = false ]; then
        cache_mtime=$(get_mtime "$cache_file")
        cache_age=$((current_time - cache_mtime))
        cached_value=$(cat "$cache_file" 2>/dev/null || echo "none")

        # Different TTL based on cached value:
        # - "none" = CACHE_TTL_NONE seconds (PRs may be created soon)
        # - Real PR numbers = CACHE_TTL_PR seconds (stable, unlikely to change)
        if [ "$cached_value" = "none" ]; then
            max_cache_age="$CACHE_TTL_NONE"
        else
            max_cache_age="$CACHE_TTL_PR"
        fi

        if [ "$cache_age" -lt "$max_cache_age" ]; then
            cache_valid=true
            pr_text="$cached_value"
        fi
    fi

    # If no valid cache, perform network lookup with timeout
    if [ "$cache_valid" = false ] && [ -n "$current_commit" ]; then
        pr_text="none"
        if command -v gh >/dev/null 2>&1; then
            pr_info=""

            tracking_remote_name=""
            tracking_repo=""
            if [ "$remote" != "no upstream" ]; then
                tracking_remote_name="${remote%%/*}"
            fi
            if [ -n "$tracking_remote_name" ]; then
                tracking_url=$(git remote get-url "$tracking_remote_name" 2>/dev/null)
                if [ -n "$tracking_url" ]; then
                    # Use the same shared parser function for consistency
                    tracking_repo=$(parse_repo_from_url "$tracking_url")
                fi
            fi

            # First try to look up by branch name (works for local branches tied to PRs)
            if [ -n "$local_branch" ]; then
                if [ -n "$repo_name" ]; then
                    pr_info=$(gh_with_timeout 5 pr view --repo "$repo_name" --json number,url --template '{{.number}} {{.url}}' "$local_branch" 2>/dev/null)
                fi
                # Fallback: try the tracking remote's repo (fork workflow)
                if [ -z "$pr_info" ] && [ -n "$tracking_repo" ] && [ "$tracking_repo" != "$repo_name" ]; then
                    pr_info=$(gh_with_timeout 5 pr view --repo "$tracking_repo" --json number,url --template '{{.number}} {{.url}}' "$local_branch" 2>/dev/null)
                fi
                if [ -z "$pr_info" ] && [ -z "$repo_name" ] && [ -z "$tracking_repo" ]; then
                    pr_info=$(gh_with_timeout 5 pr view --json number,url --template '{{.number}} {{.url}}' "$local_branch" 2>/dev/null)
                fi
            fi

            # If branch lookup failed, fall back to searching by commit SHA
            if [ -z "$pr_info" ] && [ -n "$current_commit" ]; then
                if [ -n "$repo_name" ]; then
                    pr_info=$(gh_with_timeout 5 pr list --repo "$repo_name" --state all --json number,url --search "sha:$current_commit" --limit 1 --template '{{- range $i, $pr := . -}}{{- if eq $i 0 -}}{{printf "%v %v" $pr.number $pr.url}}{{- end -}}{{- end -}}' 2>/dev/null)
                fi
                if [ -z "$pr_info" ] && [ -n "$tracking_repo" ] && [ "$tracking_repo" != "$repo_name" ]; then
                    pr_info=$(gh_with_timeout 5 pr list --repo "$tracking_repo" --state all --json number,url --search "sha:$current_commit" --limit 1 --template '{{- range $i, $pr := . -}}{{- if eq $i 0 -}}{{printf "%v %v" $pr.number $pr.url}}{{- end -}}{{- end -}}' 2>/dev/null)
                fi
                # Fallback: try without --repo when both repo_name and tracking_repo are empty
                if [ -z "$pr_info" ] && [ -z "$repo_name" ] && [ -z "$tracking_repo" ]; then
                    pr_info=$(gh_with_timeout 5 pr list --state all --json number,url --search "sha:$current_commit" --limit 1 --template '{{- range $i, $pr := . -}}{{- if eq $i 0 -}}{{printf "%v %v" $pr.number $pr.url}}{{- end -}}{{- end -}}' 2>/dev/null)
                fi
            fi

            if [ -n "$pr_info" ]; then
                # Split the PR info into number and URL (preserve $1)
                pr_number="${pr_info%% *}"
                pr_url="${pr_info#* }"
                # Handle case where there's no URL (only number)
                if [ "$pr_number" = "$pr_url" ]; then
                    pr_url=""
                fi
                if [ -n "$pr_number" ]; then
                    if [ -n "$pr_url" ]; then
                        pr_text="#$pr_number $pr_url"
                    else
                        pr_text="#$pr_number"
                    fi
                fi
            fi
        fi

        # Cache the result (even if none to avoid repeated lookups)
        echo "$pr_text" > "$cache_file" 2>/dev/null
    fi
fi





# Parse arguments (--with-status anywhere; --status-only as no-op alias for backward compat)
WITH_STATUS=false
for arg in "$@"; do
    case "$arg" in
        --with-status) WITH_STATUS=true ;;
        --status-only) : ;;  # no-op alias for backward compatibility
    esac
done

# Show git status output only when explicitly requested
if [ "$WITH_STATUS" = true ]; then
    echo "=== Git Status ==="
    git status
    echo
fi



# 3-line layout to avoid truncation on narrow terminals:
#   Line 1: Dir + Branch (status)
#   Line 2: upstream ref + PR + ctx bar
#   Line 3: PR URL (if present)

short_pr=$(echo "$pr_text" | sed 's| https://[^ ]*||')

# Capture tracked upstream for display
upstream_ref=""
if [ -n "$local_branch" ]; then
    upstream_ref=$(git rev-parse --abbrev-ref --symbolic-full-name "$local_branch@{u}" 2>/dev/null)
    if [ -z "$upstream_ref" ] || [ "$upstream_ref" = "$local_branch@{u}" ]; then
        upstream_ref=""
    fi
fi

# Line 1: Dir + Branch (with sync status) — no remote/PR to keep it short
cols=$(tput cols 2>/dev/null || echo 120)
dir_budget=$(( cols - ${#local_branch} - ${#local_status} - 20 ))
if [ "$dir_budget" -lt 8 ]; then dir_budget=8; fi
if [ ${#working_dir} -gt "$dir_budget" ]; then
    short_dir="${working_dir:0:$((dir_budget - 1))}…"
else
    short_dir="$working_dir"
fi
line1="[Dir: $short_dir | Branch: $local_branch$local_status]"
echo -e "\033[1;36m${line1}\033[0m"

# Line 2: upstream tracking + PR + ctx bar (all secondary info on one line)
pct_num=0
[ -n "$CONTEXT_PCT" ] && pct_num=$(echo "$CONTEXT_PCT" | awk '{v=int($1); print (v<0?0:(v>100?100:v))}')
if [ "$pct_num" -lt 30 ]; then
    bar_color="\033[1;32m"
elif [ "$pct_num" -le 60 ]; then
    bar_color="\033[1;33m"
elif [ "$pct_num" -le 80 ]; then
    bar_color="\033[1;38;5;208m"
else
    bar_color="\033[1;31m"
fi
filled=$((pct_num / 10))
empty=$((10 - filled))
bar=""
# Keep progress bar ASCII-only for cross-platform safety.
# GNU tr treats multibyte replacements byte-wise and can emit invalid UTF-8.
[ "$filled" -gt 0 ] && bar=$(printf "%${filled}s" | tr ' ' '#')
[ "$empty"  -gt 0 ] && bar="${bar}$(printf "%${empty}s" | tr ' ' '-')"
ctx_label="${CONTEXT_PCT:----}%"

line2_parts=""
[ -n "$upstream_ref" ] && line2_parts="→ $upstream_ref"
[ -n "$short_pr" ] && [ "$short_pr" != "none" ] && {
    [ -n "$line2_parts" ] && line2_parts="$line2_parts | "
    line2_parts="${line2_parts}PR: $short_pr"
}
echo -e "${bar_color}${line2_parts:+${line2_parts} | }ctx ${bar} ${ctx_label}\033[0m"

# Line 3: PR URL (only when a real PR URL exists)
pr_url=$(echo "$pr_text" | grep -o 'https://[^ ]*' | head -1)
[ -n "$pr_url" ] && printf '%s\n' "$pr_url"
exit 0
