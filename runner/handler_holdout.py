"""Holdout evaluator orchestration.

Owns:
  * `_tcp_port_open` — probe a TCP port with 1s timeout.
  * `_holdout_eval` — run sealed evaluator subprocess + Firebase/Makefile/Java/
    seed orchestration.

All monkeypatched helpers (``_sanitized_env``, ``_holdouts_repo_path``,
``_substitute_state``, ``_path_attr``, ``_has_unresolved_state_placeholder``)
are looked up via the ``runner.handlers`` shim (late binding).
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import time
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


# Homebrew openjdk path on Apple Silicon / Intel macOS. The Firebase emulator
# requires Java on PATH; on Linux JVMs are normally installed system-wide so
# no PATH override is needed. The darwin-only injection lives in
# `_inject_firebase_java` so non-Firebase backends and non-macOS hosts never
# touch this path.
_HOMEBREW_JAVA_BIN = "/opt/homebrew/opt/java/bin"
_HOMEBREW_JAVA_HOME = "/opt/homebrew/opt/java"


def _read_yaml_scalar(path: "pathlib.Path", *keys: str) -> str | None:
    """Return the first scalar string value found at ``keys`` in a tiny YAML subset.

    We deliberately avoid a PyYAML dependency. The manifest schema we accept is:

        emulator:
          kind: firebase
          start_command: "firebase emulators:start --only firestore"

    Indentation is two spaces; only ``key: scalar`` rows are recognized, which
    is enough for the ``emulator: {kind, start_command}`` block we own. Returns
    ``None`` if the file is missing, unparseable, or no key matches.
    """
    try:
        text = path.read_text()
    except (FileNotFoundError, PermissionError, OSError):
        return None
    lines = text.splitlines()
    # Walk the line tree tracking indentation; each ``key:`` row at deeper
    # indentation under a known parent is a candidate.
    parents: list[tuple[int, str]] = []
    want_parents = list(keys[:-1])
    for raw in lines:
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        stripped = raw.lstrip()
        indent = len(raw) - len(stripped)
        # Pop parents whose indent is >= this line's indent — they're siblings
        # or ancestors, not the parent of this line.
        while parents and parents[-1][0] >= indent:
            parents.pop()
        if ":" not in stripped:
            continue
        key, _, value = stripped.partition(":")
        key = key.strip()
        value = value.strip()
        if not key:
            continue
        # Strip a single layer of matched single or double quotes — the
        # schema we accept uses scalar values like ``start_command: 'foo'``
        # or ``port: "8080"``. Anything else (multi-line, escaped, block
        # scalars) is out of scope.
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
            value = value[1:-1]
        # Top-level scalar: callers passing a single key (no parents).
        if not parents and not want_parents:
            if key == keys[0]:
                return value or None
            continue
        # If we have no current parents and the line is a top-level key, push.
        if not parents:
            if key in want_parents:
                parents.append((indent, key))
                if not value or value.startswith("#"):
                    continue
                if len(want_parents) == 1:
                    return value
            continue
        # Otherwise check whether the active parent chain still matches.
        current_parent_keys = [p[1] for p in parents]
        if current_parent_keys == want_parents and key == keys[-1]:
            return value or None
        if key in want_parents[len(current_parent_keys):]:
            parents.append((indent, key))
    return None


def _detect_emulator_backend(impl: "pathlib.Path") -> str:
    """Discover the emulator backend the implementation under test expects.

    Discovery order (first match wins):
      1. ``dark-factory.yaml`` with ``emulator.kind: <backend>`` — explicit
         per-worktree override.
      2. ``firebase.json`` present at the impl root → ``firebase`` (keeps the
         historical amazon-clone convention working without a manifest).
      3. ``Makefile`` with a ``run:`` target → ``make``.
      4. ``package.json`` with a ``start`` script → ``node_start``.
      5. Otherwise → ``none`` (the evaluator runs without a local server;
         e.g. fibonacci, hello, or any pure-Python CLI benchmark).

    Returns one of: ``"firebase"``, ``"make"``, ``"node_start"``, ``"none"``.
    The value is consumed by `_holdout_eval` to decide whether to inject the
    Homebrew Java path, whether to strip GCP credentials, and which command to
    spawn for the local server subprocess.
    """
    manifest = impl / "dark-factory.yaml"
    kind = _read_yaml_scalar(manifest, "emulator", "kind")
    if kind:
        return str(kind).strip().lower()

    if (impl / "firebase.json").exists():
        return "firebase"

    makefile = impl / "Makefile"
    if makefile.exists():
        try:
            if re.search(r"^run\s*:", makefile.read_text(), re.MULTILINE):
                return "make"
        except (PermissionError, OSError):
            pass

    pkg_json = impl / "package.json"
    if pkg_json.exists():
        try:
            data = json.loads(pkg_json.read_text())
            if "start" in (data.get("scripts") or {}):
                return "node_start"
        except (json.JSONDecodeError, PermissionError, OSError):
            pass

    return "none"


def _inject_firebase_java(eval_env: dict) -> bool:
    """Prepend Homebrew openjdk to ``eval_env['PATH']``/``JAVA_HOME``.

    Returns True iff Java was injected. The darwin gate keeps non-macOS hosts
    untouched — Linux CI runners already have Java on PATH via apt, and the
    Homebrew path does not exist there. Non-firebase call sites must not
    invoke this helper.
    """
    if sys.platform != "darwin":
        return False
    if not os.path.isdir(_HOMEBREW_JAVA_BIN):
        return False
    eval_env["PATH"] = _HOMEBREW_JAVA_BIN + ":" + eval_env.get("PATH", "")
    eval_env["JAVA_HOME"] = _HOMEBREW_JAVA_HOME
    return True


def _strip_firebase_gcp_creds(eval_env: dict) -> int:
    """Pop real GCP credentials so the Firebase emulator uses the local project.

    Only invoked when the detected emulator backend is ``firebase``; non-firebase
    backends may legitimately need the operator's GCP credentials for
    downstream tooling. Returns the number of vars removed.
    """
    removed = 0
    for gcp_var in ("GOOGLE_APPLICATION_CREDENTIALS", "GCLOUD_PROJECT", "GOOGLE_CLOUD_PROJECT"):
        if eval_env.pop(gcp_var, None) is not None:
            removed += 1
    return removed


def _tcp_port_open(host: str, port: int, timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _holdout_eval(node: "Node", ctx: "Context") -> "Result":
    """Run the sealed holdout evaluator in a separate process.

    Emulator backend discovery is manifest-driven (see
    ``_detect_emulator_backend``). The Firebase-specific invariants from
    memory/feedback_2026-05-24_holdout_eval_emulator_infra.md only apply when
    the discovered backend is ``firebase``:

      1. Java on PATH for Firebase emulators (Homebrew openjdk path on macOS).
      2. Poll TCP ports, don't sleep — wait for all emulators to be ready.
      3. Kill process GROUP on cleanup, not just the wrapper process.
      4. Strip real GCP credentials from env so Cloud Functions emulator
         uses local project (firebase-only — non-firebase backends keep creds).
      5. Pre-clean emulator ports before launching to kill stale JVM holders.
      6. Run seed script (impl/scripts/seed.ts or npm run seed) after emulators
         are ready (firebase-only).
    """
    import random

    repo_path = _handlers_shim._holdouts_repo_path()
    node_feature = node.attrs.get("feature")
    feature = str(ctx.state.get("feature", "")) or (str(node_feature) if node_feature is not None else "")
    if isinstance(feature, str) and "${state." in feature:
        feature = _handlers_shim._substitute_state(feature, ctx)
        if "${state." in feature:
            return Result(outcome="failure", output=f"unresolved feature path: {node_feature!r}")
    feature = str(feature or "").strip()
    if not feature:
        return Result(outcome="failure", output="no feature attribute or state")

    eval_script = repo_path / "evaluator" / "run.py"
    try:
        exists = eval_script.exists()
    except PermissionError:
        exists = False
    if not exists:
        return Result(outcome="failure", output=f"holdout evaluator missing: {eval_script}")

    impl_attr = node.attrs.get("implementation")
    if impl_attr:
        resolved = _handlers_shim._substitute_state(impl_attr, ctx)
        if _handlers_shim._has_unresolved_state_placeholder(resolved):
            return Result(outcome="failure", output=f"unresolved implementation path: {impl_attr}")

    impl = _handlers_shim._path_attr(node, ctx, "implementation", ctx.workdir)
    if not impl.exists():
        return Result(outcome="failure", output=f"implementation missing: {impl}")

    port = random.randint(30001, 30999)

    # Build eval env from the sanitized environment: the server/seed
    # subprocesses below run agent-authored code (make run, npm seed,
    # scripts/seed.*), so DARK_FACTORY_HOLDOUTS / *HOLDOUT* must never reach
    # them — a seed script could copy holdout content into the worktree for
    # the next fix-loop iteration. The sealed evaluator does not need the
    # variable either (it resolves scenarios relative to its own repo path
    # and strips holdout vars from its own children).
    eval_env = _handlers_shim._sanitized_env()

    # Discover the emulator backend from the implementation tree, NOT a
    # hardcoded Firebase assumption. ``dark-factory.yaml`` -> ``firebase.json``
    # -> Makefile ``run:`` -> ``package.json`` ``start`` -> none.
    emulator_backend = _detect_emulator_backend(impl)

    # Fix 1 — Java PATH: prepend Homebrew openjdk so Firebase emulators can
    # find java. Only fires when (a) the discovered backend is ``firebase``
    # AND (b) we are on macOS — non-firebase backends never need Java, and
    # Linux CI hosts get Java from apt. This is the explicit gate the audit
    # called out (lane E / jleechan-je5).
    if emulator_backend == "firebase":
        _inject_firebase_java(eval_env)

    # Fix 4 — Strip real GCP credentials: Cloud Functions emulator must use
    # local project. Firebase-only — non-firebase backends may legitimately
    # need the operator's real GCP credentials (e.g. make/node_start hitting
    # a real staging endpoint during scoring).
    if emulator_backend == "firebase":
        _strip_firebase_gcp_creds(eval_env)

    eval_env["BENCHMARK_PORT"] = str(port)

    startup_delay = int(node.attrs.get("startup_delay", "5"))
    server_proc = None
    makefile = impl / "Makefile"
    firebase_json = impl / "firebase.json"
    has_make_run = False
    if emulator_backend == "make" and makefile.exists():
        try:
            has_make_run = bool(re.search(r"^run\s*:", makefile.read_text(), re.MULTILINE))
        except Exception:
            pass
    if has_make_run:
        env_p = dict(eval_env)
        env_p["PORT"] = str(port)
        # Fix 3 — start_new_session=True so we can killpg the whole JVM tree.
        server_proc = subprocess.Popen(
            ["make", "run"], cwd=str(impl), env=env_p,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        time.sleep(startup_delay)
    elif emulator_backend == "firebase" and firebase_json.exists():
        # Kill any lingering processes from previous runs that hold the
        # Firebase emulator ports, so the new emulator can bind cleanly.
        for _em_port in (8080, 9099, 5001, 4000, 4400):
            try:
                _lsof = subprocess.run(
                    ["lsof", "-ti", f":{_em_port}"],
                    capture_output=True, text=True, timeout=5)
                for _pid_s in _lsof.stdout.strip().split():
                    try:
                        os.kill(int(_pid_s), signal.SIGTERM)
                    except (ProcessLookupError, ValueError):
                        pass
            except Exception:
                pass
        time.sleep(2)
        # Ensure Java is on PATH — Firebase emulators require it.
        # Homebrew installs Java at /opt/homebrew/opt/java/bin but doesn't
        # add it to PATH by default. Prepend it so emulators can find `java`.
        # Already gated by `_inject_firebase_java` above; idempotent guard
        # for the legacy case where the backend was discovered via
        # ``firebase.json`` directly without a manifest.
        _homebrew_java = "/opt/homebrew/opt/java/bin"
        _java_path = eval_env.get("PATH", "")
        if sys.platform == "darwin" and os.path.isdir(_homebrew_java) and _homebrew_java not in _java_path:
            eval_env["PATH"] = _homebrew_java + ":" + _java_path
            eval_env.setdefault("JAVA_HOME", _HOMEBREW_JAVA_HOME)
        # Build Cloud Functions if source exists but compiled output is missing
        fn_pkg = impl / "functions" / "package.json"
        fn_lib = impl / "functions" / "lib" / "index.js"
        if fn_pkg.exists() and not fn_lib.exists():
            subprocess.run(
                ["npm", "install", "--prefix", str(impl / "functions"), "--silent"],
                cwd=str(impl), capture_output=True, timeout=120)
            subprocess.run(
                ["npm", "run", "build", "--prefix", str(impl / "functions")],
                cwd=str(impl), capture_output=True, timeout=120)
        server_proc = subprocess.Popen(
            ["firebase", "emulators:start",
             "--only", "firestore,auth,storage,functions"],
            cwd=str(impl), env=dict(eval_env),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        # Fix 5b — atexit cleanup so orphan JVMs are reaped even on SIGKILL (orch-2fze).
        import atexit as _atexit
        _atexit_pgid: list[int | None] = [None]
        try:
            _atexit_pgid[0] = os.getpgid(server_proc.pid)
        except Exception:
            pass

        def _kill_emulator_group(_pgid_ref: list = _atexit_pgid) -> None:
            pgid = _pgid_ref[0]
            if pgid is not None:
                try:
                    os.killpg(pgid, signal.SIGTERM)
                except Exception:
                    pass

        _atexit.register(_kill_emulator_group)
        # Poll until all required emulator ports respond or startup_delay expires.
        _emulator_ports = [8080, 9099, 5001]
        _deadline = time.monotonic() + startup_delay
        while time.monotonic() < _deadline:
            if all(_tcp_port_open("localhost", p) for p in _emulator_ports):
                break
            time.sleep(2)

        # Fix 6 — seed emulator with baseline data before evaluator runs (orch-0bne).
        _seed_pkg = impl / "package.json"
        _seed_ts = impl / "scripts" / "seed.ts"
        _seed_js = impl / "scripts" / "seed.js"
        _seeded = False
        if _seed_pkg.exists():
            try:
                _pkg_data = json.loads(_seed_pkg.read_text())
                if "seed" in _pkg_data.get("scripts", {}):
                    subprocess.run(
                        ["npm", "run", "seed"],
                        cwd=str(impl), env=dict(eval_env),
                        capture_output=True, timeout=30, check=False)
                    _seeded = True
            except Exception:
                pass
        if not _seeded and _seed_ts.exists():
            try:
                subprocess.run(
                    ["npx", "ts-node", str(_seed_ts)],
                    cwd=str(impl), env=dict(eval_env),
                    capture_output=True, timeout=30, check=False)
                _seeded = True
            except Exception:
                pass
        if not _seeded and _seed_js.exists():
            try:
                subprocess.run(
                    ["node", str(_seed_js)],
                    cwd=str(impl), env=dict(eval_env),
                    capture_output=True, timeout=30, check=False)
            except Exception:
                pass

    try:
        proc = subprocess.run(
            ["python3", str(eval_script), "--feature", feature, "--impl", str(impl)],
            cwd=repo_path, capture_output=True, text=True, timeout=600, check=False, env=eval_env)
        verdict = "failure"
        summary = {
            "verdict": verdict,
            "passed": 0,
            "total": 0,
            "status_counts": {},
            "sealed": True,
        }
        for line in reversed(proc.stdout.splitlines()):
            if line.strip().startswith("{") and line.strip().endswith("}"):
                try:
                    data = json.loads(line.strip())
                    verdict = data.get("verdict", "failure").lower()
                    scenarios = data.get("scenarios", [])
                    status_counts: dict[str, int] = {}
                    for sc in scenarios:
                        status = str(sc.get("status", "unknown"))
                        status_counts[status] = status_counts.get(status, 0) + 1
                    passed = status_counts.get("pass", 0)
                    total = len(scenarios)
                    summary = {
                        "verdict": verdict,
                        "passed": passed,
                        "total": total,
                        "status_counts": status_counts,
                        "sealed": True,
                    }

                    # Write only redacted holdout results into the implementation
                    # tree. Per-scenario data remains sealed in the evaluator.
                    results_file = impl / "results" / "holdout_results.json"
                    results_file.parent.mkdir(exist_ok=True)
                    results_file.write_text(json.dumps(summary, indent=2))

                    break
                except: pass
        # rc!=0 + verdict=pass means the evaluator process crashed/exited
        # abnormally even though it printed a pass verdict line — that's a
        # spoof attempt or infra bug, not a real pass. Route to "error" so
        # the engine can route via outcome!=success edges and the Healer
        # clusters infra crashes separately from real failures.
        if proc.returncode and verdict == "pass":
            outcome = "error"
            summary = {**summary, "verdict": "error", "returncode": proc.returncode}
        elif verdict == "pass":
            outcome = "success"
        else:
            outcome = verdict
        return Result(
            outcome=outcome,
            output=json.dumps(summary, indent=2),
            metadata={"verdict": verdict, "port": str(port), "sealed": "true"},
        )
    finally:
        if server_proc:
            # Kill the entire process group so JVM child processes (Firestore
            # emulator) are terminated along with the firebase CLI wrapper.
            # start_new_session=True gives the process its own session, so
            # os.killpg on the process group reaps all children.
            try:
                pgid = os.getpgid(server_proc.pid)
                os.killpg(pgid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                server_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    pgid = os.getpgid(server_proc.pid)
                    os.killpg(pgid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
