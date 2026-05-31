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

usage() {
  sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --clear) CLEAR=1; shift ;;
    --no-smoke) SMOKE=0; shift ;;
    --no-link) LINK=0; shift ;;
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

echo "==> installing requirements.txt into venv (PyPI via uv pip)"
uv pip install --python "${PYTHON_BIN}" -r "${REPO_ROOT}/requirements.txt"

echo "==> verifying import"
"${PYTHON_BIN}" -c "import pydot, yaml; print('deps ok:', pydot.__version__)"

chmod +x "${BIN_DIR}/dark-factory" "${BIN_DIR}/df-healer"

if [[ "${LINK}" -eq 1 ]]; then
  mkdir -p "${LOCAL_BIN}"
  ln -sf "${BIN_DIR}/dark-factory" "${LOCAL_BIN}/dark-factory"
  ln -sf "${BIN_DIR}/df-healer" "${LOCAL_BIN}/df-healer"
  echo "==> linked ${LOCAL_BIN}/dark-factory"
  echo "==> linked ${LOCAL_BIN}/df-healer"
fi

export DARK_FACTORY_HOME="${REPO_ROOT}"
export PATH="${LOCAL_BIN}:${PATH}"

if [[ "${SMOKE}" -eq 1 ]]; then
  echo "==> smoke run via dark-factory binary (echo backend, no LLM)"
  cd "${REPO_ROOT}"
  dark-factory \
    --pipeline pipelines/factory/hello.dot \
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
