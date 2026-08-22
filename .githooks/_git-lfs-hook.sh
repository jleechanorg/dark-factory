#!/bin/sh
# Parameterized git-lfs post-* hook helper.
#
# Each post-* consumer shim in this directory is a 2-line delegation:
#
#     #!/bin/sh
#     GIT_LFS_VERB=<verb> . "$(dirname "$0")/_git-lfs-hook.sh" "$@"
#
# The verb is passed via the `GIT_LFS_VERB` env var rather than a
# positional argument because POSIX `sh` (e.g., dash on Debian/Ubuntu,
# which is what `#!/bin/sh` resolves to there) does NOT forward args
# to a `.`-sourced script. Bash does, but Git invokes hooks under
# `#!/bin/sh`, so we need the env-var form to work under both.
#
# The helper centralizes the git-lfs presence check and fail-closed
# behavior; the consumers are pure shims. The error message uses
# $(basename "$0") so the calling hook names itself when run via
# `git hook` or directly, regardless of which shim invoked us.
#
# Env var / Positional arg:
#   GIT_LFS_VERB or $1 = the git lfs subcommand verb (e.g., post-checkout,
#                        post-commit, post-merge). Remaining args are
#                        forwarded to `git lfs "$verb"`.

set -eu

verb="${GIT_LFS_VERB:-${1:-}}"

if [ -z "$verb" ]; then
  echo >&2 "error: $(basename "$0"): hook verb must be set via GIT_LFS_VERB or positional argument"
  exit 2
fi

if [ -z "${GIT_LFS_VERB:-}" ] && [ $# -gt 0 ]; then
  shift
fi

command -v git-lfs >/dev/null 2>&1 || {
  echo >&2 "error: $(basename "$0"): git-lfs was not found on PATH (cannot run git lfs $verb)"
  exit 2
}

exec git lfs "$verb" "$@"

