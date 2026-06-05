"""Catalog integrity + pre-run path validation tests for workflow_graphgen."""

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from benchmarks.workflow_graphgen.catalog import (  # noqa: E402
    VOCABULARY,
    CatalogError,
    catalog_prompt_paths,
    load_catalog,
    validate_node_prompts,
)


def test_catalog_covers_all_eight_vocab_types_and_files_exist():
    data = load_catalog()  # raises if any type missing or any file absent
    assert set(data["prompts"]) == set(VOCABULARY)
    assert len(VOCABULARY) == 8


def test_validate_node_prompts_accepts_catalog_paths():
    approved = sorted(catalog_prompt_paths())
    # Both bare and '@'-prefixed refs must validate.
    validate_node_prompts(approved)
    validate_node_prompts(["@" + p for p in approved])


def test_validate_node_prompts_rejects_uncatalogued_path():
    with pytest.raises(CatalogError):
        validate_node_prompts(["prompts/catalog/plan.md", "prompts/not_in_catalog.md"])


def test_validate_node_prompts_rejects_refactor_research_stack_smoke_typos():
    # The whole point of the catalog: refactor/research/stack_smoke must resolve.
    for vocab in ("refactor", "research", "stack_smoke"):
        rel = load_catalog()["prompts"][vocab]
        validate_node_prompts([rel])
        # A near-miss path is rejected.
        with pytest.raises(CatalogError):
            validate_node_prompts([rel.replace(".md", "_missing.md")])
