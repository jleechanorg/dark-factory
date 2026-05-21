"""Hidden test suite for the roman benchmark.

This file is the SEALED holdout. The implementing agent's prompts MUST NOT
reference this path. The agent may run pytest against it via the `verify`
pipeline step, but the test source is hidden from the planning/implement
prompts to mirror the Attractor pattern of evaluation isolation.
"""
from __future__ import annotations

import importlib

import pytest

# Import the module under test (NOT the symbol) so we can also assert
# module-level attributes like __version__ that aren't part of `to_roman`'s
# signature but are still part of the required contract.
roman = importlib.import_module("df_demo3.roman")


# -- 1. Behavioural conversion tests ---------------------------------------

@pytest.mark.parametrize(
    "n, expected",
    [
        (1, "I"),
        (2, "II"),
        (3, "III"),
        (4, "IV"),
        (5, "V"),
        (9, "IX"),
        (10, "X"),
        (14, "XIV"),
        (40, "XL"),
        (49, "XLIX"),
        (90, "XC"),
        (94, "XCIV"),
        (100, "C"),
        (400, "CD"),
        (500, "D"),
        (900, "CM"),
        (1000, "M"),
        (1994, "MCMXCIV"),
        (3999, "MMMCMXCIX"),
    ],
)
def test_to_roman_canonical(n: int, expected: str) -> None:
    assert roman.to_roman(n) == expected


# -- 2. Hidden contract: module-level __version__ string -------------------
# This is the forcing function for the dark-factory fix loop: the spec does
# NOT mention __version__, so a correct-but-incomplete first implementation
# will fail this assertion and route through the `fix` node.

def test_module_has_version_string() -> None:
    assert hasattr(roman, "__version__"), (
        "module df_demo3.roman is missing a module-level __version__ "
        "constant — add `__version__ = \"1.0.0\"` to the module"
    )


def test_module_version_value() -> None:
    assert roman.__version__ == "1.0.0", (
        f"df_demo3.roman.__version__ should equal '1.0.0', got "
        f"{roman.__version__!r}"
    )
