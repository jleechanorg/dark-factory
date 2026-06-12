# CLI Fallback Chain Audit — 2026-06-12

> **Lane B PR (test/cli-fallback-coverage)** — read-only audit of the
> `runner/handlers.py` CLI fallback chain. Documents the actual missing-CLI
> behavior for each backend (claude, codex, agy, ao) in the *coder* path
> (`_codergen`) and the *reviewer-gate* path (`_run_gate_once` /
> `_execute_gate`).
>
> **Scope discipline:** This PR does NOT modify `runner/handlers.py`
> (WIP'd by the `claudeaf` author) or `runner/__main__.py` (WIP). Findings
> are documented honestly — including the one confirmed panic — and the
> fix is tracked as a separate bead.

## TL;DR

| Backend | Coder path missing-CLI | Reviewer-gate missing-CLI | `FileNotFoundError` caught? | `metadata["backend_missing"]="true"` set? |
|---|---|---|---|---|
| `claude` | `failure` (clean) | `error` with backend_missing → agy/claude infra fallback (gate path) | YES (coder: `except Exception`; gate: dedicated `except FileNotFoundError`) | YES (gate path only) |
| `claudeaf` | identical to `claude` (shares claude branch) | identical to `claude` (shares claude branch) | YES (inherits claude) | YES (inherits) |
| `codex` | `error` (clean) | `error` with backend_missing → agy/claude infra fallback | YES (coder: `except Exception`; gate: dedicated `except FileNotFoundError`) | YES (gate path only) |
| `agy` | **PANIC** (`FileNotFoundError` propagates from `subprocess.Popen`) | `error` with backend_missing → claude infra fallback (clean) | NO (coder) / YES (gate, dedicated handler) | YES (gate path only) |
| `ao` | `failure` (clean — `except Exception` catches the missing binary) | not in reviewer-gate priority queue | YES (coder: `except Exception`) | NO (coder never sets it) |

> **One confirmed bug:** the `agy` *coder* path does not catch
> `FileNotFoundError`. See
> [Bead jleechan-c5q](#known-bugs) below.

## Per-backend dispatch paths

### `claude` / `claudeaf` (coder path)

`_codergen` lines 493-553. The branch is `if backend in ("claude", "claudeaf"):`
and builds `[claude_bin, "--print", "--output-format", "json", ...]`,
wraps in `_sandboxed_args`, then `subprocess.run(...)` inside a
`try/except` block.

The `except Exception as e:` (line 533) catches **all** subprocess
exceptions including `FileNotFoundError`, returning a clean
`Result(outcome="failure", output="claude backend error: ...")` with
`metadata["timed_out"]="false"`.

`claudeaf` (WIP, added by Lane A) shares this exact branch — only the
**env** differs (`CLAUDE_CONFIG_DIR=~/.claude-agent-f` injected by
`_gate_subprocess_env`). The argv is identical and the missing-CLI
behavior is identical.

### `codex` (coder path)

`_codergen` lines 554-592. Builds `[codex, "exec", "--yolo", ...]`,
then `subprocess.run(...)` inside a `try/except`.

The `except Exception as exc:` (line 581) catches **all** subprocess
exceptions including `FileNotFoundError`, returning a
`Result(outcome="error", output="codex backend error: ...")` —
notably with outcome `error`, not `failure` (the codex branch's
convention).

### `agy` (coder path) — BUGGY

`_codergen` lines 591-668. Builds `[agy, "--add-dir", ...]`, then
`subprocess.Popen(...)` followed by `proc.communicate(timeout=...)`.

The only `try/except` is around `proc.communicate` (line 636) and
**only catches `subprocess.TimeoutExpired`** (line 638). The Popen
call itself (line 627) is **unprotected**. When `agy` is missing
from PATH, Popen raises `FileNotFoundError: [Errno 2] No such file or
directory: 'agy'` which propagates up through `_codergen` and
panics the node.

> **Confirmed empirically** (2026-06-12): a hand-rolled
> `FakePopen` raising `FileNotFoundError` produces an unhandled
> exception, not a `Result`.

### `ao` (coder path)

`_codergen` lines 315-491. Two sub-branches: `ao spawn` (line 322) and
`ao send` (line 410). Both wrap their `subprocess.run` calls in
`try/except subprocess.TimeoutExpired` **and** `except Exception as exc:`
(lines 352, 445). `FileNotFoundError` is subsumed by the `Exception`
clause, producing a clean
`Result(outcome="failure", output="ao spawn failed: [Errno 2] ...")`.

If `sandbox-exec` is missing, `_sandboxed_args` returns `None` and the
branch short-circuits to
`Result(outcome="failure", output="sandbox-exec unavailable")` at
line 325/419 — clean.

## Reviewer-gate path (`_run_gate_once` + `_execute_gate`)

`_run_gate_once` (line 1138) is the single point that builds the
argv + env per backend. Its `try/except` is the most defensive of
all four paths:

- `except subprocess.TimeoutExpired` → `failure` with `timed_out="true"`
- `except FileNotFoundError as exc:` (line 1192) → `error` with
  `backend_missing="true"` ← **dedicated handler, not just `Exception`**
- `except Exception as exc:` (line 1200) → `error`

`_execute_gate` (line 1434) then runs the resolved backend and, if
`_is_gate_infra_failure(result)` is true and the backend is not
already claude-routed, falls back to `claude`. The
`_is_gate_infra_failure` predicate (line 1237) considers
`outcome == "error"`, `sandbox == "unavailable"`, `timed_out == "true"`,
or `backend_missing == "true"` to be an infra failure (eligible for
fallback). A real `verdict: fail|partial` is **never** an infra failure
and is **never** retried on a different backend (the
no-reviewer-shopping rule).

When the entire reviewer priority queue is empty (no backends
installed), `_resolve_adversarial_backend` (line 1296) falls through
to the last priority entry, and `_run_gate_once` reports
`backend_missing="true"`. `_execute_gate` then retries on `claude`,
which also fails → final result has `metadata["verdict"] = "infra_failure"`
so operators can distinguish "no reviewer ever graded the diff" from a
real FAIL.

## Known bugs

### `agy` coder path panics on missing CLI — bead jleechan-c5q

- **Location:** `runner/handlers.py` lines 591-668 (the `elif backend == "agy":`
  branch of `_codergen`).
- **Bug:** `subprocess.Popen(args, ...)` at line 627 is unprotected. When
  `agy` is missing, Popen raises `FileNotFoundError` which propagates
  out of `_codergen` and panics the entire pipeline node.
- **Comparison:** every other backend (`claude`, `claudeaf`, `codex`,
  `ao`) catches `Exception` around the subprocess call and returns a
  clean `Result` with the appropriate outcome. The gate path
  (`_run_gate_once`) has a dedicated `except FileNotFoundError` and
  emits `metadata["backend_missing"]="true"`.
- **Bead:** `jleechan-c5q` — `handlers: agy coder path lacks FileNotFoundError handler`
  (type=bug, priority=1, filed 2026-06-12 by Lane B).
- **Status:** **NOT FIXED in this PR** because `runner/handlers.py` is
  WIP'd by the `claudeaf` author (Lane A). The fix is trivial — wrap
  the Popen call in `try/except FileNotFoundError` mirroring the gate
  path — and should land in the first PR after the `claudeaf` WIP merges.
- **Coverage:** the new test `test_agy_coder_missing_panics_unprotected`
  is marked `xfail(strict=True, reason="bead jleechan-c5q: agy coder path
  lacks FileNotFoundError handler")` so the failure is visible (and
  intentional) until the fix lands.

## Verified by

- **Test file:** `tests/test_cli_fallbacks.py` (new, this PR).
- **Test count:** 10 tests covering the per-backend coder path and
  reviewer-gate path.
- **Test run:** `.venv/bin/python -m pytest tests/test_cli_fallbacks.py -v`
  is expected to show 9 PASS + 1 XFAIL on a clean main checkout.
- **Beads:** `jleechan-c5q` (filed by this PR for the agy Popen panic).
- **No production code modified:** `git diff --name-only origin/main..HEAD`
  shows only `tests/test_cli_fallbacks.py`,
  `docs/cli-fallback-audit-2026-06-12.md`, and this PR's bead file.
