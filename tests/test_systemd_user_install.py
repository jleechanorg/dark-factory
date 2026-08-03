import os
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "daemon" / "systemd" / "install-systemd-user.sh"


def test_systemd_user_unit_renders_rust_daemon_with_watchdog(tmp_path):
    env = os.environ.copy()
    env["HOME"] = str(tmp_path / "home")

    rendered = subprocess.check_output(
        [str(INSTALLER), "--render-only", "--repo", str(ROOT)],
        cwd=ROOT,
        env=env,
        text=True,
    )

    assert "Type=notify\n" in rendered
    assert "NotifyAccess=main\n" in rendered
    assert "Restart=on-failure\n" in rendered
    assert "WatchdogSec=7200s\n" in rendered
    assert ".npm-global/bin" in rendered
    assert ".nvm/versions/node/v22.22.0/bin" in rendered
    assert f"WorkingDirectory={ROOT}\n" in rendered
    assert f"ExecStart={ROOT}/daemon/target/release/daemon\n" in rendered
    assert "factory-af-tick" not in rendered
    assert "factory-tick" not in rendered
    assert "launchd" not in rendered


def test_systemd_user_installer_dry_run_has_no_host_mutation(tmp_path):
    env = os.environ.copy()
    env["HOME"] = str(tmp_path / "home")
    env["SYSTEMD_USER_DIR"] = str(tmp_path / "systemd-user")

    dry_run = subprocess.check_output(
        [str(INSTALLER), "--dry-run", "--skip-build", "--repo", str(ROOT)],
        cwd=ROOT,
        env=env,
        text=True,
    )

    assert "systemctl --user daemon-reload" in dry_run
    assert "loginctl show-user" in dry_run
    assert "loginctl enable-linger" in dry_run
    assert "systemctl --user enable --now ai.dark-factory.daemon.service" in dry_run
    assert not (tmp_path / "systemd-user").exists()
