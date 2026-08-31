"""Regression guards for the binary-first slash-command contract."""
from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess


def _install_fake_runner_python(home: pathlib.Path, call_log: pathlib.Path) -> None:
    (home / ".venv" / "bin").mkdir(parents=True, exist_ok=True)
    call_log.write_text("", encoding="utf-8")
    fake_python = home / ".venv" / "bin" / "python"
    fake_python.write_text(
        "#!/usr/bin/env python3\n"
        "import json\n"
        "import os\n"
        "import pathlib\n"
        "import sys\n"
        "\n"
        "log_path = pathlib.Path(os.environ['DF_FAKE_RUNNER_CALL_LOG'])\n"
        "log_path.parent.mkdir(parents=True, exist_ok=True)\n"
        "with log_path.open('a', encoding='utf-8') as handle:\n"
        "    handle.write(json.dumps({'argv': sys.argv[1:]}) + '\\n')\n"
        "\n"
        "if len(sys.argv) >= 3 and sys.argv[1] == '-m':\n"
        "    module = sys.argv[2]\n"
        "    args = sys.argv[3:]\n"
        "    if module == 'runner.preflight':\n"
        "        if '--json' in sys.argv:\n"
        "            preflight_json = os.environ.get('DF_FAKE_PREFLIGHT_JSON', '{}')\n"
        "            print(preflight_json)\n"
        "            rc = int(os.environ.get('DF_FAKE_PREFLIGHT_RC', '0'))\n"
        "            raise SystemExit(rc)\n"
        "        raise SystemExit(0)\n"
        "\n"
        "    if module == 'runner._merge_train':\n"
        "        raise SystemExit(0)\n"
        "\n"
        "    if module == 'runner.healer':\n"
        "        raise SystemExit(0)\n"
        "\n"
        "    if module == 'runner':\n"
        "        if args and args[0] == 'review':\n"
        "            rc = int(os.environ.get('DF_FAKE_REVIEW_RETURN', '0'))\n"
        "            raise SystemExit(rc)\n"
        "        if 'review' in args:\n"
        "            # Simulate a broken review invocation when the wrapper injects\n"
        "            # positional flags ahead of the review subcommand.\n"
        "            raise SystemExit(3)\n"
        "        raise SystemExit(0)\n"
        "\n"
        "    if module == 'runner.panic_hook':\n"
        "        panic_root = pathlib.Path.home() / '.dark-factory' / 'panics'\n"
        "        panic_root.mkdir(parents=True, exist_ok=True)\n"
        "        (panic_root / 'wrapper-review-panic.txt').write_text('panic', encoding='utf-8')\n"
        "        raise SystemExit(0)\n"
        "\n"
        "raise SystemExit(0)\n",
        encoding="utf-8",
    )
    fake_python.chmod(0o755)


def _read_fake_runner_calls(call_log: pathlib.Path) -> list[list[str]]:
    if not call_log.exists():
        return []
    return [json.loads(line)["argv"] for line in call_log.read_text(encoding="utf-8").splitlines() if line.strip()]


def _run_installed_wrapper(
    binary_args: list[str],
    *,
    workdir: pathlib.Path,
    review_return: int = 0,
    env_overrides: dict[str, str] | None = None,
    binary_name: str = "dark-factory",
) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
    fake_home = workdir / "fake_home"
    fake_home.mkdir(parents=True, exist_ok=True)
    call_log = fake_home / "fake-runner-calls.jsonl"
    _install_fake_runner_python(fake_home, call_log)
    env = os.environ.copy()
    env["HOME"] = str(fake_home)
    env["DARK_FACTORY_HOME"] = str(fake_home)
    env["DF_FAKE_RUNNER_CALL_LOG"] = str(call_log)
    env["DF_FAKE_REVIEW_RETURN"] = str(review_return)
    if env_overrides:
        env.update(env_overrides)
    result = subprocess.run(
        [str(ROOT / "bin" / binary_name), *binary_args],
        cwd=workdir,
        env=env,
        capture_output=True,
        text=True,
    )
    return result, call_log


def test_dark_factory_review_invocation_does_not_inject_workdir_before_review(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    task_file = workdir / "task.md"
    output_dir = workdir / "review-output"
    task_file.write_text("run a controller review", encoding="utf-8")
    output_dir.mkdir(exist_ok=True)

    result, call_log = _run_installed_wrapper(
        [
            "review",
            "--base-sha",
            "a" * 40,
            "--head-sha",
            "b" * 40,
            "--task-file",
            str(task_file),
            "--output-dir",
            str(output_dir),
        ],
        workdir=workdir,
    )

    assert result.returncode == 0
    calls = _read_fake_runner_calls(call_log)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert len(runner_calls) == 1
    assert runner_calls[0][2] == "review"
    assert "--workdir" not in runner_calls[0][3:]
    assert not any(call[:2] == ["-m", "runner._merge_train"] for call in calls)


def test_dark_factory_pipeline_paths_still_take_merge_train_lock(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    result, call_log = _run_installed_wrapper(
        ["--pipeline", "benchmarks/attractor-spec-review/pipelines/review_main.dot", "--goal", "ci"],
        workdir=workdir,
    )

    assert result.returncode == 0
    calls = _read_fake_runner_calls(call_log)
    assert any(call[:2] == ["-m", "runner._merge_train"] for call in calls)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert runner_calls[0][2:] == [
        "--workdir",
        str(workdir),
        "--pipeline",
        "benchmarks/attractor-spec-review/pipelines/review_main.dot",
        "--goal",
        "ci",
    ]


def test_dark_factory_review_failure_does_not_create_panic_artifact(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    task_file = workdir / "task.md"
    output_dir = workdir / "review-output-fail"
    task_file.write_text("run a controller review", encoding="utf-8")

    result, call_log = _run_installed_wrapper(
        [
            "review",
            "--base-sha",
            "a" * 40,
            "--head-sha",
            "b" * 40,
            "--task-file",
            str(task_file),
            "--output-dir",
            str(output_dir),
        ],
        workdir=workdir,
        review_return=2,
    )

    assert result.returncode == 2
    calls = _read_fake_runner_calls(call_log)
    assert any(call[:2] == ["-m", "runner"] and call[2] == "review" for call in calls)
    assert not any(call[:2] == ["-m", "runner._merge_train"] for call in calls)
    assert not any(call[:2] == ["-m", "runner.panic_hook"] for call in calls)
    fake_home = workdir / "fake_home"
    assert not (fake_home / ".dark-factory" / "panics").exists()

ROOT = pathlib.Path(__file__).resolve().parent.parent
COMMANDS_DIR = ROOT / ".claude" / "commands"
SKILLS_DIR = ROOT / ".claude" / "skills"

PROOF_LABELS = (
    "# Literal command run:",
    "# Run ID:",
    "# CXDB SHA:",
    "# Final outcome:",
    "# Exit code:",
    "# Wall-clock:",
    "# Logs:",
    "# Evidence envelope:",
)


def _command(name: str) -> str:
    return (COMMANDS_DIR / name).read_text()


def _skill(name: str) -> str:
    return (SKILLS_DIR / name / "SKILL.md").read_text()


def _aliases(text: str) -> list[str]:
    match = re.search(r"^aliases:\s*\[(.*?)\]\s*$", text, re.MULTILINE)
    if not match:
        return []
    body = match.group(1).strip()
    if not body:
        return []
    return [part.strip().strip("\"'") for part in body.split(",")]


def test_f_and_fs_require_binary_proof_block() -> None:
    """f.md/fs.md are thin stubs pointing at a canonical SKILL.md; the
    binary proof-block contract lives there (single source of truth), not
    duplicated in the command file. Verify the skill each command points
    at actually carries the full contract, and that the stub itself still
    points at that skill (so the pointer can't silently rot)."""
    missing: list[str] = []
    command_to_skill = {"f.md": "dark-factory", "fs.md": "factory-spec"}
    for command_name, skill_name in command_to_skill.items():
        command_text = _command(command_name)
        if f"skills/{skill_name}/SKILL.md" not in command_text:
            missing.append(f"{command_name}: no longer points at skills/{skill_name}/SKILL.md")

        skill_text = _skill(skill_name)
        for label in PROOF_LABELS:
            if label not in skill_text:
                missing.append(f"{skill_name}/SKILL.md: missing {label}")
        if "dark-factory \\" not in skill_text:
            missing.append(f"{skill_name}/SKILL.md: missing literal dark-factory command shape")
    assert not missing, "Slash command binary proof block is incomplete:\n  " + "\n  ".join(missing)


def test_skill_call_is_not_valid_factory_run_proof() -> None:
    violations: list[str] = []
    forbidden_phrases = (
        "Quote the **actual** command or `Skill()`",
        "Quote the **actual** `Skill()`",
        "Skill() call you ran",
        "Skill(\"dark-factory\"",
    )
    for name in ("f.md", "fs.md", "factory.md"):
        text = _command(name)
        for phrase in forbidden_phrases:
            if phrase in text:
                violations.append(f"{name}: contains invalid proof phrase {phrase!r}")
    assert not violations, "Skill() must not be accepted as factory-run proof:\n  " + "\n  ".join(violations)


def test_workflow_or_skill_cannot_be_redeemed_by_pasted_proof_block() -> None:
    violations: list[str] = []
    invalid_patterns = (
        r"(?:in-Claude workflow|`Skill\(\)`(?: call| result)?).*unless .*proof block",
        r"unless (?:it includes |the )binary proof block",
    )
    for name in ("f.md", "fs.md"):
        compact = " ".join(_command(name).split())
        for pattern in invalid_patterns:
            if re.search(pattern, compact):
                violations.append(f"{name}: matches invalid permissive proof pattern {pattern!r}")
    assert not violations, "Workflow/Skill proof cannot be redeemed by pasted metadata:\n  " + "\n  ".join(violations)


def test_fs_has_no_read_only_in_session_modes() -> None:
    forbidden = ("/fs --show", "read-only, no pipeline invoked")
    invalid_patterns = (
        r"/fs --review[^\n]*(?:no pipeline|read-only|in-session)",
        r"/fs --review-attractor[^\n]*(?:no pipeline|read-only|in-session)",
    )
    violations: list[str] = []
    for path in sorted(COMMANDS_DIR.glob("*.md")):
        text = path.read_text()
        for phrase in forbidden:
            if phrase in text:
                violations.append(f"{path.name}: contains {phrase!r}")
        for pattern in invalid_patterns:
            if re.search(pattern, text):
                violations.append(f"{path.name}: matches {pattern!r}")
    assert not violations, "/fs must be binary-backed; read-only helpers belong in /factory-spec: " + ", ".join(violations)


def test_fs_alias_has_single_owner() -> None:
    owners = [
        path.name
        for path in sorted(COMMANDS_DIR.glob("*.md"))
        if "fs" in _aliases(path.read_text())
    ]
    assert owners == ["fs.md"]


def test_f_defaults_to_fresh_reviewer_and_keeps_calibration_explicit() -> None:
    """The default /f and /factory route is fresh review; calibration is opt-in.

    The reviewer-calibration contract lives in dark-factory/SKILL.md (single
    source of truth) rather than being duplicated in each command file.
    """
    f_text = _command("f.md")
    factory_text = _command("factory.md")
    assert "skills/dark-factory/SKILL.md" in f_text, "f.md no longer points at dark-factory/SKILL.md"
    assert "skills/dark-factory/SKILL.md" in factory_text, "factory.md no longer points at dark-factory/SKILL.md"

    skill_text = _skill("dark-factory")
    required_phrases = (
        "--reviewer-calibration=true",
        "explicit opt-in",
        "fresh fully tooled",
        "## Reviewer calibration",
        "evidence/<run-id>/reviewer-calibration/",
        "dark-factory review \\",
        "--workdir <repo>",
        "--base-sha <full-40-hex-sha>",
        "--head-sha <full-40-hex-sha>",
        "--task-file <path>",
        "--output-dir <dir>",
        "--backend codex",
        "controller-receipt.json",
        "SHA-256 of the controller receipt",
        "prompt SHA-256",
        "envelope SHA-256",
        "`prompt.txt` is a binary-emitted audit capture, not an authority file",
    )
    missing = [phrase for phrase in required_phrases if phrase not in skill_text]
    assert not missing, "dark-factory/SKILL.md missing reviewer calibration contract: " + ", ".join(missing)
    assert "calibration on" not in f_text.lower()
    assert "calibration on" not in factory_text.lower()
    assert "reviewer calibration: not requested" in skill_text.lower()


def test_f_resolves_factory_home_from_the_installed_binary() -> None:
    """The /f skill must not route a PATH-selected binary to a stale checkout."""
    skill_text = _skill("dark-factory")
    required = (
        'if [ -z "${DARK_FACTORY_HOME:-}" ]; then',
        'DARK_FACTORY_BIN="$(command -v dark-factory 2>/dev/null)"',
        "os.path.realpath(sys.argv[1])",
        'DARK_FACTORY_HOME="$(cd "$(dirname "$DARK_FACTORY_BIN")/.." && pwd -P)"',
        "export DARK_FACTORY_HOME",
    )
    missing = [phrase for phrase in required if phrase not in skill_text]
    assert not missing, "dark-factory/SKILL.md missing installed-binary home resolver: " + ", ".join(missing)
    assert "DARK_FACTORY_HOME=~/projects/dark-factory" not in skill_text


def test_reviewer_contract_is_binary_owned_and_vendor_neutral() -> None:
    paths = (
        SKILLS_DIR / "dark-factory" / "SKILL.md",
        SKILLS_DIR / "reviewer-calibration" / "SKILL.md",
        ROOT / ".claude" / "workflows" / "dark-factory.md",
    )
    required_command_parts = (
        "dark-factory review \\",
        "--workdir",
        "--base-sha",
        "--head-sha",
        "--task-file",
        "--output-dir",
        "--backend",
    )
    forbidden = (
        "codex exec --yolo -m ",
        "codex exec --yolo --skip-git-repo-check",
        "claude --print --dangerously-skip-permissions /skeptic",
        'ADVERSARIAL_REVIEWER="${ADVERSARIAL_REVIEWER:-codex}"',
    )
    violations: list[str] = []
    for path in paths:
        text = path.read_text()
        missing = [part for part in required_command_parts if part not in text]
        if missing:
            violations.append(f"{path}: missing binary review command parts {missing}")
        for phrase in forbidden:
            if phrase in text:
                violations.append(f"{path}: contains raw/vendor-pinned reviewer command {phrase!r}")
    assert not violations, "Reviewer authority must remain binary-owned:\n  " + "\n  ".join(violations)


def test_reviewer_contract_requires_digest_bound_controller_receipt() -> None:
    texts = (
        _skill("dark-factory"),
        _skill("reviewer-calibration"),
        (ROOT / ".claude" / "workflows" / "dark-factory.md").read_text(),
    )
    required = (
        "controller receipt",
        "prompt",
        "envelope",
        "digest",
        "base",
        "head",
    )
    for text in texts:
        lowered = text.lower()
        missing = [term for term in required if term not in lowered]
        assert not missing, f"review controller receipt contract missing {missing}"
    assert "prompt.txt` is a binary-emitted audit capture" in texts[0]
    assert "`prompt.txt` is a binary-emitted audit capture only" in texts[1]


def test_dark_factory_preflight_warn_replaces_explicit_backend_arg(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    preflight_json = json.dumps({
        "status": "warn",
        "configured": "codex",
        "fallback_recommendation": "claude",
    })
    result, call_log = _run_installed_wrapper(
        ["--pipeline", "two_node.dot", "--backend", "codex", "--goal", "ci"],
        workdir=workdir,
        env_overrides={"DF_FAKE_PREFLIGHT_JSON": preflight_json},
    )
    assert result.returncode == 0
    assert "codex missing; falling back to claude" in result.stderr
    calls = _read_fake_runner_calls(call_log)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert runner_calls[0][2:] == [
        "--workdir",
        str(workdir),
        "--pipeline",
        "two_node.dot",
        "--backend",
        "claude",
        "--goal",
        "ci",
    ]


def test_dark_factory_preflight_warn_replaces_equals_backend_arg(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    preflight_json = json.dumps({
        "status": "warn",
        "configured": "codex",
        "fallback_recommendation": "claude",
    })
    result, call_log = _run_installed_wrapper(
        ["--pipeline", "two_node.dot", "--backend=codex", "--goal", "ci"],
        workdir=workdir,
        env_overrides={"DF_FAKE_PREFLIGHT_JSON": preflight_json},
    )
    assert result.returncode == 0
    assert "codex missing; falling back to claude" in result.stderr
    calls = _read_fake_runner_calls(call_log)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert runner_calls[0][2:] == [
        "--workdir",
        str(workdir),
        "--pipeline",
        "two_node.dot",
        "--backend=claude",
        "--goal",
        "ci",
    ]


def test_dark_factory_preflight_warn_adds_backend_arg_when_absent(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    preflight_json = json.dumps({
        "status": "warn",
        "configured": "codex",
        "fallback_recommendation": "claude",
    })
    result, call_log = _run_installed_wrapper(
        ["--pipeline", "two_node.dot", "--goal", "ci"],
        workdir=workdir,
        env_overrides={
            "DARK_FACTORY_BACKEND": "codex",
            "DF_FAKE_PREFLIGHT_JSON": preflight_json,
        },
    )
    assert result.returncode == 0
    assert "codex missing; falling back to claude" in result.stderr
    calls = _read_fake_runner_calls(call_log)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert runner_calls[0][2:] == [
        "--workdir",
        str(workdir),
        "--backend",
        "claude",
        "--pipeline",
        "two_node.dot",
        "--goal",
        "ci",
    ]


def test_dark_factory_preflight_warn_review_subcommand_keeps_codex_backend(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    task_file = workdir / "task.md"
    output_dir = workdir / "review-output"
    task_file.write_text("run a controller review", encoding="utf-8")
    output_dir.mkdir(exist_ok=True)

    preflight_json = json.dumps({
        "status": "warn",
        "configured": "codex",
        "fallback_recommendation": "claude",
    })
    result, call_log = _run_installed_wrapper(
        [
            "review",
            "--base-sha",
            "a" * 40,
            "--head-sha",
            "b" * 40,
            "--task-file",
            str(task_file),
            "--output-dir",
            str(output_dir),
        ],
        workdir=workdir,
        env_overrides={
            "DARK_FACTORY_BACKEND": "codex",
            "DF_FAKE_PREFLIGHT_JSON": preflight_json,
        },
    )
    assert result.returncode == 0
    assert "codex missing; review keeps the requested backend (no fallback)" in result.stderr
    calls = _read_fake_runner_calls(call_log)
    runner_calls = [call for call in calls if call[:2] == ["-m", "runner"]]
    assert runner_calls[0][2] == "review"
    assert runner_calls[0][3:5] == ["--base-sha", "a" * 40]


def test_df_healer_preflight_warn_replaces_or_adds_backend_arg(tmp_path: pathlib.Path) -> None:
    workdir = tmp_path
    preflight_json = json.dumps({
        "status": "warn",
        "configured": "codex",
        "fallback_recommendation": "echo",
    })
    result, call_log = _run_installed_wrapper(
        ["--backend", "codex"],
        workdir=workdir,
        env_overrides={"DF_FAKE_PREFLIGHT_JSON": preflight_json},
        binary_name="df-healer",
    )
    assert result.returncode == 0
    assert "codex missing; falling back to echo" in result.stderr
    calls = _read_fake_runner_calls(call_log)
    healer_calls = [call for call in calls if call[:2] == ["-m", "runner.healer"]]
    assert healer_calls[0][2:] == ["--backend", "echo"]
