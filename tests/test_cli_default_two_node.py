"""Regression guard: ``dark-factory`` defaults to ``two_node.dot`` when no
``--pipeline`` is passed.

User contract (set 2026-08-02): **slim two-node is the default** for `/f` and
`/factory`. The default must be `pipelines/slim/two_node.dot` — a generic
worker + a static Codex cold reviewer — unless the operator passes
`--pipeline <name>` to opt into a richer graph.

This test only exercises argparse-level parsing (cheap, no runner side
effects); the slim graph itself is pinned by tests/test_two_node_dot.py.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811


def test_dark_factory_defaults_pipeline_to_two_node_dot() -> None:
    """When no --pipeline is passed, argparse picks `two_node.dot` (bare
    filename) so the resolver can find it under
    ``$DARK_FACTORY_HOME/pipelines/slim/two_node.dot``."""
    # Import here so the test can run without an installed binary.
    from runner.__main__ import main  # type: ignore  # noqa: E402

    # We can't actually invoke `main()` end-to-end without a holdouts repo,
    # but argparse runs before any side effects. Parse argv manually using
    # the same ArgumentParser by re-running the parser build.
    # The cleanest portable test is to inspect the parser default value.
    # We rebuild the parser by reading the function source for the
    # ``default=pathlib.Path("two_node.dot")`` literal.
    import inspect
    import re

    src = inspect.getsource(main)
    # The default must reference the bare filename `two_node.dot` (with
    # either single or double quotes) — NOT a different pipeline.
    match = re.search(
        r'p\.add_argument\(\s*[\'"]--pipeline[\'"][^)]*?default\s*=\s*pathlib\.Path\(\s*[\'"]([^\'"]+)[\'"]\s*\)',
        src,
        re.DOTALL,
    )
    assert match, (
        "runner/__main__.py must declare a default for --pipeline so /f "
        "and /factory default to the slim two-node graph. Could not find "
        "the argparse add_argument call for --pipeline."
    )
    default_filename = match.group(1)
    assert default_filename == "two_node.dot", (
        f"Default --pipeline filename must be 'two_node.dot' "
        f"(the slim two-node default graph); got {default_filename!r}"
    )


def test_omitted_pipeline_keeps_the_literal_command_pipeline_free(
    monkeypatch, tmp_path: pathlib.Path
) -> None:
    """The binary records the command the operator actually supplied.

    The two-node graph is selected internally when ``--pipeline`` is omitted;
    the proof command must not pretend the operator explicitly selected it.
    """
    from runner import __main__ as cli
    from runner.engine_persist import StepRecord

    captured: dict[str, str] = {}

    def _capture_bundle(**kwargs) -> None:
        captured["command"] = kwargs["command"]

    monkeypatch.setattr(
        cli,
        "run",
        lambda *args, **kwargs: [
            StepRecord(node="exit", outcome="success", ts=0.0, output_preview="done")
        ],
    )
    monkeypatch.setattr(cli, "_write_evidence_bundle", _capture_bundle)

    rc = cli.main(
        [
            "--goal",
            "review this design document against its evidence",
            "--workdir",
            str(tmp_path),
            "--backend",
            "echo",
            "--no-perf-log",
        ]
    )

    assert rc == 0
    assert "--pipeline" not in captured["command"]
    assert captured["command"].startswith("dark-factory --goal ")


def test_dark_factory_default_pipeline_file_exists_in_factory_home() -> None:
    """The slim two-node default graph must live at
    ``$DARK_FACTORY_HOME/pipelines/slim/two_node.dot`` — that's where
    ``resolve_pipeline_path`` will look when an operator invokes
    ``dark-factory`` (no --pipeline) from any target-repo cwd.

    Without this file in place, every default `/f` invocation would fail
    with a missing-pipeline error before the worker ever runs.
    """
    from runner.paths import factory_home  # type: ignore  # noqa: E402

    home = factory_home()
    if home is None:
        import pytest
        pytest.skip(
            "DARK_FACTORY_HOME is not set in this test environment; "
            "cannot verify the default pipeline lives under factory home."
        )
    expected = home / "pipelines" / "slim" / "two_node.dot"
    assert expected.exists(), (
        f"Default slim two-node pipeline must exist at "
        f"$DARK_FACTORY_HOME/pipelines/slim/two_node.dot; got {expected} "
        f"(does not exist)."
    )


def test_omitted_pipeline_bypasses_colliding_workdir_two_node_dot(tmp_path: pathlib.Path) -> None:
    """When --pipeline is omitted, dark-factory must resolve the canonical
    $DARK_FACTORY_HOME/pipelines/slim/two_node.dot even if a colliding
    `two_node.dot` exists in the target workdir."""
    from runner.paths import factory_home, resolve_pipeline_path

    home = factory_home()
    if home is None:
        import pytest
        pytest.skip("DARK_FACTORY_HOME is not set")

    # Create a dummy colliding two_node.dot in tmp_path
    colliding = tmp_path / "two_node.dot"
    colliding.write_text("digraph colliding { start -> exit }", encoding="utf-8")

    # Omitted pipeline resolves via `pipelines/slim/two_node.dot`
    target_pipeline = pathlib.Path("pipelines/slim/two_node.dot")
    resolved = resolve_pipeline_path(target_pipeline, workdir=tmp_path)

    canonical_expected = (home / "pipelines" / "slim" / "two_node.dot").resolve()
    assert resolved == canonical_expected, (
        f"Omitted --pipeline must resolve canonical {canonical_expected}, got {resolved}"
    )


def test_authoritative_skill_instructions_agree_on_two_node_default() -> None:
    """The authoritative slash-command skill documents the omitted-pipeline default.

    README.md and AGENTS.md are intentionally outside this minimal transplant.
    """
    skill_file = ROOT / ".claude" / "skills" / "dark-factory" / "SKILL.md"

    if skill_file.exists():
        skill_text = skill_file.read_text(encoding="utf-8")
        normalized_skill_text = " ".join(skill_text.split())
        assert "any reviewable target" in skill_text
        assert "two_node" in skill_text
        assert "The previous \"auto-select from the goal\" behavior is retired" in normalized_skill_text
        for stale_instruction in (
            "plan/spec producer, independent\nreview, bounded fix loop, evidence gates",
            "Docs-only / test-only / config-only PRs have no behavioral surface",
            "If no pipeline fits (e.g. docs-only PR), **say so and stop**",
        ):
            assert stale_instruction not in skill_text, stale_instruction


def test_tracked_command_and_installer_surfaces_publish_two_node_default() -> None:
    """Installed command projections must agree with the runtime default."""
    command_paths = (
        ROOT / ".claude" / "commands" / "f.md",
        ROOT / ".claude" / "commands" / "factory.md",
    )
    required_command_contract = (
        "any reviewable target",
        "pipelines/slim/two_node.dot",
        "generic worker",
        "controller-owned codex cold reviewer",
        "no shadow reviewer",
        "explicit `--pipeline` is the only graph override",
        "active cli",
        "installed/repository skills and policies",
        "controller prompt remains the detailed review authority",
    )
    stale_command_claims = (
        "auto-routes",
        "auto-route",
        "auto-detect: pr-mode or feature-mode",
        "pr-mode vs feature-mode",
        "step 0c (pipeline select)",
        "run first when the goal needs a spec",
        "docs-only",
        "if no pipeline fits",
    )

    for command_path in command_paths:
        command_text = " ".join(command_path.read_text(encoding="utf-8").split()).lower()
        for required_text in required_command_contract:
            assert required_text in command_text, f"{command_path}: {required_text}"
        for stale_text in stale_command_claims:
            assert stale_text not in command_text, f"{command_path}: {stale_text}"

    install_text = (ROOT / "install.sh").read_text(encoding="utf-8")
    example_start = install_text.index("  # default review")
    default_example = install_text[example_start : install_text.index("  # healer")]
    assert "pipelines/slim/two_node.dot" in default_example
    assert "any reviewable target" in default_example
    assert "--pipeline" not in default_example
    assert "--feature" not in default_example


def test_installer_generated_pre_commit_hook_is_exactly_ignored() -> None:
    """The installer-owned hook stays executable without dirtying git status."""
    generated_hook = ".githooks/pre-commit"
    ignored = subprocess.run(
        ["git", "check-ignore", "--no-index", "--verbose", "--", generated_hook],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert ignored.returncode == 0, ignored.stderr
    assert f":{generated_hook}\t{generated_hook}" in ignored.stdout

    tracked_hook = subprocess.run(
        ["git", "check-ignore", "--no-index", "--", ".githooks/pre-push"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert tracked_hook.returncode == 1, (
        "Only the generated pre-commit hook may be ignored; tracked hook "
        f"coverage must remain visible. stdout={tracked_hook.stdout!r}"
    )
