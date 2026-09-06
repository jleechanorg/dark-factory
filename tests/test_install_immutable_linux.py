import hashlib
import json
import os
import shutil
import stat
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RELEASE_SHA = "0123456789abcdef0123456789abcdef01234567"
RELEASE_TREE = "89abcdef0123456789abcdef0123456789abcdef"
MIGRATED_RELEASE_TREE = "fedcba9876543210fedcba9876543210fedcba99"


def _write_executable(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def test_linux_install_keeps_all_runtime_payloads_outside_git_checkout(tmp_path):
    checkout = tmp_path / "checkout"
    checkout.mkdir()
    shutil.copy2(ROOT / "install.sh", checkout / "install.sh")
    (checkout / "requirements.lock").write_text("")
    (checkout / "ignored-sentinel.bin").write_text("must not enter release")
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
    bridge_source = ROOT / "daemon" / "scripts" / "ao-spawn-v013-bridge.mjs"
    bridge_target = checkout / "daemon" / "scripts" / bridge_source.name
    bridge_target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(bridge_source, bridge_target)
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
  if [ -n "${{DARK_FACTORY_TEST_HEAD_FILE:-}}" ] && [ -f "${{DARK_FACTORY_TEST_HEAD_FILE:-}}" ]; then
    cat "$DARK_FACTORY_TEST_HEAD_FILE"
  else
    printf '{RELEASE_SHA}\\n'
  fi
elif [ "${{3:-}}" = "rev-parse" ] && [ "${{4:-}}" = '{RELEASE_SHA}^{{tree}}' ]; then
  printf '{RELEASE_TREE}\\n'
elif [ "${{3:-}}" = "rev-parse" ] && [ "${{4%%^*}}" != "${{4:-}}" ]; then
  # Any other <sha>^{{tree}} lookup (e.g. the migration re-run's new HEAD) —
  # the manifest tree value is unasserted for that release, so any distinct
  # valid 40-hex object id is enough.
  printf '{MIGRATED_RELEASE_TREE}\\n'
elif [ "${{3:-}}" = "archive" ]; then
  tar -C "${{2}}" --exclude=ignored-sentinel.bin -cf - .
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
if [ "${1:-}" = "-" ]; then
  exec /usr/bin/python3 "$@"
fi
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
    assert not (release / "ignored-sentinel.bin").exists()
    assert not ((release / "install.sh").stat().st_mode & stat.S_IWUSR)
    daemon_binary = release / "daemon" / "target" / "release" / "daemon"
    assert daemon_binary.is_file()
    assert not (daemon_binary.stat().st_mode & stat.S_IWUSR)
    release_manifest = json.loads((release / "release-manifest.json").read_text())
    assert release_manifest["schema_version"] == 2
    assert release_manifest["source_commit"] == RELEASE_SHA
    assert release_manifest["source_tree"] == RELEASE_TREE
    assert release_manifest["files"]["daemon/target/release/daemon"]["sha256"] == (
        hashlib.sha256(daemon_binary.read_bytes()).hexdigest()
    )
    assert ".venv/bin/python" in release_manifest["files"]
    assert "daemon/scripts/ao-spawn-v013-bridge.mjs" in release_manifest["files"]
    state_root = home / ".local" / "state" / "dark-factory"
    state_db = state_root / ".beads" / "beads.db"
    assert state_db.is_file()
    assert (state_root / ".beads" / "issues.jsonl").read_text() == seed_beads.read_text()
    assert br_log.read_text().splitlines() == [
        f"init --db {state_db}",
        f"sync --db {state_db} --import-only",
    ]

    # Verify migration on upgrade / re-run with existing DB
    migrated_beads = '{"id":"factory-tdd"}\n{"id":"factory-migrated"}\n'
    seed_beads.write_text(migrated_beads)
    head_file = tmp_path / "head.txt"
    head_file.write_text("fedcba9876543210fedcba9876543210fedcba98\n")
    migrate_env = env.copy()
    migrate_env["DARK_FACTORY_TEST_HEAD_FILE"] = str(head_file)
    migrate_proc = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=migrate_env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert migrate_proc.returncode == 0, migrate_proc.stdout + migrate_proc.stderr
    assert (state_root / ".beads" / "issues.jsonl").read_text() == migrated_beads
    assert br_log.read_text().splitlines() == [
        f"init --db {state_db}",
        f"sync --db {state_db} --import-only",
        f"sync --db {state_db} --import-only",
    ]

    new_release = install_root / "releases" / "fedcba9876543210fedcba9876543210fedcba98"
    new_daemon_binary = new_release / "daemon" / "target" / "release" / "daemon"

    rendered_unit = subprocess.run(
        [str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert rendered_unit.returncode == 0, rendered_unit.stdout + rendered_unit.stderr
    assert f"WorkingDirectory={new_release}\n" in rendered_unit.stdout
    assert f"ExecStart={new_daemon_binary}\n" in rendered_unit.stdout
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
    assert all(Path(path) == new_release for path in runtime_homes)

    # Reuse must validate the complete executable/runtime payload with a
    # verifier outside the release. Restoring read-only mode after tampering
    # must not bypass provenance.
    venv_python = release / ".venv" / "bin" / "python"
    original_python = venv_python.read_bytes()
    original_python_mode = venv_python.stat().st_mode
    venv_python.chmod(venv_python.stat().st_mode | stat.S_IWUSR)
    venv_python.write_text("#!/bin/sh\nexit 9\n")
    venv_python.chmod(original_python_mode & ~stat.S_IWUSR)
    tampered_python = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert tampered_python.returncode != 0
    assert "release manifest does not match complete runtime payload" in tampered_python.stderr
    venv_python.chmod(venv_python.stat().st_mode | stat.S_IWUSR)
    venv_python.write_bytes(original_python)
    venv_python.chmod(original_python_mode & ~stat.S_IWUSR)

    bridge = release / "daemon" / "scripts" / "ao-spawn-v013-bridge.mjs"
    original_bridge = bridge.read_bytes()
    bridge.chmod(bridge.stat().st_mode | stat.S_IWUSR)
    bridge.write_text("throw new Error('tampered');\n")
    bridge.chmod(bridge.stat().st_mode & ~stat.S_IWUSR)
    tampered_bridge = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert tampered_bridge.returncode != 0
    assert "release manifest does not match complete runtime payload" in tampered_bridge.stderr
    bridge.chmod(bridge.stat().st_mode | stat.S_IWUSR)
    bridge.write_bytes(original_bridge)
    bridge.chmod(bridge.stat().st_mode & ~stat.S_IWUSR)

    daemon_binary.chmod(daemon_binary.stat().st_mode | stat.S_IWUSR)
    daemon_binary.write_text("#!/bin/sh\nexit 9\n")
    daemon_binary.chmod(daemon_binary.stat().st_mode & ~stat.S_IWUSR)
    tampered = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert tampered.returncode != 0
    assert "release manifest does not match complete runtime payload" in tampered.stderr

    release.chmod(release.stat().st_mode | stat.S_IWUSR)
    mutable = subprocess.run(
        [str(checkout / "install.sh"), "--no-smoke"],
        cwd=checkout,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert mutable.returncode != 0
    assert "refusing to reuse mutable release" in mutable.stderr

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
