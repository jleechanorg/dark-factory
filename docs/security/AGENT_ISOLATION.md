# Agent isolation — Linux backend (jleechan-haux)

## Problem

`runner/handler_sandbox.py` enforces the "implementing agent cannot read
sealed holdouts" contract (see this repo's top-level `CLAUDE.md`, "CRITICAL:
Agent Isolation") by wrapping every coder subprocess (`claude`, `codex`,
`agy`, the `ao spawn`/`ao send` argv) in a deny-list sandbox. Until this
change, the only implementation was macOS's `sandbox-exec`. On Linux,
`_sandboxed_args`/`_sandboxed_args_for_workdir` returned `None`
unconditionally (no `sandbox-exec` binary exists on Linux), and every
caller correctly failed the node closed with `outcome="failure"`. That is
the right failure mode for an *unavailable* backend, but it meant **every
real Linux pipeline run failed at the first codergen node** — there was no
working backend to fall back to. CI's workaround was `DISABLE_SANDBOX=1`
set globally, which — if an operator did the same locally to unblock a
real run — meant zero containment, not degraded containment.

## What was tried and rejected (empirically, not theoretically)

Both obvious candidates were tested against a real locked-down host
(`Jeff-Ubuntu`, Ubuntu 24.04, `kernel.apparmor_restrict_unprivileged_userns=1`
— the shipped default) before writing any production code:

### bubblewrap (`bwrap`)

```
$ bwrap --unshare-user --ro-bind /usr /usr -- /bin/true
bwrap: setting up uid map: Permission denied
```

`bwrap --help` documents `--unshare-user`: "may be automatically implied
if not setuid". This bwrap binary is not setuid (`getcap` / `ls -la` show
no elevated bits), so **every** bwrap invocation needs a working
unprivileged user namespace to do anything at all — even path-masking via
`--tmpfs <path>` (which itself needs a mount namespace, which itself needs
the user namespace). On a host where unprivileged user namespaces are
blocked, bwrap cannot be used, full stop — not "with reduced isolation,"
not usable at all.

### `systemd-run --user --scope -p InaccessiblePaths=...`

This is the more dangerous failure mode, because it looks like it works:

```
$ systemd-run --user --unit=probe --property=InaccessiblePaths=/etc \
    --property=NoNewPrivileges=yes --wait --pipe -- ls /etc
adduser.conf
alsa
...   # <- full /etc listing, NOT blocked
Finished with result: success

$ systemctl --user show probe.service -p InaccessiblePaths
InaccessiblePaths=        # <- empty: the property was never applied
```

The unit reports `Finished with result: success` and the property comes
back **empty** afterward — meaning the mount-namespacing directive was
silently dropped for an unprivileged `--user` transient unit on this host,
not merely "denied and logged." Trusting `systemd-run`'s exit code here
would repeat exactly the failure mode flagged in this project's own
learnings ("gate self-certification anti-pattern" — a check whose expected
value comes from its own unverified assumption can't fail). This backend
is therefore **not used**, and this behavior is the reason
`_verify_linux_preload_denies` (below) does a real behavioral canary
instead of trusting any subprocess's exit code.

## What was built instead: an LD_PRELOAD deny-path shim

`scripts/agent-isolation/deny_paths_preload.c` intercepts
`open`/`open64`/`openat`/`openat64`/`fopen`/`fopen64` via the dynamic
linker (`LD_PRELOAD`) and denies (`errno=ENOENT`) any resolved path under a
colon-separated `DENY_PATHS` list. Path resolution:

- Absolute paths are checked directly.
- Relative paths are resolved against `getcwd()` (for `open`/`fopen`) or
  against `/proc/self/fd/<dirfd>` (for `openat`, when `dirfd != AT_FDCWD`).
- `realpath(3)` is used when the target exists, so a symlink pointing into
  a denied tree is still denied.

This requires **no kernel privilege, no namespaces, no setuid** — just a C
compiler (`cc`/`gcc`) and a dynamically-linked target process. It is
compiled once and cached by content hash under
`~/.cache/dark-factory/agent-isolation/` (`_linux_preload_lib_path` in
`runner/handler_sandbox.py`).

### Honesty about scope (read this before assuming it's bulletproof)

- Only covers dynamically-linked processes that respect `LD_PRELOAD`. A
  statically-linked binary, a setuid/setgid binary (the dynamic linker
  drops `LD_PRELOAD` for those), or a process that issues raw
  `openat(2)` via `syscall(2)` directly bypasses this shim entirely. This
  is **userspace interposition, not kernel-level containment**.
- Covers the file-open entry points every mainstream CLI actually uses on
  glibc Linux (git, python, node, ripgrep, coreutils). It does not
  intercept `stat`/`lstat`/`access` (existence-probing of a denied path is
  still possible; reading its contents is not).
- **Hardlink/rename bypass**: `link(2)` and `renameat(2)` are not
  intercepted. A coder process that already knows a holdout path can
  `ln $HOLDOUT/scenario.txt /tmp/x` and then read `/tmp/x` — the hardlink
  resolves to a non-denied path, so the deny check never fires. This is a
  real gap, flagged by independent review of this PR (see PR #233 review
  comment). Mitigation for now: the operational contract (this repo's
  `CLAUDE.md` "CRITICAL: Agent Isolation") never puts holdout *paths*
  into the implementing agent's prompt, so an agent has no starting
  point to construct such a hardlink without first guessing the sealed
  repo's layout. Closing this gap fully needs either `link`/`renameat`
  interception in the shim or (more robustly) a real mount-namespace
  backend — tracked in follow-up bead jleechan-l2da.
- Denies with `ENOENT` (not `EACCES`) deliberately, so a probe can't
  distinguish "path doesn't exist" from "path is denied."
- When path resolution itself fails (e.g. `/proc/self/fd/<dirfd>` can't be
  read during a relative `openat`), the shim denies rather than silently
  allowing the real call through — see `path_is_denied`'s empty-string
  branch in `deny_paths_preload.c`. This is intentionally fail-closed:
  "couldn't determine what this path really is" must not become "let it
  through."

Given the choice between bwrap/systemd-run (unusable or silently-broken on
this host) and this shim (real, verified, portable), this is the correct
call for the stated acceptance bar — "filesystem restrictions denying
every sealed holdout path" — but it is not a substitute for a real
namespace/VM boundary. See "Follow-ups" below.

### Runtime verification, not exit-code trust

`_verify_linux_preload_denies(lib_path)` in `runner/handler_sandbox.py`
does a real behavioral canary: it creates a temp file, puts its directory
in `DENY_PATHS`, spawns a real subprocess with the shim loaded, and
confirms the read actually fails **and** the content never reaches
stdout. This result is cached for the life of the process. If it can't be
built or the canary fails, `_sandboxed_args`/`_sandboxed_args_for_workdir`
return `None` and the caller fails the node closed — the exact same
contract as a missing `sandbox-exec` on macOS (`outcome="failure"`,
`"sandbox-exec unavailable"`; the message string wasn't changed to avoid
touching the many existing tests that assert on it literally).

## Platform dispatch

```
_sandboxed_args(args) / _sandboxed_args_for_workdir(args, workdir):
  DISABLE_SANDBOX set          -> passthrough (testing escape hatch, unchanged)
  sys.platform == "darwin"     -> sandbox-exec (unchanged)
  sys.platform.startswith("linux") -> LD_PRELOAD shim (new)
  anything else                -> None (fail closed)
```

## Testing

- `tests/security/test_agent_isolation.py` (Linux-only, self-skipping on
  other platforms): shim builds + caches; behavioral canary passes on a
  real host and correctly rejects a bogus/fake library; fail-closed when
  the compiler is missing or the canary fails; real subprocess cannot read
  a holdout-path file OR a sealed-benchmark-doc file (absolute path AND
  relative-path-after-`chdir`); a real subprocess CAN still read a normal
  in-workdir file through the same wrapper; and — the strongest form of
  the acceptance bar — a full pass through the real
  `_codergen(backend="claude")` handler with a fake `claude` binary on
  PATH: an attempted holdout read is denied end-to-end, and a normal task
  through the identical code path still returns `outcome="success"`.
- macOS leakage coverage already existed before this change:
  `tests/test_sealed_paths.py` and `tests/test_ao_sandbox.py` exercise real
  behavior under `sandbox-exec` and self-skip on non-macOS hosts (verified
  they still pass; they were unaffected since they gate on
  `shutil.which("sandbox-exec")`, not on anything this change touches).

## What this PR does NOT do (follow-ups)

- **Wiring beyond the sandbox layer** — `_codergen`'s claude/codex/agy/ao
  branches already called `_sandboxed_args`/`_sandboxed_args_for_workdir`
  before this change (that wiring predates this PR); this PR only fixes
  what those functions *return* on Linux. There is no separate AO/factory
  dispatch-level wiring change needed or made here (e.g. no GitHub App
  token issuance, no egress proxy, no new hook deny-lists) — that is a
  materially larger surface (network egress control, credential scoping,
  resource caps) than this bead's acceptance criteria ("filesystem
  restrictions denying every sealed holdout path") calls for, and none of
  it was empirically verified in this pass. See the filed follow-up bead
  for that broader hardening work if/when it's wanted.
- **Bubblewrap tier for hosts where user namespaces ARE available** — this
  PR does not ship a `bwrap`-based tier, because it could not be exercised
  or verified on the only host available for this work (unprivileged
  userns is blocked here). Shipping untested namespace-wiring code that
  *looks* more secure than the verified LD_PRELOAD shim would itself be a
  regression to the "gate self-certification" trap this doc explicitly
  argues against. A `bwrap`-preferred tier (falling back to this shim when
  `bwrap`'s own real canary fails) is a reasonable follow-up on a host
  where it can actually be tested.
- **`stat`/`lstat`/`access` interception** — existence-probing of denied
  paths is not blocked, only content reads. Low priority (the sealed
  content itself, not its existence, is the asset being protected) but
  worth closing eventually.
- **CI's global `DISABLE_SANDBOX=1`** (`.github/workflows/ci.yml`) is left
  unchanged in this PR. It predates a working Linux backend; now that one
  exists, removing it would make the *entire* existing test suite exercise
  real sandboxing on every PR (compiling C, spawning extra subprocesses),
  which is a bigger behavioral change than this PR's scope justifies to
  land in the same pass. The new `tests/security/` file forces the real
  backend on regardless of the ambient CI env var, so it is exercised in
  CI even with the global flag in place.
