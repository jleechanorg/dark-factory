"""CLI contracts for the supported MiniMax coder backend."""

from __future__ import annotations

import pytest


def test_runner_cli_accepts_minimax_backend(capsys):
    from runner.__main__ import main

    with pytest.raises(SystemExit) as exc:
        main(["--backend", "minimax", "--help"])

    assert exc.value.code == 0
    assert "--backend" in capsys.readouterr().out
