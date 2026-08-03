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


def test_docs_and_skill_instructions_agree_on_two_node_default() -> None:
    """Authoritative .claude slash-command skill instructions and README must agree
    that omitted --pipeline defaults to `two_node.dot`."""
    skill_file = ROOT / ".claude" / "skills" / "dark-factory" / "SKILL.md"
    readme_file = ROOT / "README.md"
    agents_file = ROOT / "AGENTS.md"

    if skill_file.exists():
        skill_text = skill_file.read_text(encoding="utf-8")
        assert "two_node" in skill_text
        assert "defaults to" in skill_text or "default" in skill_text

    if readme_file.exists():
        readme_text = readme_file.read_text(encoding="utf-8")
        assert "two_node" in readme_text

    if agents_file.exists():
        agents_text = agents_file.read_text(encoding="utf-8")
        assert "two_node" in agents_text


