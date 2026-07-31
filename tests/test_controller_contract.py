"""Tests for the deterministic ``controller_contract.inspect`` API and CLI.

Contract (issue / design: docs/superpowers/specs/2026-07-30-slim-feature-repo-factory-design.md):

* Explicit ``--profile`` is mandatory; ``inspect`` does not infer.
* Loads ``<workdir>/dark-factory/factory.toml`` and
  ``<workdir>/dark-factory/pipelines/factory_<profile>.dot``.
* Strictly validates only the supported v1 manifest keys, action keys,
  and profile keys (no silent interpretation of unknown keys).
* Rejects unsafe shell, path traversal / out-of-root paths, environment-
  driven execution, and any target-controlled holdout path or content.
* Validates referenced actions exist, receipt classes match the manifest,
  the single ``repair_passes=1`` bound holds, and the exact approved four-
  node slim / six-node feature topology (names, edges, ``max_visits``).
* Returns deterministic JSON: explicit profile, absolute manifest + pipeline
  paths, SHA-256 digests of both source files, expanded ordered action plan
  with receipt classes, and ``execution_enabled: false``.
* On any contract violation: fail-closed with deterministic machine-
  readable JSON and nonzero exit. Never call a model, execute an action,
  launch a writer, or claim ownership / fencing / checkpoint capability.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys

import pytest


SLIM_DOT = """\
digraph factory_slim {
    start [shape=Mdiamond];
    work [type="codergen", max_visits="2", max_retries="0"];
    verify [type="factory_verify", max_retries="0"];
    exit [shape=Msquare];

    start -> work;
    work -> verify;
    verify -> exit [condition="outcome=success"];
    verify -> work [condition="outcome=failure"];
}
"""

FEATURE_DOT = """\
digraph factory_feature {
    start [shape=Mdiamond];
    spec [type="codergen", max_visits="1", max_retries="0"];
    build [type="codergen", max_visits="2", max_retries="0"];
    prove [type="factory_prove", max_retries="0"];
    independent_review [
        type="factory_independent_review",
        prefer_adversarial="true",
        max_retries="0"
    ];
    exit [shape=Msquare];

    start -> spec;
    spec -> build;
    build -> prove;
    prove -> independent_review;
    independent_review -> exit [condition="outcome=success"];
    prove -> build [condition="outcome=failure"];
    independent_review -> build [condition="outcome=failure"];
}
"""

VALID_MANIFEST = """\
schema_version = 1
controller_api_version = 1

[actions.test]
kind = "tool"
argv = ["./run_tests.sh"]
cwd = "."
timeout_seconds = 600
receipt_class = "test"

[actions.holdout]
kind = "holdout_eval"
timeout_seconds = 600
receipt_class = "behavioral_proof"

[actions.evidence]
kind = "slash"
name = "es"
timeout_seconds = 600
receipt_class = "evidence"

[actions.review]
kind = "slash"
name = "er"
timeout_seconds = 600
receipt_class = "independent_review"

[controller]
infra_retry_attempts = 1
suspension_max_age_seconds = 86400
artifact_retention_days = 30

[profiles.slim]
verify = ["test", "holdout", "evidence", "review"]
repair_passes = 1

[profiles.feature]
prove = ["test", "holdout", "evidence"]
review = ["review"]
repair_passes = 1
"""


def _write_repo(workdir: pathlib.Path, profile: str, manifest_toml: str, dot_src: str) -> None:
    factory_dir = workdir / "dark-factory"
    pipelines_dir = factory_dir / "pipelines"
    pipelines_dir.mkdir(parents=True, exist_ok=True)
    (factory_dir / "factory.toml").write_text(manifest_toml, encoding="utf-8")
    (pipelines_dir / f"factory_{profile}.dot").write_text(dot_src, encoding="utf-8")


def test_inspect_slim_returns_deterministic_json(tmp_path: pathlib.Path) -> None:
    """Public seam — ``controller_contract.inspect(workdir, profile='slim')``.

    Validates the slim profile against the v1 manifest; returns deterministic
    JSON containing the required keys and ``execution_enabled: false``.
    """
    from runner import controller_contract

    _write_repo(tmp_path, "slim", VALID_MANIFEST, SLIM_DOT)
    result = controller_contract.inspect(workdir=tmp_path, profile="slim")
    payload = json.loads(result)

    assert payload["profile"] == "slim"
    assert payload["manifest_path"] == str((tmp_path / "dark-factory" / "factory.toml").resolve())
    assert payload["pipeline_path"] == str(
        (tmp_path / "dark-factory" / "pipelines" / "factory_slim.dot").resolve()
    )
    assert payload["execution_enabled"] is False
    assert payload["status"] == "pass"

    # SHA-256 digests should match the source files we just wrote.
    manifest_bytes = (tmp_path / "dark-factory" / "factory.toml").read_bytes()
    pipeline_bytes = (tmp_path / "dark-factory" / "pipelines" / "factory_slim.dot").read_bytes()
    assert payload["manifest_sha256"] == hashlib.sha256(manifest_bytes).hexdigest()
    assert payload["pipeline_sha256"] == hashlib.sha256(pipeline_bytes).hexdigest()

    # Slim must list four ordered action IDs with their receipt classes.
    plan = payload["action_plan"]
    assert [a["action_id"] for a in plan] == ["test", "holdout", "evidence", "review"]
    assert [a["receipt_class"] for a in plan] == [
        "test",
        "behavioral_proof",
        "evidence",
        "independent_review",
    ]
    # Repair bound is preserved exactly once in v1.
    assert payload["repair_passes"] == 1


def test_inspect_feature_returns_six_node_topology(tmp_path: pathlib.Path) -> None:
    from runner import controller_contract

    _write_repo(tmp_path, "feature", VALID_MANIFEST, FEATURE_DOT)
    result = controller_contract.inspect(workdir=tmp_path, profile="feature")
    payload = json.loads(result)

    assert payload["profile"] == "feature"
    assert payload["status"] == "pass"
    node_names = [n["name"] for n in payload["topology"]["nodes"]]
    assert sorted(node_names) == sorted(
        ["start", "spec", "build", "prove", "independent_review", "exit"]
    )

    # Feature plan has two ordered stages: prove then review.
    assert payload["action_plan"]["prove"] == [
        {"action_id": "test", "receipt_class": "test"},
        {"action_id": "holdout", "receipt_class": "behavioral_proof"},
        {"action_id": "evidence", "receipt_class": "evidence"},
    ]
    assert payload["action_plan"]["review"] == [
        {"action_id": "review", "receipt_class": "independent_review"},
    ]


def test_inspect_rejects_unknown_profile(tmp_path: pathlib.Path) -> None:
    from runner import controller_contract

    _write_repo(tmp_path, "slim", VALID_MANIFEST, SLIM_DOT)
    with pytest.raises(ValueError, match="profile"):
        controller_contract.inspect(workdir=tmp_path, profile="bogus")


def test_inspect_rejects_path_traversal_action_path(tmp_path: pathlib.Path) -> None:
    """Unsafe manifest — an action resolves a path that escapes the workdir."""
    from runner import controller_contract

    bad_manifest = VALID_MANIFEST + (
        '\n[actions.escape]\nkind = "tool"\n'
        'argv = ["cat"]\ncwd = "../outside"\n'
        'timeout_seconds = 60\n'
        'receipt_class = "test"\n\n'
        '[profiles.slim]\nverify = ["escape"]\nrepair_passes = 1\n'
    )
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_shell_command_in_tool_action(tmp_path: pathlib.Path) -> None:
    """Unsafe manifest — a tool action uses a shell command string."""
    from runner import controller_contract

    bad_manifest = VALID_MANIFEST.replace(
        'argv = ["./run_tests.sh"]', 'argv = ["/bin/sh -c rm -rf /"]'
    )
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_environment_driven_execution(tmp_path: pathlib.Path) -> None:
    """Unsafe manifest — argv references environment / shell interpolation."""
    from runner import controller_contract

    bad_manifest = VALID_MANIFEST.replace(
        'argv = ["./run_tests.sh"]', 'argv = ["$SHELL"]'
    )
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_unknown_action_in_plan(tmp_path: pathlib.Path) -> None:
    """Profile references an action that is not declared — fail closed."""
    from runner import controller_contract

    bad_manifest = (
        'schema_version = 1\ncontroller_api_version = 1\n\n'
        '[actions.test]\nkind = "tool"\nargv = ["./run_tests.sh"]\n'
        'cwd = "."\ntimeout_seconds = 60\nreceipt_class = "test"\n\n'
        '[profiles.slim]\nverify = ["test", "ghost"]\nrepair_passes = 1\n'
    )
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError, match="ghost"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_repair_passes_greater_than_one(tmp_path: pathlib.Path) -> None:
    """The v1 contract allows exactly one repair pass per profile."""
    from runner import controller_contract

    bad_manifest = VALID_MANIFEST.replace("repair_passes = 1", "repair_passes = 2")
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError, match="repair_passes"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_unknown_top_level_manifest_key(tmp_path: pathlib.Path) -> None:
    """Unknown manifest top-level keys must fail closed — never auto-interpreted."""
    from runner import controller_contract

    bad_manifest = (
        'schema_version = 1\n'
        'controller_api_version = 1\n'
        'sneaky_extra_global = "value"\n\n'
        '[actions.test]\n'
        'kind = "tool"\n'
        'argv = ["./run_tests.sh"]\n'
        'cwd = "."\n'
        'timeout_seconds = 60\n'
        'receipt_class = "test"\n\n'
        '[profiles.slim]\n'
        'verify = ["test"]\n'
        'repair_passes = 1\n'
    )
    _write_repo(tmp_path, "slim", bad_manifest, SLIM_DOT)
    with pytest.raises(ValueError, match="unknown"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_feature_when_slim_dot_provided(tmp_path: pathlib.Path) -> None:
    """Profile ↔ graph mismatch — feature profile cannot load slim dot."""
    from runner import controller_contract

    _write_repo(tmp_path, "slim", VALID_MANIFEST, SLIM_DOT)
    with pytest.raises(ValueError):
        controller_contract.inspect(workdir=tmp_path, profile="feature")


def test_cli_dispatch_emits_json_and_nonzero_exit_on_error(tmp_path: pathlib.Path) -> None:
    """End-to-end CLI invocation: succeeds on a valid slim repo, exits 2 on missing manifest."""
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    py = sys.executable
    # Bad repo (no manifest at all) — must fail closed with nonzero exit.
    proc = subprocess.run(
        [py, "-m", "runner", "controller", "inspect", "--workdir", str(tmp_path), "--profile", "slim"],
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    assert proc.returncode != 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail"
    assert payload["profile"] == "slim"
    assert payload["execution_enabled"] is False


def test_cli_dispatch_succeeds_on_valid_slim_repo(tmp_path: pathlib.Path) -> None:
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    _write_repo(tmp_path, "slim", VALID_MANIFEST, SLIM_DOT)
    py = sys.executable
    proc = subprocess.run(
        [py, "-m", "runner", "controller", "inspect", "--workdir", str(tmp_path), "--profile", "slim"],
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "pass"
    assert payload["profile"] == "slim"
    assert payload["execution_enabled"] is False


def _run_cli(workdir: pathlib.Path, *, profile: str = "slim",
             manifest: str = VALID_MANIFEST, dot: str = SLIM_DOT) -> subprocess.CompletedProcess[str]:
    """Invoke the controller inspect subcommand and return the completed process."""
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    _write_repo(workdir, profile, manifest, dot)
    return subprocess.run(
        [sys.executable, "-m", "runner", "controller", "inspect",
         "--workdir", str(workdir), "--profile", profile],
        cwd=str(repo_root), capture_output=True, text=True, check=False, timeout=30,
    )


@pytest.mark.parametrize("bad_max_retries", ["true", "1.5"])
def test_dot_max_retries_translates_to_contract_error(
    tmp_path: pathlib.Path, bad_max_retries: str,
) -> None:
    from runner import controller_contract
    bad_dot = SLIM_DOT.replace(
        'max_visits="2", max_retries="0"',
        f'max_visits="2", max_retries="{bad_max_retries}"',
    )
    _write_repo(tmp_path, "slim", VALID_MANIFEST, bad_dot)
    with pytest.raises(controller_contract.ContractError, match="max_retries"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


@pytest.mark.parametrize("bad_max_retries", ["true", "1.5"])
def test_cli_dot_max_retries_fail_closed_no_traceback(
    tmp_path: pathlib.Path, bad_max_retries: str,
) -> None:
    bad_dot = SLIM_DOT.replace(
        'max_visits="2", max_retries="0"',
        f'max_visits="2", max_retries="{bad_max_retries}"',
    )
    proc = _run_cli(tmp_path, dot=bad_dot)
    assert proc.returncode == 2 and proc.stderr == "", (proc.stdout, proc.stderr)
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail"
    assert payload["profile"] == "slim"
    assert payload["execution_enabled"] is False
    assert "max_retries" in payload["error"]


def test_malformed_toml_translates_to_contract_error(tmp_path: pathlib.Path) -> None:
    from runner import controller_contract
    bad = VALID_MANIFEST.replace(
        "[actions.test]\nkind = \"tool\"",
        "[actions.test  # missing closing bracket\nkind = \"tool\"",
    )
    _write_repo(tmp_path, "slim", bad, SLIM_DOT)
    with pytest.raises(controller_contract.ContractError, match="TOML"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_cli_malformed_toml_fail_closed_no_traceback(tmp_path: pathlib.Path) -> None:
    bad = VALID_MANIFEST.replace(
        "[actions.test]\nkind = \"tool\"",
        "[actions.test  # missing closing bracket\nkind = \"tool\"",
    )
    proc = _run_cli(tmp_path, manifest=bad)
    assert proc.returncode == 2 and proc.stderr == "", (proc.stdout, proc.stderr)
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail" and payload["execution_enabled"] is False
    assert payload["profile"] == "slim"


def test_cli_pass_output_is_byte_for_byte_deterministic(tmp_path: pathlib.Path) -> None:
    """Repeated CLI calls on identical inputs produce identical stdout."""
    outs = [_run_cli(tmp_path).stdout for _ in range(3)]
    assert outs[0] == outs[1] == outs[2]


def test_inspect_rejects_reviewer_with_test_class(tmp_path: pathlib.Path) -> None:
    """Stage-required sequence: reviewer's receipt_class must be independent_review."""
    from runner import controller_contract
    bad = VALID_MANIFEST.replace(
        'receipt_class = "independent_review"', 'receipt_class = "test"', 1,
    )
    _write_repo(tmp_path, "slim", bad, SLIM_DOT)
    with pytest.raises(controller_contract.ContractError, match="receipt_class"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_swapped_holdout_class(tmp_path: pathlib.Path) -> None:
    """Ordered sequence: holdout action must declare behavioral_proof, not evidence."""
    from runner import controller_contract
    bad = VALID_MANIFEST.replace(
        'kind = "holdout_eval"\ntimeout_seconds = 600\nreceipt_class = "behavioral_proof"',
        'kind = "holdout_eval"\ntimeout_seconds = 600\nreceipt_class = "evidence"',
    )
    _write_repo(tmp_path, "slim", bad, SLIM_DOT)
    with pytest.raises(controller_contract.ContractError, match="receipt_class"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_accepts_alternate_action_ids_with_required_classes(tmp_path: pathlib.Path) -> None:
    """Alternate action IDs preserve the required ordered receipt_class sequence."""
    from runner import controller_contract
    alt = (
        VALID_MANIFEST
        .replace("[actions.test]", "[actions.alpha_test]")
        .replace("argv = [\"./run_tests.sh\"]", "argv = [\"./alpha.sh\"]")
        .replace("[actions.holdout]", "[actions.beta_holdout]")
        .replace("[actions.evidence]", "[actions.gamma_evidence]")
        .replace("[actions.review]", "[actions.delta_review]")
        .replace(
            "verify = [\"test\", \"holdout\", \"evidence\", \"review\"]",
            "verify = [\"alpha_test\", \"beta_holdout\", \"gamma_evidence\", \"delta_review\"]",
        )
    )
    _write_repo(tmp_path, "slim", alt, SLIM_DOT)
    payload = json.loads(controller_contract.inspect(workdir=tmp_path, profile="slim"))
    plan = payload["action_plan"]
    assert [a["action_id"] for a in plan] == [
        "alpha_test", "beta_holdout", "gamma_evidence", "delta_review",
    ]
    assert [a["receipt_class"] for a in plan] == [
        "test", "behavioral_proof", "evidence", "independent_review",
    ]


def test_inspect_rejects_feature_review_with_test_class(tmp_path: pathlib.Path) -> None:
    """Feature review stage must reference an action with receipt_class=independent_review."""
    from runner import controller_contract
    bad = (
        VALID_MANIFEST
        .replace(
            'receipt_class = "independent_review"', 'receipt_class = "test"', 1,
        )
        .replace(
            '[profiles.slim]\nverify = ["test", "holdout", "evidence", "review"]\nrepair_passes = 1\n\n',
            "",
        )
    )
    _write_repo(tmp_path, "feature", bad, FEATURE_DOT)
    with pytest.raises(controller_contract.ContractError, match="receipt_class"):
        controller_contract.inspect(workdir=tmp_path, profile="feature")


def test_inspect_rejects_tool_with_independent_review_class(tmp_path: pathlib.Path) -> None:
    """Kind→class compatibility: tool actions cannot emit independent_review."""
    from runner import controller_contract
    bad = VALID_MANIFEST.replace(
        'kind = "tool"\nargv = ["./run_tests.sh"]\ncwd = "."\ntimeout_seconds = 600\nreceipt_class = "test"',
        'kind = "tool"\nargv = ["./run_tests.sh"]\ncwd = "."\ntimeout_seconds = 600\nreceipt_class = "independent_review"',
        1,
    )
    _write_repo(tmp_path, "slim", bad, SLIM_DOT)
    with pytest.raises(controller_contract.ContractError, match="receipt_class"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")


def test_inspect_rejects_holdout_eval_with_test_class(tmp_path: pathlib.Path) -> None:
    """Kind→class compatibility: holdout_eval actions cannot emit test."""
    from runner import controller_contract
    bad = VALID_MANIFEST.replace(
        'kind = "holdout_eval"\ntimeout_seconds = 600\nreceipt_class = "behavioral_proof"',
        'kind = "holdout_eval"\ntimeout_seconds = 600\nreceipt_class = "test"',
    )
    _write_repo(tmp_path, "slim", bad, SLIM_DOT)
    with pytest.raises(controller_contract.ContractError, match="receipt_class"):
        controller_contract.inspect(workdir=tmp_path, profile="slim")
