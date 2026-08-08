import os
import shutil
import stat
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RELEASE_SHA = "0123456789abcdef0123456789abcdef01234567"


def _write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def test_linux_install_keeps_all_runtime_payloads_outside_git_checkout(tmp_path):
    checkout = tmp_path / "checkout"
    checkout.mkdir()
    shutil.copy2(ROOT / "install.sh", checkout / "install.sh")
    (checkout / "requirements.lock").write_text("")

    for name in ("dark-factory", "df-healer", "df-validate"):
        _write_executable(checkout / "bin" / name, "#!/bin/sh\nexit 0\n")
    for name in ("f", "fs", "factory", "factory-spec"):
        path = checkout / ".claude" / "commands" / f"{name}.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{name}\n")
    for name in ("dark-factory", "factory-spec"):
        path = checkout / ".claude" / "skills" / name / "SKILL.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{name}\n")

    fake_bin = tmp_path / "fake-bin"
    _write_executable(fake_bin / "uname", "#!/bin/sh\nprintf 'Linux\\n'\n")
    _write_executable(
        fake_bin / "git-lfs",
        "#!/bin/sh\nprintf 'git-lfs/3.0.0 (test)\\n'\n",
    )
    _write_executable(
        fake_bin / "git",
        f"#!/bin/sh\nprintf '{RELEASE_SHA}\\n'\n",
    )
    _write_executable(
        fake_bin / "uv",
        """#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  printf 'uv 0.0.0-test\\n'
elif [ "${1:-}" = "venv" ]; then
  mkdir -p "$2/bin"
  printf '#!/bin/sh\\nexit 0\\n' > "$2/bin/python"
  chmod +x "$2/bin/python"
fi
""",
    )

    home = tmp_path / "home"
    install_root = tmp_path / "installed"
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "PATH": f"{fake_bin}:{env['PATH']}",
            "DARK_FACTORY_INSTALL_ROOT": str(install_root),
        }
    )

    proc = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr

    release = install_root / "releases" / RELEASE_SHA
    runtime_entries = [
        *(home / ".local" / "bin" / name for name in ("dark-factory", "df-healer", "df-validate")),
        *(home / ".claude" / "commands" / f"{name}.md" for name in ("f", "fs", "factory", "factory-spec")),
        *(home / ".claude" / "skills" / name for name in ("dark-factory", "factory-spec")),
    ]
    for entry in runtime_entries:
        resolved = entry.resolve()
        assert resolved.is_relative_to(release), f"{entry} resolves outside release: {resolved}"
        assert not resolved.is_relative_to(checkout), f"{entry} resolves into Git checkout"

    assert not (release / ".git").exists()
    assert not ((release / "install.sh").stat().st_mode & stat.S_IWUSR)
