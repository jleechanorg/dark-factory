import hashlib
import json
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
    seed_beads = checkout / ".beads" / "issues.jsonl"
    seed_beads.parent.mkdir(parents=True, exist_ok=True)
    seed_beads.write_text('{"id":"factory-tdd"}\n')

    for name in ("dark-factory", "df-healer", "df-validate", "df-funnel", "df-funnel-lanes"):
        destination = checkout / "bin" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "bin" / name, destination)
        if name == "df-funnel-lanes":
            # The installer must make every advertised CLI executable in the
            # immutable release; do not inherit this guarantee from Git mode.
            destination.chmod(destination.stat().st_mode & ~stat.S_IXUSR)
    for name in ("f", "fs", "factory", "factory-spec"):
        path = checkout / ".claude" / "commands" / f"{name}.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{name}\n")
    for name in ("dark-factory", "factory-spec"):
        path = checkout / ".claude" / "skills" / name / "SKILL.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"{name}\n")
    shutil.copytree(ROOT / "daemon" / "systemd", checkout / "daemon" / "systemd")
    (checkout / "daemon" / "Cargo.toml").write_text("[package]\nname = 'daemon'\n")

    fake_bin = tmp_path / "fake-bin"
    _write_executable(fake_bin / "uname", "#!/bin/sh\nprintf 'Linux\\n'\n")
    _write_executable(
        fake_bin / "git-lfs",
        "#!/bin/sh\nprintf 'git-lfs/3.0.0 (test)\\n'\n",
    )
    _write_executable(
        fake_bin / "git",
        f"""#!/bin/sh
if [ "${{3:-}}" = "rev-parse" ] && [ "${{4:-}}" = "HEAD" ]; then
  printf '{RELEASE_SHA}\\n'
elif [ "${{3:-}}" = "status" ] && [ "${{DARK_FACTORY_FAKE_DIRTY:-0}}" = "1" ]; then
  printf ' M runner/engine.py\\n'
fi
""",
    )
    _write_executable(
        fake_bin / "cargo",
        """#!/bin/sh
set -eu
manifest=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--manifest-path" ]; then
    manifest="$2"
    break
  fi
  shift
done
[ -n "$manifest" ]
target="$(dirname "$manifest")/target/release/daemon"
mkdir -p "$(dirname "$target")"
printf '#!/bin/sh\\nexit 0\\n' > "$target"
chmod +x "$target"
""",
    )
    _write_executable(
        fake_bin / "uv",
        """#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  printf 'uv 0.0.0-test\\n'
elif [ "${1:-}" = "venv" ]; then
  mkdir -p "$2/bin"
  cat > "$2/bin/python" <<'PY'
#!/bin/sh
if [ -n "${DARK_FACTORY_RUNTIME_LOG:-}" ]; then
  printf '%s\n' "${DARK_FACTORY_HOME:-}" >> "$DARK_FACTORY_RUNTIME_LOG"
fi
exit 0
PY
  chmod +x "$2/bin/python"
fi
""",
    )
    _write_executable(
        fake_bin / "br",
        """#!/bin/sh
set -eu
printf '%s\\n' "$*" >> "${DARK_FACTORY_BR_LOG:?}"
db=''
previous=''
for arg in "$@"; do
  if [ "$previous" = '--db' ]; then
    db="$arg"
    break
  fi
  previous="$arg"
done
[ -n "$db" ]
mkdir -p "$(dirname "$db")"
touch "$db"
""",
    )

    home = tmp_path / "home"
    install_root = tmp_path / "installed"
    runtime_log = tmp_path / "runtime-homes.log"
    br_log = tmp_path / "br.log"
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "PATH": f"{fake_bin}:{env['PATH']}",
            "DARK_FACTORY_INSTALL_ROOT": str(install_root),
            "DARK_FACTORY_RUNTIME_LOG": str(runtime_log),
            "DARK_FACTORY_BR_LOG": str(br_log),
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
        *(home / ".local" / "bin" / name for name in ("dark-factory", "df-healer", "df-validate", "df-funnel", "df-funnel-lanes")),
        *(home / ".claude" / "commands" / f"{name}.md" for name in ("f", "fs", "factory", "factory-spec")),
        *(home / ".claude" / "skills" / name for name in ("dark-factory", "factory-spec")),
    ]
    for entry in runtime_entries:
        resolved = entry.resolve()
        assert resolved.is_relative_to(release), f"{entry} resolves outside release: {resolved}"
        assert not resolved.is_relative_to(checkout), f"{entry} resolves into Git checkout"
    assert (release / "bin" / "df-funnel-lanes").stat().st_mode & stat.S_IXUSR

    assert not (release / ".git").exists()
    assert not ((release / "install.sh").stat().st_mode & stat.S_IWUSR)
    daemon_binary = release / "daemon" / "target" / "release" / "daemon"
    assert daemon_binary.is_file()
    assert not (daemon_binary.stat().st_mode & stat.S_IWUSR)
    release_manifest = json.loads((release / "release-manifest.json").read_text())
    assert release_manifest == {
        "schema_version": 1,
        "source_commit": RELEASE_SHA,
        "daemon": {
            "path": "daemon/target/release/daemon",
            "sha256": hashlib.sha256(daemon_binary.read_bytes()).hexdigest(),
        },
    }
    state_root = home / ".local" / "state" / "dark-factory"
    state_db = state_root / ".beads" / "beads.db"
    assert state_db.is_file()
    assert (state_root / ".beads" / "issues.jsonl").read_text() == seed_beads.read_text()
    assert br_log.read_text().splitlines() == [
        f"init --db {state_db}",
        f"sync --db {state_db} --import-only",
    ]

    rendered_unit = subprocess.run(
        [str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert rendered_unit.returncode == 0, rendered_unit.stdout + rendered_unit.stderr
    assert f"WorkingDirectory={release}\n" in rendered_unit.stdout
    assert f"ExecStart={daemon_binary}\n" in rendered_unit.stdout
    assert f"Environment=DARK_FACTORY_BR_DB={state_db}\n" in rendered_unit.stdout
    assert str(checkout) not in rendered_unit.stdout

    runtime_log.unlink(missing_ok=True)
    poisoned_env = env.copy()
    poisoned_env["DARK_FACTORY_HOME"] = str(checkout)
    launched = subprocess.run(
        [str(home / ".local" / "bin" / "dark-factory"), "review"],
        cwd=tmp_path,
        env=poisoned_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert launched.returncode == 0, launched.stdout + launched.stderr
    runtime_homes = runtime_log.read_text().splitlines()
    assert runtime_homes
    assert all(Path(path) == release for path in runtime_homes)

    dirty_env = env.copy()
    dirty_env["DARK_FACTORY_FAKE_DIRTY"] = "1"
    dirty_env["DARK_FACTORY_INSTALL_ROOT"] = str(tmp_path / "dirty-installed")
    dirty_install = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=dirty_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert dirty_install.returncode != 0
    assert "dirty" in dirty_install.stderr.lower()
