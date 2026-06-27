"""Regression tests for ``runner.handler_holdout`` emulator-backend detection.

The audit on 2026-06-27 (lane E / jleechan-je5) found that
``runner/handler_holdout.py`` hardcoded Firebase as the universal emulator
assumption: it always prepended ``/opt/homebrew/opt/java`` to ``PATH``, always
set ``JAVA_HOME``, and always stripped GCP credentials. The fix introduces
``_detect_emulator_backend`` which discovers the backend from a manifest and
gates the Firebase-only side effects on ``sys.platform == "darwin"`` and the
discovered backend == ``"firebase"``.

These tests pin that contract:

  * A fibonacci worktree (no firebase.json, no dark-factory.yaml, no
    Makefile ``run:`` target) is detected as ``"none"`` — no firebase, no
    Homebrew Java, no GCP-strip.
  * A worktree with ``dark-factory.yaml: emulator.kind: firebase`` is
    detected as ``"firebase"`` even when ``firebase.json`` is absent.
  * A worktree with ``dark-factory.yaml: emulator.kind: make`` is detected
    as ``"make"`` and never invokes the firebase-specific helpers.
  * The Homebrew Java injection helper is a no-op on non-darwin platforms
    and when the path does not exist.
  * The GCP cred-strip helper only removes the Firebase-specific vars and
    leaves other env alone.
  * The full ``_holdout_eval`` flow against a fibonacci impl never spawns
    ``firebase``, ``make run``, or ``lsof`` against the firebase ports.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

# Import order matters: ``runner.handler_holdout`` does
# ``import runner.handlers`` at module top, and ``runner.handlers`` re-exports
# from ``runner.handler_holdout`` — a circular import. Resolving through the
# shim entry-point first lets Python fully populate both modules so the
# subsequent direct import from ``runner.handler_holdout`` is just a cache hit.
from runner.handlers import _holdout_eval  # noqa: F401  (prime the cycle)
from runner.handler_core import Context
from runner.handler_holdout import (
    _detect_emulator_backend,
    _inject_firebase_java,
    _strip_firebase_gcp_creds,
)
from runner.parser import Node

ROOT = pathlib.Path(__file__).resolve().parent.parent


# ---------------------------------------------------------------------------
# _detect_emulator_backend
# ---------------------------------------------------------------------------


def test_detect_returns_none_for_fibonacci_worktree(tmp_path):
    """A pure-Python fibonacci worktree has no emulator backend."""
    # Mirror benchmarks/fibonacci/starter — just a Python file.
    (tmp_path / "fib.py").write_text("def fib(n: int) -> int:\n    return 0\n")

    assert _detect_emulator_backend(tmp_path) == "none"


def test_detect_prefers_dark_factory_yaml_manifest(tmp_path):
    """Manifest kind wins over the firebase.json fallback."""
    (tmp_path / "firebase.json").write_text("{}")
    (tmp_path / "dark-factory.yaml").write_text(
        "emulator:\n  kind: make\n  start_command: 'make run'\n"
    )

    assert _detect_emulator_backend(tmp_path) == "make"


def test_detect_recognises_firebase_json(tmp_path):
    """The historical amazon-clone convention: firebase.json == firebase."""
    (tmp_path / "firebase.json").write_text('{"emulators": {}}')

    assert _detect_emulator_backend(tmp_path) == "firebase"


def test_detect_recognises_makefile_run_target(tmp_path):
    """A Makefile with a `run:` target is a make backend."""
    (tmp_path / "Makefile").write_text("run:\n\tpython -m http.server\n")

    assert _detect_emulator_backend(tmp_path) == "make"


def test_detect_recognises_package_json_start_script(tmp_path):
    """A package.json with a ``start`` script is a node_start backend."""
    import json as _json

    (tmp_path / "package.json").write_text(
        _json.dumps({"scripts": {"start": "node server.js"}})
    )

    assert _detect_emulator_backend(tmp_path) == "node_start"


def test_detect_handles_missing_or_unreadable_files_gracefully(tmp_path):
    """Detection must not raise when manifests are partial or unreadable."""
    import json as _json

    # Malformed package.json — _detect should fall through, not raise.
    (tmp_path / "package.json").write_text("{not valid json")
    # Empty firebase.json — exists, so still wins (historical convention).
    (tmp_path / "firebase.json").write_text("{}")

    assert _detect_emulator_backend(tmp_path) == "firebase"


# ---------------------------------------------------------------------------
# _read_yaml_scalar
# ---------------------------------------------------------------------------


def test_yaml_scalar_handles_indented_blocks(tmp_path):
    """The minimal YAML reader must resolve ``emulator.kind``."""
    from runner.handler_holdout import _read_yaml_scalar

    manifest = tmp_path / "dark-factory.yaml"
    manifest.write_text(
        "# header comment\n"
        "version: 1\n"
        "emulator:\n"
        "  kind: firebase\n"
        "  start_command: 'firebase emulators:start --only firestore'\n"
        "other:\n"
        "  field: ignored\n"
    )

    assert _read_yaml_scalar(manifest, "emulator", "kind") == "firebase"
    assert _read_yaml_scalar(manifest, "emulator", "start_command") == "firebase emulators:start --only firestore"
    assert _read_yaml_scalar(manifest, "version") == "1"
    assert _read_yaml_scalar(manifest, "missing") is None


def test_yaml_scalar_returns_none_for_missing_file(tmp_path):
    from runner.handler_holdout import _read_yaml_scalar

    assert _read_yaml_scalar(tmp_path / "no-such.yaml", "emulator", "kind") is None


# ---------------------------------------------------------------------------
# _inject_firebase_java — darwin + path-existence gating
# ---------------------------------------------------------------------------


def test_inject_firebase_java_is_noop_on_non_darwin(monkeypatch):
    """On Linux/Windows, the helper must not touch PATH or JAVA_HOME.

    Pins the second half of the audit's gate: even when the emulator backend
    is firebase, a Linux CI host must not be told to use the macOS Homebrew
    openjdk path.
    """
    monkeypatch.setattr(sys, "platform", "linux")

    env = {"PATH": "/usr/bin:/bin"}
    assert _inject_firebase_java(env) is False
    assert env == {"PATH": "/usr/bin:/bin"}


def test_inject_firebase_java_is_noop_when_path_missing(monkeypatch):
    """On macOS, but when Homebrew is not installed at the canonical path, do nothing."""
    monkeypatch.setattr(sys, "platform", "darwin")

    # Force the existence check to fail even on hosts that do have /opt/homebrew/opt/java.
    monkeypatch.setattr("runner.handler_holdout.os.path.isdir", lambda p: False)

    env = {"PATH": "/usr/bin:/bin"}
    assert _inject_firebase_java(env) is False
    assert env == {"PATH": "/usr/bin:/bin"}


def test_inject_firebase_java_prepends_on_darwin_when_path_exists(monkeypatch):
    """Happy path: macOS with Homebrew openjdk present gets PATH and JAVA_HOME set."""
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr("runner.handler_holdout.os.path.isdir", lambda p: p.startswith("/opt/homebrew"))

    env = {"PATH": "/usr/bin:/bin"}
    assert _inject_firebase_java(env) is True
    assert env["PATH"].startswith("/opt/homebrew/opt/java/bin:")
    assert env["PATH"].endswith("/usr/bin:/bin")
    assert env["JAVA_HOME"] == "/opt/homebrew/opt/java"


# ---------------------------------------------------------------------------
# _strip_firebase_gcp_creds
# ---------------------------------------------------------------------------


def test_strip_firebase_gcp_creds_removes_only_firebase_vars():
    env = {
        "GOOGLE_APPLICATION_CREDENTIALS": "/tmp/sa.json",
        "GCLOUD_PROJECT": "my-proj",
        "GOOGLE_CLOUD_PROJECT": "my-proj",
        "PATH": "/usr/bin",
        "OTHER_VAR": "keep-me",
    }
    removed = _strip_firebase_gcp_creds(env)
    assert removed == 3
    assert "GOOGLE_APPLICATION_CREDENTIALS" not in env
    assert "GCLOUD_PROJECT" not in env
    assert "GOOGLE_CLOUD_PROJECT" not in env
    assert env["PATH"] == "/usr/bin"
    assert env["OTHER_VAR"] == "keep-me"


def test_strip_firebase_gcp_creds_idempotent_when_vars_absent():
    env = {"PATH": "/usr/bin"}
    assert _strip_firebase_gcp_creds(env) == 0
    assert env == {"PATH": "/usr/bin"}


# ---------------------------------------------------------------------------
# Full _holdout_eval flow against a fibonacci-shaped impl — the headline
# regression test for lane E.
# ---------------------------------------------------------------------------


@pytest.fixture
def fibonacci_worktree(tmp_path, monkeypatch):
    """Build a fibonacci-style impl tree under tmp_path with the minimum a
    real eval would need: the implementation file plus a fake eval repo that
    just prints a pass verdict. No firebase.json, no Makefile, no
    package.json.
    """
    impl = tmp_path / "impl"
    impl.mkdir()
    (impl / "fib.py").write_text(
        "from __future__ import annotations\n"
        "import sys\n\n"
        "def fib(n: int) -> int:\n    return 0\n\n"
        "if __name__ == '__main__':\n    print(fib(int(sys.argv[1])))\n"
    )

    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json\nprint(json.dumps({'verdict': 'pass', 'scenarios': []}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))

    # The fake repo path is used for both sealed-evaluator resolution and as
    # the repo_path the engine considers home for the impl attribute. Pin a
    # ``holdouts_repo`` node attr so the test does not depend on the default
    # resolution outside the monkeypatched env.
    return impl, fake_repo


def test_fibonacci_worktree_never_touches_firebase_or_homebrew_java(
    monkeypatch, fibonacci_worktree, tmp_path
):
    """Headline regression: a fibonacci impl must not spawn firebase, run
    ``lsof`` against the firebase ports, or inject Homebrew Java — the audit
    found the handler unconditionally assumed Firebase.
    """
    impl, _fake_repo = fibonacci_worktree

    # Make any Homebrew-path lookup a hard fail so the helper short-circuits
    # even on macOS dev hosts.
    monkeypatch.setattr("runner.handler_holdout.os.path.isdir", lambda p: False)
    # Force non-darwin so the platform gate is also exercised.
    monkeypatch.setattr(sys, "platform", "linux")

    # Track every subprocess invocation so we can assert no firebase/make/lsof
    # against the canonical ports.
    spawned: list[tuple[str, ...]] = []

    def _fake_popen(args, *a, **kw):
        spawned.append(tuple(args))
        # Return a stub that satisfies the finally-block's .pid, .wait(),
        # and os.getpgid() lookup. We never let it actually spawn.
        class _Stub:
            pid = 99999

            def wait(self, timeout=None):
                return 0

        return _Stub()

    def _fake_run(args, *a, **kw):
        spawned.append(tuple(args))
        class _Completed:
            returncode = 0
            stdout = '{"verdict": "pass", "scenarios": []}\n'
            stderr = ""

        return _Completed()

    monkeypatch.setattr("runner.handler_holdout.subprocess.Popen", _fake_popen)
    monkeypatch.setattr("runner.handler_holdout.subprocess.run", _fake_run)

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "fibonacci",
            "implementation": str(impl),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)

    # Sanity: the eval still passes — gating did not break the happy path.
    assert result.outcome == "success", result.output

    # No firebase/make/lsof invocations.
    bad_invocs = [
        argv for argv in spawned
        if any(
            str(part).startswith("firebase")
            or str(part) == "make"
            or str(part) == "lsof"
            for part in argv
        )
    ]
    assert not bad_invocs, (
        f"fibonacci worktree triggered firebase/make/lsof: {bad_invocs}"
    )

    # No Popen at all for a non-server backend — only subprocess.run for the
    # sealed evaluator is expected.
    popen_args = [argv for argv in spawned if argv and argv[0] != "python3"]
    assert not popen_args, (
        f"fibonacci worktree spawned a server subprocess: {popen_args}"
    )


def test_fibonacci_worktree_does_not_inject_homebrew_java_into_eval_env(
    monkeypatch, fibonacci_worktree
):
    """Even on darwin dev hosts, a non-firebase impl must not have
    JAVA_HOME or the Homebrew bin in PATH.
    """
    impl, _ = fibonacci_worktree

    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr("runner.handler_holdout.os.path.isdir", lambda p: True)

    captured: dict = {}

    def _capture_run(args, *a, **kw):
        captured["env"] = dict(kw.get("env") or {})
        class _Completed:
            returncode = 0
            stdout = '{"verdict": "pass", "scenarios": []}\n'
            stderr = ""

        return _Completed()

    monkeypatch.setattr("runner.handler_holdout.subprocess.run", _capture_run)
    monkeypatch.setattr(
        "runner.handler_holdout.subprocess.Popen",
        lambda *a, **kw: (_ for _ in ()).throw(AssertionError("Popen must not be called for fibonacci")),
    )

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "fibonacci",
            "implementation": str(impl),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)
    assert result.outcome == "success", result.output

    env = captured.get("env", {})
    # JAVA_HOME must not be injected — the discovered backend is "none", not
    # firebase, so the darwin+firebase gate must keep the env clean.
    assert "JAVA_HOME" not in env, (
        f"fibonacci worktree leaked JAVA_HOME={env.get('JAVA_HOME')!r}"
    )
    # The injected java path must not appear at the *front* of PATH. The
    # user's PATH may legitimately contain /opt/homebrew from their own
    # shell, so the assertion is on the prepend position, not on substring.
    from runner.handler_holdout import _HOMEBREW_JAVA_BIN

    assert not env.get("PATH", "").startswith(_HOMEBREW_JAVA_BIN + ":"), (
        f"fibonacci worktree prepended Homebrew Java bin: {env.get('PATH')!r}"
    )


def test_fibonacci_worktree_keeps_gcp_creds_in_env(monkeypatch, fibonacci_worktree):
    """Non-firebase impls must NOT strip GOOGLE_APPLICATION_CREDENTIALS — the
    audit found the handler unconditionally stripped them, which would break
    any make/node_start backend that legitimately needs the operator's GCP
    credentials.
    """
    impl, _ = fibonacci_worktree

    monkeypatch.setenv("GOOGLE_APPLICATION_CREDENTIALS", "/tmp/operator-sa.json")
    monkeypatch.setenv("GCLOUD_PROJECT", "operator-project")

    captured: dict = {}

    def _capture_run(args, *a, **kw):
        captured["env"] = dict(kw.get("env") or {})
        class _Completed:
            returncode = 0
            stdout = '{"verdict": "pass", "scenarios": []}\n'
            stderr = ""

        return _Completed()

    monkeypatch.setattr("runner.handler_holdout.subprocess.run", _capture_run)
    monkeypatch.setattr(
        "runner.handler_holdout.subprocess.Popen",
        lambda *a, **kw: (_ for _ in ()).throw(AssertionError("Popen must not be called for fibonacci")),
    )

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "fibonacci",
            "implementation": str(impl),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)
    assert result.outcome == "success", result.output

    env = captured.get("env", {})
    # Sanitized env may strip some vars (HOLDOUT*, etc.) but never GCP creds
    # for a non-firebase backend.
    assert env.get("GOOGLE_APPLICATION_CREDENTIALS") == "/tmp/operator-sa.json"
    assert env.get("GCLOUD_PROJECT") == "operator-project"


def test_manifest_declared_firebase_still_injects_java_and_strips_gcp(
    monkeypatch, tmp_path
):
    """The fix must not regress firebase-detection: a worktree that opts in
    via ``dark-factory.yaml`` gets the same Firebase-side handling the
    historical ``firebase.json`` path got.
    """
    impl = tmp_path / "impl"
    impl.mkdir()
    (impl / "dark-factory.yaml").write_text("emulator:\n  kind: firebase\n")
    (impl / "functions" / "package.json").parent.mkdir(parents=True)
    (impl / "functions" / "package.json").write_text("{}")
    (impl / "functions" / "lib" / "index.js").parent.mkdir(parents=True)
    (impl / "functions" / "lib" / "index.js").write_text("module.exports = {};")

    fake_repo = tmp_path / "fake-holdouts"
    (fake_repo / "evaluator").mkdir(parents=True)
    (fake_repo / "evaluator" / "run.py").write_text(
        "import json\nprint(json.dumps({'verdict': 'pass', 'scenarios': []}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))

    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr("runner.handler_holdout.os.path.isdir", lambda p: True)
    # Skip the heavy Firebase emulator Popen by short-circuiting at the eval
    # subprocess step — this test only cares that the env got the Java +
    # GCP-strip treatment.
    seen: dict = {}

    def _capture_run(args, *a, **kw):
        seen["env"] = dict(kw.get("env") or {})
        seen["args"] = list(args)
        class _Completed:
            returncode = 0
            stdout = '{"verdict": "pass", "scenarios": []}\n'
            stderr = ""

        return _Completed()

    monkeypatch.setattr("runner.handler_holdout.subprocess.run", _capture_run)
    monkeypatch.setattr(
        "runner.handler_holdout.subprocess.Popen",
        lambda *a, **kw: (_ for _ in ()).throw(AssertionError("firebase Popen must not fire in this test")),
    )

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "fibonacci",
            "implementation": str(impl),
            "startup_delay": "0",
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)
    assert result.outcome == "success", result.output

    env = seen.get("env", {})
    assert env.get("JAVA_HOME") == "/opt/homebrew/opt/java"
    assert env.get("PATH", "").startswith("/opt/homebrew/opt/java/bin:")
    assert "GOOGLE_APPLICATION_CREDENTIALS" not in env
    assert "GCLOUD_PROJECT" not in env


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))