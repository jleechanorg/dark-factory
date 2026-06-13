"""Unit tests for validate_pipeline in runner/parser."""

from __future__ import annotations

import pathlib
import sys
import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from runner.parser import validate_pipeline


def _write_dot(tmp_path: pathlib.Path, name: str, body: str) -> pathlib.Path:
    p = tmp_path / name
    p.write_text(body)
    return p


def test_validate_pipeline_prompt_absolute(tmp_path):
    # Setup prompt file
    prompt_file = tmp_path / "my_prompt.md"
    prompt_file.write_text("# Hello")
    
    # Write a pipeline referencing absolute path
    dot_content = f"""
    digraph test {{
        start [shape=Mdiamond]
        exit [shape=Msquare]
        work [type="codergen", prompt="{prompt_file.resolve()}", timeout=120]
        start -> work
        work -> exit
    }}
    """
    p = _write_dot(tmp_path, "test_abs.dot", dot_content)
    
    graph, diagnostics = validate_pipeline(p)
    assert graph is not None
    # No DF_MISSING_PROMPT diagnostics should be present
    assert not any(d["code"] == "DF_MISSING_PROMPT" for d in diagnostics)


def test_validate_pipeline_prompt_dot_relative(tmp_path):
    # Setup prompt relative to dot file
    prompts_dir = tmp_path / "prompts"
    prompts_dir.mkdir()
    prompt_file = prompts_dir / "my_prompt.md"
    prompt_file.write_text("# Hello")
    
    # Write a pipeline referencing relative path
    dot_content = """
    digraph test {
        start [shape=Mdiamond]
        exit [shape=Msquare]
        work [type="codergen", prompt="@prompts/my_prompt.md", timeout=120]
        start -> work
        work -> exit
    }
    """
    p = _write_dot(tmp_path, "test_rel.dot", dot_content)
    
    graph, diagnostics = validate_pipeline(p)
    assert graph is not None
    assert not any(d["code"] == "DF_MISSING_PROMPT" for d in diagnostics)


def test_validate_pipeline_prompt_factory_home_relative(tmp_path, monkeypatch):
    # Setup factory home
    home = tmp_path / "factory_home"
    prompts_dir = home / "prompts"
    prompts_dir.mkdir(parents=True)
    prompt_file = prompts_dir / "my_prompt.md"
    prompt_file.write_text("# Hello")
    
    # Set env var
    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))
    
    # Write a pipeline referencing relative path to a separate temp directory
    dot_content = """
    digraph test {
        start [shape=Mdiamond]
        exit [shape=Msquare]
        work [type="codergen", prompt="@prompts/my_prompt.md", timeout=120]
        start -> work
        work -> exit
    }
    """
    # Create the pipeline elsewhere so it is not relative to pipeline parent
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    p = _write_dot(elsewhere, "test_home_rel.dot", dot_content)
    
    graph, diagnostics = validate_pipeline(p)
    assert graph is not None
    assert not any(d["code"] == "DF_MISSING_PROMPT" for d in diagnostics)


def test_validate_pipeline_prompt_missing(tmp_path):
    # Write a pipeline referencing relative path that does not exist anywhere
    dot_content = """
    digraph test {
        start [shape=Mdiamond]
        exit [shape=Msquare]
        work [type="codergen", prompt="@prompts/non_existent.md", timeout=120]
        start -> work
        work -> exit
    }
    """
    p = _write_dot(tmp_path, "test_missing.dot", dot_content)
    
    graph, diagnostics = validate_pipeline(p)
    # Even if there is a diagnostic, the graph is parsed and returned
    assert graph is not None
    # We should have a DF_MISSING_PROMPT diagnostic
    missing_prompts = [d for d in diagnostics if d["code"] == "DF_MISSING_PROMPT"]
    assert len(missing_prompts) == 1
    assert "codergen prompt file not found" in missing_prompts[0]["message"]
