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
import time
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _tcp_port_open(host: str, port: int, timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _holdout_eval(node: "Node", ctx: "Context") -> "Result":
    """Run the sealed holdout evaluator in a separate process.

    Infrastructure invariants (see memory/feedback_2026-05-24_holdout_eval_emulator_infra.md):
    1. Java on PATH for Firebase emulators (Homebrew openjdk path).
    2. Poll TCP ports, don't sleep — wait for all emulators to be ready.
    3. Kill process GROUP on cleanup, not just the wrapper process.
    4. Strip real GCP credentials from env so Cloud Functions emulator uses local project.
    5. Pre-clean emulator ports before launching to kill stale JVM holders.
    6. Run seed script (impl/scripts/seed.ts or npm run seed) after emulators are ready.
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

    # Fix 1 — Java PATH: prepend Homebrew openjdk so Firebase emulators can find java.
    homebrew_java = "/opt/homebrew/opt/java/bin"
    if os.path.isdir(homebrew_java):
        eval_env["PATH"] = homebrew_java + ":" + eval_env.get("PATH", "")
        eval_env["JAVA_HOME"] = "/opt/homebrew/opt/java"

    # Fix 4 — Strip real GCP credentials: Cloud Functions emulator must use local project.
    for gcp_var in ("GOOGLE_APPLICATION_CREDENTIALS", "GCLOUD_PROJECT", "GOOGLE_CLOUD_PROJECT"):
        eval_env.pop(gcp_var, None)

    eval_env["BENCHMARK_PORT"] = str(port)

    startup_delay = int(node.attrs.get("startup_delay", "5"))
    server_proc = None
    makefile = impl / "Makefile"
    firebase_json = impl / "firebase.json"
    has_make_run = False
    if makefile.exists():
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
    elif firebase_json.exists():
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
        _homebrew_java = "/opt/homebrew/opt/java/bin"
        _java_path = eval_env.get("PATH", "")
        if _homebrew_java not in _java_path:
            eval_env["PATH"] = _homebrew_java + ":" + _java_path
        eval_env.setdefault("JAVA_HOME", "/opt/homebrew/opt/java")
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
