#!/usr/bin/env bash
# Dark Factory — uv-based install + smoke run.
#
# Installs a uv-managed Python + venv, links `dark-factory` and `df-healer`
# into ~/.local/bin, then smoke-runs via the installed binary wrapper.
#
# Usage:
#   ./install.sh              # install (or refresh deps) + smoke run
#   ./install.sh --clear      # recreate .venv from scratch
#   ./install.sh --no-smoke   # install only, skip smoke run
#   ./install.sh --no-link    # skip ~/.local/bin symlinks
#   ./install.sh --no-cmds    # skip ~/.claude/ commands+skills symlinks
#
# Requires: uv on PATH (https://docs.astral.sh/uv/)

set -euo pipefail

PYTHON_VERSION="${PYTHON_VERSION:-3.13}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENV_DIR="${REPO_ROOT}/.venv"
PYTHON_BIN="${VENV_DIR}/bin/python"
BIN_DIR="${REPO_ROOT}/bin"
LOCAL_BIN="${HOME}/.local/bin"
CLEAR=0
SMOKE=1
LINK=1
CMDS=1

usage() {
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --clear) CLEAR=1; shift ;;
    --no-smoke) SMOKE=0; shift ;;
    --no-link) LINK=0; shift ;;
    --no-cmds) CMDS=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if ! command -v uv >/dev/null 2>&1; then
  echo "ERROR: uv not found on PATH." >&2
  echo "Install: curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
  exit 1
fi

echo "==> uv $(uv --version)"
echo "==> repo: ${REPO_ROOT}"
echo "==> python: ${PYTHON_VERSION}"

echo "==> installing Python ${PYTHON_VERSION} via uv"
uv python install "${PYTHON_VERSION}"

if [[ "${CLEAR}" -eq 1 && -d "${VENV_DIR}" ]]; then
  echo "==> removing existing venv (${VENV_DIR})"
  rm -rf "${VENV_DIR}"
fi

if [[ ! -d "${VENV_DIR}" ]]; then
  echo "==> creating venv at ${VENV_DIR}"
  uv venv "${VENV_DIR}" --python "${PYTHON_VERSION}"
else
  echo "==> reusing venv at ${VENV_DIR}"
fi

REQUIREMENTS_FILE="${REPO_ROOT}/requirements.lock"
if [[ ! -f "${REQUIREMENTS_FILE}" ]]; then
  echo "ERROR: requirements.lock not found." >&2
  echo "Regenerate with: uv pip compile requirements.txt --python-version ${PYTHON_VERSION} -o requirements.lock" >&2
  exit 1
fi
echo "==> installing $(basename "${REQUIREMENTS_FILE}") into venv (PyPI via uv pip)"
uv pip install --python "${PYTHON_BIN}" -r "${REQUIREMENTS_FILE}"

echo "==> verifying import"
"${PYTHON_BIN}" -c "import pydot, yaml; print('deps ok:', pydot.__version__)"

# Configure repo-local git hooks (.githooks/) so the pre-push graph-audit
# guard fires before any push. Mirrors the .github/workflows/ci.yml:35
# step locally. Setting core.hooksPath is local-only (.git/config), so
# other operators' checkouts and CI runners are unaffected. Idempotent:
# skips if already pointing at .githooks.
if [[ -d "${REPO_ROOT}/.githooks" ]]; then
  current_hooks_path="$(git -C "${REPO_ROOT}" config --get core.hooksPath 2>/dev/null || true)"
  if [[ "${current_hooks_path}" != ".githooks" ]]; then
    git -C "${REPO_ROOT}" config core.hooksPath .githooks
    echo "==> configured core.hooksPath=.githooks (was: '${current_hooks_path:-<unset>}')"
  else
    echo "==> core.hooksPath already .githooks"
  fi
fi

# Install the bead-JSONL auto-sorter pre-commit hook — root-cause fix
# for the +1686/-1685 noise pattern (worldai PR #7848 et al.). Defense in
# depth on top of the CI guard in tests/test_bead_jsonl_sort.py.
#
# Conditional: skips when scripts/install-beads-hook.sh is absent
# (older clones without the bead machinery) or when .beads/ is absent
# (repo without an initialized br workspace). Both checks are
# independent: a fresh fork with .beads/ but no scripts/ still no-ops,
# and a fresh clone with scripts/ but no .beads/ also no-ops.
#
# Idempotent: the installer overwrites .git/hooks/pre-commit on every
# install.sh run, so re-running after a feat/bead-sort-root-cause
# update picks up the latest hook version.
if [[ -f "${REPO_ROOT}/scripts/install-beads-hook.sh" && -d "${REPO_ROOT}/.beads" ]]; then
  echo "==> Installing bead-JSONL sort pre-commit hook"
  bash "${REPO_ROOT}/scripts/install-beads-hook.sh"
fi

chmod +x "${BIN_DIR}/dark-factory" "${BIN_DIR}/df-healer" "${BIN_DIR}/df-validate"

if [[ "${LINK}" -eq 1 ]]; then
  mkdir -p "${LOCAL_BIN}"
  ln -sf "${BIN_DIR}/dark-factory" "${LOCAL_BIN}/dark-factory"
  ln -sf "${BIN_DIR}/df-healer" "${LOCAL_BIN}/df-healer"
  ln -sf "${BIN_DIR}/df-validate" "${LOCAL_BIN}/df-validate"
  echo "==> linked ${LOCAL_BIN}/dark-factory"
  echo "==> linked ${LOCAL_BIN}/df-healer"
  echo "==> linked ${LOCAL_BIN}/df-validate"
fi

# Mirror repo-scope commands + skills to user-scope (~/.claude/) so /f /fs
# /factory /factory-spec and the dark-factory / factory-spec skills resolve
# from any cwd. The repo is the single source of truth — any drift in
# ~/.claude/ is overwritten on re-run. Pass --no-cmds to skip.
if [[ "${CMDS}" -eq 1 ]]; then
  CLAUDE_DIR="${HOME}/.claude"
  mkdir -p "${CLAUDE_DIR}/commands" "${CLAUDE_DIR}/skills"

  # 4 factory commands — file symlinks so edits land in the repo
  for cmd in f fs factory factory-spec; do
    src="${REPO_ROOT}/.claude/commands/${cmd}.md"
    dst="${CLAUDE_DIR}/commands/${cmd}.md"
    if [[ -f "${src}" ]]; then
      ln -sf "${src}" "${dst}"
      echo "==> linked ${dst} -> ${src}"
    else
      echo "==> SKIP: ${src} not found" >&2
    fi
  done

  # 2 factory skills — directory symlinks (cp -R would break on next run).
  # Existing regular directories are backed up to <dst>.bak.<unix-ts> so the
  # operator can recover user-scope edits; subsequent runs replace the symlink
  # cleanly with `ln -sfn`.
  for skill in dark-factory factory-spec; do
    src="${REPO_ROOT}/.claude/skills/${skill}"
    dst="${CLAUDE_DIR}/skills/${skill}"
    if [[ ! -d "${src}" ]]; then
      echo "==> SKIP: ${src} not found" >&2
      continue
    fi
    if [[ -d "${dst}" && ! -L "${dst}" ]]; then
      backup="${dst}.bak.$(date +%s)"
      echo "==> ${dst} is a regular directory; backing up to ${backup}"
      mv "${dst}" "${backup}"
    fi
    ln -sfn "${src}" "${dst}"
    echo "==> linked ${dst} -> ${src}"
  done
fi

export DARK_FACTORY_HOME="${REPO_ROOT}"
export PATH="${LOCAL_BIN}:${PATH}"

if [[ "${SMOKE}" -eq 1 ]]; then
  echo "==> smoke run via dark-factory binary (parallel demo, echo backend, no LLM)"
  cd "${REPO_ROOT}"
  "${BIN_DIR}/dark-factory" \
    --pipeline pipelines/parallel_demo.dot \
    --goal "install.sh smoke" \
    --no-perf-log \
    --max-steps 20 \
    --backend echo
fi

cat <<EOF

Dark Factory ready (binary install).

  export DARK_FACTORY_HOME="${REPO_ROOT}"
  export DARK_FACTORY_HOLDOUTS="\${HOME}/projects/dark-factory-holdouts"
  export PATH="\${HOME}/.local/bin:\${PATH}"

  # full loop (/f default pipeline) — run from any repo; implements in cwd
  dark-factory \\
    --pipeline pipelines/slim/minimal_feature.dot \\
    --goal "your feature" \\
    --backend claude \\
    --feature hello \\
    --cxdb ~/.dark-factory/cxdb.sqlite

  # healer
  df-healer --cxdb ~/.dark-factory/cxdb.sqlite

EOF
