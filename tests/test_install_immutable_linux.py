import hashlib
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


def test_linux_install_exact_reviewed_immutable_release_drift_rejected(tmp_path):
    checkout, fake_bin, home, real_root, install_root = tmp_path / "checkout", tmp_path / "fake-bin", tmp_path / "home", tmp_path / "real-installed", tmp_path / "installed"; real_root.mkdir(); install_root.symlink_to(real_root)
    checkout.mkdir()
    shutil.copy2(ROOT / "install.sh", checkout / "install.sh")
    (checkout / "requirements.lock").write_text("")
    (checkout / ".beads").mkdir(parents=True, exist_ok=True)
    (checkout / ".beads" / "issues.jsonl").write_text("")
    for name in ("dark-factory", "df-healer", "df-validate", "df-funnel", "df-funnel-lanes"):
        _write_executable(checkout / "bin" / name, "#!/bin/sh\nexit 0\n")
    shutil.copytree(ROOT / "daemon" / "systemd", checkout / "daemon" / "systemd")
    (checkout / "daemon" / "Cargo.toml").write_text("[package]\nname = 'daemon'\n")
    payload = checkout / "payload.txt"
    payload.write_text("reviewed-runtime-payload\n")
    payload_sha = hashlib.sha256(payload.read_bytes()).hexdigest()

    rev_sha, stale_sha, drift_sha = "537282f7647676b3a32edcc75a96ad5fa34d5b59", "422e86bc5e123456789abcdef0123456789abcde", "1111111111111111111111111111111111111111"
    stale_rel = install_root / "releases" / stale_sha
    _write_executable(stale_rel / "bin" / "dark-factory", "#!/bin/sh\nexit 0\n")
    (stale_rel / ".dark-factory-runtime-root").write_text(str(stale_rel) + "\n")
    launcher = home / ".local" / "bin" / "dark-factory"
    launcher.parent.mkdir(parents=True, exist_ok=True)
    launcher.symlink_to(stale_rel / "bin" / "dark-factory")

    _write_executable(fake_bin / "uname", "#!/bin/sh\nprintf 'Linux\\n'\n")
    _write_executable(fake_bin / "git-lfs", "#!/bin/sh\nprintf 'git-lfs/3.0.0 (test)\\n'\n")
    _write_executable(fake_bin / "git", '#!/bin/sh\n[ "${3:-}" = "rev-parse" ] && [ "${4:-}" = "HEAD" ] && printf "%s\\n" "${DARK_FACTORY_FAKE_GIT_SHA:-}"\nexit 0\n')
    _write_executable(fake_bin / "cargo", '#!/bin/sh\nmanifest=""\nwhile [ "$#" -gt 0 ]; do [ "$1" = "--manifest-path" ] && manifest="$2"; shift; done\nif [ -n "$manifest" ]; then\n  t="$(dirname "$manifest")/target/release/daemon"\n  mkdir -p "$(dirname "$t")"\n  printf "#!/bin/sh\\nexit 0\\n" > "$t"\n  chmod +x "$t"\nfi\nexit 0\n')
    _write_executable(fake_bin / "uv", '#!/bin/sh\nif [ "${1:-}" = "--version" ]; then\n  printf "uv 0.0.0-test\\n"\nelif [ "${1:-}" = "venv" ]; then\n  mkdir -p "$2/bin"\n  printf "#!/bin/sh\\nexit 0\\n" > "$2/bin/python"\n  chmod +x "$2/bin/python"\nfi\nexit 0\n')
    _write_executable(fake_bin / "br", "#!/bin/sh\nexit 0\n")

    env = {
        **os.environ,
        "HOME": str(home),
        "PATH": f"{fake_bin}:{os.environ['PATH']}",
        "DARK_FACTORY_INSTALL_ROOT": str(install_root),
        "DARK_FACTORY_EXPECTED_RELEASE_SHA": rev_sha,
        "DARK_FACTORY_FAKE_GIT_SHA": rev_sha,
    }

    # 1 & 2: Install exact reviewed release, assert payload hash and rendered unit paths under reviewed release
    assert subprocess.run([str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only", "--uninstall"], cwd=checkout, env=env, capture_output=True, text=True).returncode == 2; r1 = subprocess.run([str(checkout / "install.sh"), "--no-smoke", "--no-cmds"], cwd=checkout, env=env, capture_output=True, text=True)
    assert r1.returncode == 0, r1.stdout + r1.stderr
    rel = real_root.resolve() / "releases" / rev_sha
    assert (rel / "payload.txt").is_file()
    assert hashlib.sha256((rel / "payload.txt").read_bytes()).hexdigest() == payload_sha
    daemon_bin = rel / "daemon" / "target" / "release" / "daemon"

    # Even if launcher resolves to stale release, systemd render must select expected reviewed release
    launcher.unlink()
    launcher.symlink_to(stale_rel / "bin" / "dark-factory")
    unit1 = subprocess.run([str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only"], cwd=checkout, env=env, capture_output=True, text=True)
    assert unit1.returncode == 0, unit1.stdout + unit1.stderr
    assert f"WorkingDirectory={rel}\n" in unit1.stdout, f"dark-factory-824t.1: systemd render must use reviewed release {rel}, got:\n{unit1.stdout}"
    assert f"ExecStart={daemon_bin}\n" in unit1.stdout
    assert stale_sha not in unit1.stdout

    # 3: Idempotent reinstall/render at same requested/reported SHA
    r2 = subprocess.run([str(checkout / "install.sh"), "--no-smoke", "--no-cmds"], cwd=checkout, env=env, capture_output=True, text=True)
    assert r2.returncode == 0, r2.stdout + r2.stderr
    unit2 = subprocess.run([str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only"], cwd=checkout, env=env, capture_output=True, text=True)
    assert unit2.returncode == 0 and unit2.stdout == unit1.stdout
    orig_daemon = daemon_bin.read_bytes(); daemon_bin.chmod(daemon_bin.stat().st_mode | stat.S_IWUSR)
    daemon_bin.write_text("#!/bin/sh\n# poisoned\nexit 1\n")
    poison_run = subprocess.run([str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--dry-run", "--skip-build"], cwd=checkout, env=env, capture_output=True, text=True)
    assert poison_run.returncode != 0, f"dark-factory-824t.1: expected failure on same-SHA poisoned daemon in {rel}"; daemon_bin.write_bytes(orig_daemon)

    # 4 & 5: Drift rejection: source payload changes and git reports drift SHA while requested SHA remains fixed
    payload.write_text("drifted-runtime-payload\n")
    drift_env = {**env, "DARK_FACTORY_FAKE_GIT_SHA": drift_sha}
    r_drift = subprocess.run([str(checkout / "install.sh"), "--no-smoke", "--no-cmds"], cwd=checkout, env=drift_env, capture_output=True, text=True)
    assert r_drift.returncode != 0, f"dark-factory-824t.1: expected drift rejection for {drift_sha} != {rev_sha}"
    assert not (install_root / "releases" / drift_sha).exists(), "dark-factory-824t.1: drifted release must not be created"
    assert launcher.resolve().is_relative_to(rel), "dark-factory-824t.1: launcher must stay pinned to reviewed release"
    unit_drift = subprocess.run([str(checkout / "daemon" / "systemd" / "install-systemd-user.sh"), "--render-only"], cwd=checkout, env=drift_env, capture_output=True, text=True)
    assert f"WorkingDirectory={rel}\n" in unit_drift.stdout
    assert drift_sha not in unit_drift.stdout
