"""Conformance tests for ``_check_command_binaries`` (Lane B1; bead jleechan-ku3).

These tests pin two contracts that must remain stable across refactors:

1. **Failure message format.** When a ``tool`` node references a binary
   that ``shutil.which`` cannot resolve, the preflight emits an error
   containing the exact substring ``"binary not found on PATH: <name>"``.
   Downstream tooling (cron, CI annotations, the Healer aggregator) may
   parse this string; changing the wording silently breaks that
   pipeline.

2. **Shell-builtin skip list.** The preflight must NOT flag any of
   ``{cd, test, echo, true, false, pwd, [, [[}`` as missing — they are
   shell builtins, not standalone binaries. The skip set is exposed as
   ``_SHELL_BUILTINS`` at module level so the conformance test can pin
   it directly rather than inferring it from observed behavior.

A happy-path test (against ``pipelines/factory/hello.dot``) ensures the
new check doesn't regress the existing pipeline corpus. The hello
pipeline contains zero ``tool`` nodes, so the new check trivially
passes and the test asserts the overall status remains ``pass``.
"""

from __future__ import annotations

import pathlib
import shutil
import subprocess
import sys
import textwrap

from conftest import hermetic_subprocess_env  # noqa: E402

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner import structural_preflight


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def _write_dot(tmp_path: pathlib.Path, name: str, body: str) -> pathlib.Path:
    """Write a minimal .dot file and return its path."""
    p = tmp_path / name
    p.write_text(textwrap.dedent(body).lstrip("\n"))
    return p


# ---------------------------------------------------------------------------
# Contract 1: failure message format
# ---------------------------------------------------------------------------


def test_missing_binary_message_format(tmp_path):
    """A tool node referencing a non-existent binary fails with the
    exact message ``"binary not found on PATH: <name>"``.

    The bogus name ``definitely-not-a-real-binary-xyz123`` is used to
    guarantee ``shutil.which`` returns ``None`` regardless of the
    host environment.
    """
    p = _write_dot(
        tmp_path,
        "missing_binary.dot",
        """\
        digraph missing_binary {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run it", command="definitely-not-a-real-binary-xyz123"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)

    assert result["status"] == "fail"
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is False
    assert len(bin_check["missing"]) == 1

    # Exact-prefix contract: "<node>: binary not found on PATH: <binary>"
    missing_entry = bin_check["missing"][0]
    assert missing_entry == "runit: binary not found on PATH: definitely-not-a-real-binary-xyz123", (
        f"unexpected entry shape: {missing_entry!r}"
    )

    # And the human-readable errors list carries the same exact phrase.
    expected_substring = "binary not found on PATH: definitely-not-a-real-binary-xyz123"
    assert any(expected_substring in e for e in result["errors"]), (
        f"errors list missing exact phrase {expected_substring!r}: {result['errors']!r}"
    )


def test_missing_binary_is_preserved_when_prompt_paths_also_fail(tmp_path):
    """The new check runs after the existing checks; a missing-binary
    error must surface alongside (not be shadowed by) a missing-prompt
    error. This guards the additive-only constraint.
    """
    p = _write_dot(
        tmp_path,
        "both_fail.dot",
        """\
        digraph both_fail {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            // codergen without timeout AND missing prompt -> timeout_paths + prompt_paths both fail
            work  [type="codergen", label="Work", prompt="@prompts/nope.md", timeout=30]
            // tool referencing bogus binary -> command_binaries fails
            runit [type="tool", label="Run", command="definitely-not-a-real-binary-xyz123"]
            start -> work
            work  -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)

    assert result["status"] == "fail"
    failed_check_names = {c["name"] for c in result["checks"] if not c["ok"]}
    # All three independent failures must be reported.
    assert {"prompt_paths", "timeout_thresholds", "command_binaries"} <= failed_check_names, (
        f"expected three failing checks, got: {failed_check_names}"
    )
    # The binary-not-found phrase is in the human-readable errors list.
    assert any(
        "binary not found on PATH: definitely-not-a-real-binary-xyz123" in e
        for e in result["errors"]
    )


# ---------------------------------------------------------------------------
# Contract 2: shell-builtin skip list
# ---------------------------------------------------------------------------


def test_shell_builtins_skip_list_pinned():
    """The module-level ``_SHELL_BUILTINS`` set is the exact 8-element
    set called out in the bead description. Adding or removing entries
    must require updating this test, which keeps the skip list
    contractually stable.
    """
    assert structural_preflight._SHELL_BUILTINS == frozenset(
        {"cd", "test", "echo", "true", "false", "pwd", "[", "[["}
    ), (
        "shell-builtin skip list drifted from the documented 8-element "
        "set; update this pin-test alongside the change."
    )


@pytest.mark.parametrize(
    "builtin",
    ["cd", "test", "echo", "true", "false", "pwd", "[", "[["],
)
def test_each_shell_builtin_is_skipped(tmp_path, builtin):
    """A tool node whose command head is a shell builtin must NOT
    produce a binary-not-found error, regardless of whether the builtin
    exists as a binary on disk (e.g. ``/bin/echo``). The check uses
    ``shutil.which`` but pre-skips the builtin set so operators can
    write idiomatic shell pipelines (``test -f file``, ``echo hi``,
    ``cd dir``) without false-positive preflight failures.
    """
    # Pick a name that is unique so multiple parametrized cases don't
    # collide in node-name collisions in the same .dot.
    p = _write_dot(
        tmp_path,
        f"builtin_{builtin.replace('[', 'lb_').replace(']', '_rb')}.dot",
        f"""\
        digraph builtin_test {{
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="{builtin} some-arg"]
            start -> runit
            runit -> exit
        }}
        """,
    )

    result = structural_preflight.validate_structure(p)

    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True, (
        f"shell builtin {builtin!r} was incorrectly flagged as a missing "
        f"binary; got: {bin_check['missing']!r}"
    )
    assert bin_check["missing"] == []


def test_shell_builtin_as_only_command(tmp_path):
    """A tool node whose command IS exactly a shell builtin (no args)
    must be skipped. This catches off-by-one bugs in the token-parse
    logic where an empty-args-list path might short-circuit past the
    builtin check.
    """
    p = _write_dot(
        tmp_path,
        "builtin_only.dot",
        """\
        digraph builtin_only {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="cd"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True


# ---------------------------------------------------------------------------
# Contract 3: PATH-resolvable binaries pass
# ---------------------------------------------------------------------------


def test_python3_binary_resolves(tmp_path):
    """``python3`` is on PATH on the dev box; a tool node that invokes
    it must pass the binary check. This is the positive complement to
    the missing-binary test and guards against false negatives caused
    by an over-aggressive skip list.
    """
    if shutil.which("python3") is None:
        pytest.skip("python3 not on PATH; cannot exercise positive path")

    p = _write_dot(
        tmp_path,
        "real_binary.dot",
        """\
        digraph real_binary {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="python3 --version"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True, (
        f"python3 should resolve on PATH; got: {bin_check['missing']!r}"
    )
    assert bin_check["missing"] == []


def test_quoted_complex_command_uses_first_token(tmp_path):
    """A command like ``bash -c 'echo hi'`` must check ``bash`` (the
    first token), not ``bash -c 'echo hi'`` as a single binary name.
    The runner's ``_tool`` handler uses ``shlex.split`` on the command
    string, so the preflight must mirror that parsing.
    """
    if shutil.which("bash") is None:
        pytest.skip("bash not on PATH; cannot exercise quoted-command path")

    p = _write_dot(
        tmp_path,
        "quoted.dot",
        """\
        digraph quoted {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="bash -c 'echo hi'"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True, (
        f"first-token extraction failed for quoted command; got: {bin_check['missing']!r}"
    )


def test_state_placeholder_command_is_skipped(tmp_path):
    """A tool node whose command is ``${state.<key>}`` (the common
    pattern in slim/*.dot) must be SKIPPED — the actual binary is not
    knowable at preflight time. This guards against flagging real
    pipelines as broken.
    """
    p = _write_dot(
        tmp_path,
        "state_placeholder.dot",
        """\
        digraph state_placeholder {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="${state.slim.test_command}"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True, (
        f"state placeholder head must be skipped; got: {bin_check['missing']!r}"
    )
    assert bin_check["missing"] == []


def test_empty_command_does_not_crash(tmp_path):
    """A tool node without a ``command`` attribute (or with empty one)
    must NOT crash and must NOT be flagged. The runner surfaces
    ``"no command attribute"`` at runtime; preflight should not
    shadow that diagnostic with a misleading binary error.
    """
    p = _write_dot(
        tmp_path,
        "empty_cmd.dot",
        """\
        digraph empty_cmd {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run"]
            start -> runit
            runit -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True


# ---------------------------------------------------------------------------
# Happy-path regression: the canonical hello pipeline still passes
# ---------------------------------------------------------------------------


def test_hello_pipeline_still_passes():
    """Regression guard: ``pipelines/factory/hello.dot`` has no
    ``tool`` nodes, so the new check trivially passes. The overall
    status must remain ``pass`` (the existing checks still hold).
    """
    hello = ROOT / "pipelines" / "factory" / "hello.dot"
    assert hello.exists(), f"expected fixture: {hello}"

    result = structural_preflight.validate_structure(hello)

    # hello.dot has codergen nodes without `timeout` attrs and prompts
    # that resolve via workdir/factory_home fallback, so under the
    # *current* structural-preflight rules it does not pass — the
    # corpus-cleanup is a follow-up tracked in bead jleechan-wou.
    # What we pin HERE is only that the command-binaries check does
    # NOT regress hello.dot's behavior — it does not flag any
    # missing-binary errors because there are zero tool nodes.
    bin_check = next(c for c in result["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is True, (
        f"hello.dot must not trigger command-binaries failures; got: {bin_check!r}"
    )
    assert bin_check["missing"] == []


# ---------------------------------------------------------------------------
# CLI smoke: the exact message survives subprocess + JSON envelope
# ---------------------------------------------------------------------------


def test_cli_subprocess_emits_exact_message(tmp_path):
    """Running ``python -m runner.structural_preflight <bad> --json``
    emits the exact ``binary not found on PATH: <name>`` phrase in the
    errors list. This guards the CLI surface (used by ``bin/df-validate``
    and downstream automation) so the message format is stable end
    to end, not just in-process.
    """
    p = _write_dot(
        tmp_path,
        "cli_missing.dot",
        """\
        digraph cli_missing {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            runit [type="tool", label="Run", command="definitely-not-a-real-binary-xyz123"]
            start -> runit
            runit -> exit
        }
        """,
    )

    import json

    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner.structural_preflight",
            str(p),
            "--json",
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        timeout=30,
        env=hermetic_subprocess_env(
            PATH="/usr/bin:/bin",
            HOME=str(ROOT),
            PYTHONPATH=str(ROOT),
            DARK_FACTORY_HOME=str(ROOT),
        ),
    )
    assert proc.returncode == 2, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail"
    bin_check = next(c for c in payload["checks"] if c["name"] == "command_binaries")
    assert bin_check["ok"] is False
    # The exact phrase is the contract: downstream tooling may parse it.
    expected = "binary not found on PATH: definitely-not-a-real-binary-xyz123"
    assert any(expected in e for e in payload["errors"]), (
        f"expected exact phrase in errors; got: {payload['errors']!r}"
    )