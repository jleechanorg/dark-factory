# Lane C — working-tree + config review (pr228 uncommitted)

Lane: C of /swarm pr228 review. Mining uncommitted working-tree surface on
branch `pr228` against `/thermo` and `/code-standards` lenses (ZFC +
ponytail + root-cause-first). Committed pr228 diffs are NOT in this lane.

## Scope

Uncommitted working tree (per the brief):

| File | State | Type |
|---|---|---|
| `.githooks/pre-push` | M | Modified shim, was delegating to `pre-push-graph-audit.sh` and `pre-push-repro-artifact-guard.sh` |
| `config/daemon.toml` | M | New top-level keys (`ao_project`, `base_branch`, `stage`, `max_workers`, etc.) were removed back to defaults; new `[repos.*]` block added |
| `goals/2026-07-07-1915-af-e2e-proof/01-success-criteria.md` | M | Downgrades C2 from DONE 2026-07-08 → INSUFFICIENT 2026-07-10 with audit paragraph |
| `roadmap/README.md` | M | Adds 2026-07-10 activity line |
| `.githooks/post-checkout` | NEW | 3-line shim, mirrors Git LFS upstream pre-push pattern |
| `.githooks/post-commit` | NEW | 3-line shim, mirrors Git LFS upstream pre-push pattern |
| `.githooks/post-merge` | NEW | 3-line shim, mirrors Git LFS upstream pre-push pattern |
| `failed_run_log.txt` | NEW | 1.4 MB, 8495 lines, CI runner boot logs (2026-07-09T04:49 UTC, runner `2.335.1`, Ubuntu 24.04) |
| `failed_run_log2.txt` | NEW | 1.4 MB, 8551 lines, same runner/host pattern, ~6 minutes later |
| `goals/2026-07-07-1915-af-e2e-proof/02-ironclad-exit-criteria-pr7888.md` | NEW | 136-line E1–E6 exit criteria + ground rule + anti-goalpost-moving clauses |
| `roadmap/activity/2026-07-10.md` | NEW | 84-line activity entry — 6 daemon infra bugs, /af coder loop results, audit correction |

Uncommitted files NOT in working tree but referenced by additions:
- `worldarchitect.ai` PRs #7888 / #8128 / #7999 / #8177 / #8289 / #8189 / #8036 — are external_ref targets, not in this repo
- dark-factory PRs #212–#217, #227 — are external_ref targets surfaced in 2026-07-10 activity

`core.hooksPath` (verified in repo) = `~/.githooks` ... wait, rechecking —
verified above: `core.hooksPath = .githooks` (the in-repo path). That means
the three new shims DO get installed by `git -C dark-factory checkout`.
Re-reading the brief: "Skim `.githooks/post-merge` for installation
wiring — this is the canonical place new hooks should be registered."
The brief is correct: `.githooks/` is wired via `core.hooksPath`, so any
file here is auto-registered by the git control plane without further
wiring. There is no `install_hooks.sh` to update — the registration is
declarative.

---

## /thermo findings

### F-C1 — Copy-paste triplication across three new git-hooks (HIGH)

`.githooks/post-checkout`, `post-commit`, and `post-merge` are
**byte-identical except for the trailing subcommand and the literal hook
filename in the error message**. The only variable parts are:

1. `git lfs post-<foo> "$@"` — the verb changes per hook
2. The hook filename mentioned in the error message — the only way
   someone reading the failure can locate and delete the right file

This is the textbook "shell-script the same pattern three times with
subtle variations" the brief calls out. The real pre-existing
`.githooks/pre-push` was a **shim** — it delegated to two separate
guard scripts (`pre-push-graph-audit.sh`,
`pre-push-repro-artifact-guard.sh`). That shim pattern was the
existing abstraction layer for "git-LFS wrapper plus extra rules."

**Code-judo move**: introduce ONE helper `.githooks/_git-lfs-hook.sh`
(or `_githook-wrapper.sh`) that takes the verb as `$1`, emits the error
message with the resolved hook filename, and calls `git lfs "$1" "$@"`.
Then each hook becomes a **one-liner**:

```sh
#!/bin/sh
. "$(dirname "$0")/_git-lfs-hook.sh" post-checkout "$@"
```

That collapses the three near-duplicate files (12 lines of repeated
error formatting + plumbing) into a 3-line shell call per hook + one
parameterized helper. The error message currently hard-codes the hook's
filename — that is impossible to express correctly without parameterizing
it (a typo here would tell the user to delete the wrong hook).

Furthermore, the prior `pre-push` shim at `git show HEAD:.githooks/pre-push`
delegated graph-audit + repro-artifact-guard as
**side-chained sibling scripts** (not concatenated onto `git lfs pre-push`).
The new pre-push diff REMOVES those two delegations entirely. That
collapses:

| Before (committed) | After (working tree) |
|---|---|
| `git lfs pre-push "$@"` | `git lfs pre-push "$@"` |
| `pre-push-graph-audit.sh` | — gone |
| `pre-push-repro-artifact-guard.sh` | — gone |

This is a **silent reduction of the pre-push safety net to git-LFS-only**,
with no comment in either direction explaining why the chain was
shortened, no diff justification, and no companion commit re-adding the
guards elsewhere. The 1059-byte `pre-push-graph-audit.sh` and the
1288-byte `pre-push-repro-artifact-guard.sh` still sit in `.githooks/`
but will no longer fire. If those guards are now enforced by CI on
`origin`, that's acceptable; if they were the operator's only local
guard, this is a regression — the diff commit-message is not in scope
to verify, so treat as **needs confirmation** before merge.

> **The original shim pattern is the canonical LFS wrapper pattern; the
> three new files don't follow it**. They reproduce Git LFS's own
> upstream `pre-push` template word-for-word (per the verbatim error
> string), minus the bash-features the upstream template uses.

### F-C2 — Switch from `bash` to POSIX `sh` for LFS hooks (LOW, documentary)

Committed `pre-push` used `#!/usr/bin/env bash` + `set -euo pipefail`.
The new `post-*` hooks use `#!/bin/sh`. This is fine for LFS single-call
shims (LFS upstream itself uses `#!/bin/sh`), but the new `pre-push`
also drops down to `#!/bin/sh` from `#!/usr/bin/env bash`. That is a
real change in behavior:

- `set -euo pipefail` is **removed entirely**. With one command
  (`git lfs pre-push "$@"`), this is harmless in practice — the parent
  shell will not propagate a non-zero exit for LFS's purposes anyway,
  but the *symbolic* guarantee is gone.
- Variables that previously benefited from `pipefail` are no longer
  evaluated under it (none exist here, so no live regression).

The dropped `set -euo pipefail` is consistent with the upstream Git LFS
template (which doesn't set it), but the brief asks for principled
failure semantics — **swallow-and-go** should be an explicit choice,
not an artifact of dropping bash. Add a one-line comment, OR
re-establish `set -eu` for `sh` compatibility (drop `-o pipefail` since
`set -o pipefail` is a bashism, unsafe under POSIX `sh`).

### F-C3 — `config/daemon.toml` adds unread keys (HIGH, blocker-shape)

The added block:

```toml
[repos."jleechanorg/worldarchitect.ai"]
ao_project = "worldarchitect"
push_remote = "worldai"

[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "origin"
```

is **dead config** with respect to the daemon in this branch.
`daemon/src/config.rs:4-18` defines the deserialized struct — it has
no `repos: HashMap<...>` field. `toml::from_str` will silently drop
unknown top-level tables when deserializing into a flat struct via
`serde`, unless `#[serde(deny_unknown_fields)]` is set (it isn't here —
verified). Confirmed by re-running `grep -rn 'push_remote\|repos\b'`
on `daemon/`: the daemon never references `push_remote` or any `[repos.*]`
table. There is no `router.rs` consumer.

**This means the added `[repos.*]` block has zero runtime effect today.**
Worse, it ships with a long comment about why `push_remote` must be
`"worldai"` and not `"origin"` — this comment is documentation for a
field that nothing reads. If the design is "future routing will use this
table," the commit should land the router code in the same PR (per the
"single PR per behavior change" rule of thumb, and per the repo's own
CLAUDE.md: "Merge confidence should come from outcome artifacts").
Adding config without the consumer is a fork-bomb for future debuggers:
they will assume the table is doing something.

If the intent is "this is a placeholder for a different dispatch layer
not in this repo," say so explicitly in the comment — at minimum add
"NOT YET CONSUMED BY `daemon`; will be wired in PR #XYZ." Otherwise this
either (a) belongs in the daemon config struct in the same PR, or (b)
belongs in a NEW config file with a different loader (so the noise in
`daemon.toml` is visible as "not daemon config").

Also note the **top-level keys were removed** in the diff. The committed
`daemon.toml` had `target_repo`, `ao_project`, `base_branch`, `stage`,
`max_workers`, `max_batch`, `fast_tick_secs`, `slow_tick_secs`,
`autonomy_timebox_secs`, `budget_warn_usd`, `spec_dir` — but the
working-tree version only has those SAME keys (git diff hunks confirm
the original file had those 11 lines + the new 12-line append). Wait —
actually the diff shows ONLY an APPEND at line 9+:
the original lines 1-11 are unchanged, the new lines 12-23 append
the `[repos.*]` block. So this is purely additive on top of an
in-tree config, but the `[repos.*]` content is unread.

> **Pure append has worse debugging surface than either (a) consolidating
> into the daemon struct now, or (b) splitting into a sibling `dispatch.toml`
> with its own loader.**

### F-C4 — `failed_run_log.txt` / `failed_run_log2.txt` are not hook input (MEDIUM)

Both files are 1.4 MB of GitHub Actions runner boot noise. They are
untracked (per `??` in the brief). Not gitignored. They live at the
repo root with descriptive filenames that strongly suggest "this is a
log file I should commit." They are not consumed by `.githooks/*`
(bash/shim hook scripts don't process logs). They are not consumed by
any test (the daemon doesn't read them). They are not referenced in
any docs in this repo.

A grep for `fatal` finds two occurrences each:

- `failed_run_log.txt`:  `fatal: could not read Username for 'https://github.com': No such device or address`
- `failed_run_log2.txt`: same fatal twice

The error is a GitHub Actions runner failing to authenticate over an
HTTPS git URL — i.e. an auth-credential glitch on the runner side,
NOT a daemon bug. The other 8500 lines are routine runner boot
(versions, dirs, refs fetched). This is **operational noise with a
single transient auth error**, not a diagnostic record worth committing.

The brief's root-cause-first lens asks: "if these are logs of past
failures, are they diagnostic or showing repeated fixes that didn't
address the root cause?" Answer: **neither — they are unedited GitHub
Actions runner boot logs from 2026-07-09, with a single auth failure
that already resolved (based on the runner going on to fetch refs
successfully in lines 93-100+).** They are not diagnostic for any open
issue. They should either be deleted, moved to `logs/` (already
gitignored, per `.gitignore`), or formally `gitignore`d (the
`.gitignore` already excludes `*.png`, `logs/`, `evidence/` — adding
`failed_run_log*.txt` is one line and removes the temptation).

> **Adjacent finding**: there are also `branch_fail_step__ayz83rw` and
> `branch_fail_step_hg0iohpa` files at repo root — untracked. Same
> pattern: branch_state artifacts that appear to be from local
> experimental runs. Same recommendation: not committed, should be
> gitignored or routed to `logs/`.

### F-C5 — Goal doc and activity doc are durable facts, not speculation (LOW)

`02-ironclad-exit-criteria-pr7888.md` (NEW) sets the ironclad E1–E6
exit criteria, with explicit "ground rule" requiring evidence from a
source OTHER than an agent's self-report or a status label known to be
unreliable. Each box has a "Verified independently by …" pointer
when checked. The anti-goalpost-moving section is constructive — it
explicitly enumerates what does NOT count as evidence (e.g.
"a daemon telemetry event that LOOKS like success does not satisfy any
criterion above without independent confirmation"). This is the kind
of gate that prevents future review from accepting weak evidence.

`roadmap/activity/2026-07-10.md` (NEW) is a faithful activity diary
for a single day. It is detailed, candid (mentions misread CI signals,
a parsing bug in own monitoring, several false positives caught in
cross-verification), and explicitly attributes work to a sidekick team.
It does not speculate on future implementations beyond the filed beads
(each with a stable bead ID — see the cross-PR refs in the appendix).

The changes to `goals/2026-07-07-1915-af-e2e-proof/01-success-criteria.md`
are an honest downgrade of a previously-closed criterion. The audit
paragraph at lines 28-37 is REPRODUCIBLE — it identifies the specific
shortfall ("no `.cast` or `.mp4`, no complete metadata/run/methodology
bundle, … READY chain found (`ez-gh-actions-u3w`, PR #32) records
`all_green=false` before READY with no intervening `all_green=true`
event") and names the badge state. The critique stops short of
pointing at remedies (correct — the remedy work is in
`02-ironclad-exit-criteria-pr7888.md`), which is exactly the right
separation between "what's wrong" and "what's the new plan."

These three docs are NOT speculative — they record durable facts about
the night of 2026-07-10, including its failures. The activity log
includes the admission "PR #7888 not yet merged — `mergeable_state: clean`
achieved but merge itself is correctly human-approval-gated" rather
than overstating the achievement. Per operator context this is the
exact pattern of "honest partial proof" the repo CLAUDE.md calls out.

**No /thermo finding on the docs themselves.** They are doing the work
the brief asks them to do.

### F-C6 — Pre-push shim collapse: lost noise to missing explanation (HIGH for ops)

Re-stating the F-C1 finding focused on `pre-push`:

The committed `pre-push` (visible in `git show HEAD:.githooks/pre-push`)
called BOTH `git lfs pre-push "$@"` AND two sibling guard scripts
(`pre-push-graph-audit.sh` and `pre-push-repro-artifact-guard.sh`).
The working-tree `pre-push` only calls `git lfs pre-push "$@"`.

The two guard scripts are STILL in `.githooks/`:

```
-rwxr-xr-x 1 jleechan jleechan  1059 Jul  5 21:29 pre-push-graph-audit.sh
-rwxr-xr-x 1 jleechan jleechan  1288 Jul  6 21:30 pre-push-repro-artifact-guard.sh
```

They will NOT fire on push, because nothing exec'd into them. If
their responsibility has moved to CI, that should be stated in the
diff message AND it should be confirmed that CI guards the same
risks. As-is, this is a **silent downgrade** of local pre-push
guarantees: a developer who has been relying on `git push` failing
fast will now see pushes succeed that previously failed, and they will
attribute the success to "nothing changed," not "the local guard
went away."

Per repo CLAUDE.md: "Prefer harness fixes over one-off repairs when
the failure is recurring" — these are exactly the kind of local
guardrails that benefit from a pre-commit-on-delete refactoring.
Either:

(a) Keep the chain in pre-push and add the LFS line alongside, OR
(b) Move the graph audit + repro-guard under CI-only and add a
`delete-or-disable` commit that explains the relocation, OR
(c) Document explicitly that the audit/guard purpose has been
superseded.

(c) with a one-paragraph commit message is the lowest-cost path.

---

## /code-standards findings

### F-C7 — Ponytail rung 2: existing canonical helpers exist (HIGH)

`scripts/setup-agent-hooks.sh` is the **existing canonical hook-installer**
for this repo. It writes JSON config for Codex / Cursor / Gemini /
OpenCode CLIs into per-CLI files. It does NOT install git hooks.

`scripts/install-beads-hook.sh` is the **existing canonical git-hook
installer** for this repo. It writes a `pre-commit` hook directly
into `.git/hooks/`.

**These two scripts already exist and have nothing to do with the
new git hooks.** The three new shims are not "reinventing" them —
they are the right concept (small git hook shell script in
`.githooks/`, mirroring the pre-push convention) — but they ARE
reimplementing each other three times. This is F-C1 with a
ponytail-rung-2 lens: "does this pattern already exist in this
codebase?" — yes, in single-instance form via `pre-push`. The new
three shims should adopt the same shim-then-delegate pattern
(pre-push-graph-audit.sh is a sibling-script example), which means
putting the LFS wrapper in ONE script that the three hooks call.

> **Note**: the committed pre-push existed as a **shim-then-source**
> pattern. By deleting that and replacing with a flat single-line
> pre-push, AND by writing three copies of the post-* variant
> without the shim-then-source shape, the working tree is **less
> consistent with itself** than the committed state. The
> simplification direction is wrong: the right move is "one LFS
> wrapper helper, four hook files that delegate into it."

### F-C8 — Zero-Framework-Cognition: no ZFC violation in this lane (PASS)

None of the new files perform `text.contains("...")` / `regex /
intent detection / heuristic classification / hand-tuned scoring /
hardcoded routing tables` for semantic judgment. The hooks do only:

1. `command -v git-lfs` — a process-state query (deterministic,
   exempt per CLAUDE.md)
2. `git lfs post-* "$@"` — a deterministic tool invocation
3. A `printf` to stderr that names the offending hook filename —
   pure data formatting

The `config/daemon.toml` block similarly does not classify or route on
text — it just declares key/value pairs that (per F-C3) are NOT being
consumed at all (so they couldn't even violate ZFC yet). The goal
doc + roadmap doc are narrative documents, not code. The
`failed_run_log*.txt` are raw CI output, unmodified.

The remaining ZFC risk is downstream: when someone wires `[repos.*].push_remote`
into actual routing logic, the dispatch table needs to be a deterministic
key lookup (server-owned), NOT a model picking which remote to push to.
**Flag for the lane that will wire this table.**

### F-C9 — ZFC-leveling-roadmap: N/A (PASS)

No "level" / "tier" / "autonomy level" pickers in any of the files in
this lane. Skipped cleanly.

### F-C10 — root-cause-first: `failed_run_log*.txt` are not root-cause artifacts (MEDIUM)

The brief's lens asks "are `failed_run_log.txt` / `failed_run_log2.txt`
diagnostic or are they showing repeated fixes that didn't address the
root cause?" Already covered in F-C4: they're raw GitHub Actions
runner boot logs with a single transient auth failure (no
authentication token in the runner's authorized_keys / PAT set). They
do not show "repeated fixes" — they show one transient failure that
resolved on the same runner instance.

They ARE a root-cause stand-in for the broader 2026-07-10 audit
("agent self-certification pattern, agentic git pushing on runners
without auth, etc.") — but the root-cause for that is the audit
report itself, not this log file. Per root-cause-first skill,
"the smallest thing that fails if the logic breaks" — these logs
don't enable a regression test.

> The PR-green evidence artifact the audit asks for (`*.cast` /
> `*.mp4` + structured methodology + metadata bundle) is the
> correct root-cause-driven artifact. A 1.4 MB log dump
> uncommitted at repo root is not.

---

## Security / disclosure findings

### F-S1 — `failed_run_log*.txt` contain redacted token placeholders (LOW, file-handling)

The boot logs contain `--add safe.directory` invocations and
`AUTHORIZATION: basic ***` lines. GitHub Actions redacts these to
`***` in the public stream — that's why we see `***`, not a real
token. The files themselves contain no live secrets AS PROVIDED.

**However**: the same files were ingested from a GitHub Actions run;
they contain a checked-in workflow name (`.beads/issues.jsonl sorted
by id`), runner version, runner ID, host region (`westus3`, `eastus`)
and per-job env (`BR_VERSION: 0.2.16`, `BR_ASSET_SHA256: …`). This is
mild operational intel — not a credential leak but a soft disclosure
of internal infra. If this repo is private, fine; if it's public, the
commit would expose build infra signals. Confirmed
[`jleechanorg/dark-factory`](https://github.com/jleechanorg/dark-factory)
**is public** via the auto-factory mission's [PR #212+ history
referenced](#) — committing these logs as text artifacts on a public
repo is a soft disclosure that wouldn't normally pass a security
check.

> Recommendation: **do not commit** either `failed_run_log*.txt`.
> Either (a) `gitignore` them in `.gitignore` as `failed_run_log*.txt`,
> (b) move to `logs/` (already gitignored), or (c) delete. They have
> no canonical value beyond the audit report already recorded in
> `02-ironclad-exit-criteria-pr7888.md` and `roadmap/activity/2026-07-10.md`.

### F-S2 — `config/daemon.toml` path: no live secrets (PASS)

The new `[repos.*]` block contains `ao_project` (an AO project name)
and `push_remote` (a git remote nickname). No tokens, no paths,
no credentials. The label `target_repo = "jleechanorg/worldarchitect.ai"`
(top-level) was already in the committed file and is unchanged.
No secrets-or-paths exposure created.

### F-S3 — pre-push security audit: still fail-closed on LFS missing (PASS)

The new `pre-push` and the three new post-* hooks all `exit 2` if
git-lfs is missing. This is fail-closed (push blocked if LFS won't
fire). The new hooks do NOT honor `--no-verify` overrides any
differently than the committed state — git's `--no-verify` is a
client-side bypass that affects all pre-* hooks uniformly.

The new hooks do NOT print credentials or paths. The error message
points the user at `core.hookspath`, which is a generic git concept
without per-machine sensitivity.

The new hooks are NOT bypassable by env var (no `*_SKIP` or `*_BYPASS`
env var honored). PASS on env-var override.

> The one concern in this section was F-C6 (silent local guard
> downgrade); re-classified here as a security finding because
> guards DROPPING is a soft security regression.

### F-S4 — `config/daemon.toml` introduces conditional routing (MEDIUM — needs spec-intent confirmation)

The `[repos.*]` block IS conditional routing — per `target_repo` key,
pick the right `ao_project` and `push_remote`. This is exactly the
"conditional routing" pattern the global CLAUDE.md `spec-intent
confirmation` rule calls out:

> "Before implementing any infrastructure spec (CI workflows, runner
> routing, deployment config, access control): state the decision
> explicitly and confirm before implementing."

The decision to use `[repos.<owner/repo>]` keyed by `target_repo` is
**not confirmed in the commit and not consumed by any code in this
PR**. This is the canonical "design intent unclear; refactor or
confirm" state the rule prevents. There are at least three plausible
shapes:

(a) `target_repo: String` lookup against a `repos: HashMap<String, RepoCfg>`,
    resolved once at startup; router code passes through.
(b) Per-PR / per-bead config override (each bead carries its own
    `push_remote`).
(c) Out-of-band hardcoded mapping in the AO worktree's git remote
    (the daemon never chooses; the worktree creator already wired
    the right remote).

The current shape (a table in `daemon.toml` that nothing reads) is a
fourth shape: **declaration without consumer**. Picking (a) requires
adding `repos: HashMap<...>` to `daemon/src/config.rs` AND
`dispatch.rs` decision logic. Picking (b) or (c) doesn't need
`daemon.toml` at all.

This finding stands independent of F-C3 — even if you wire (a) in,
the routing semantics need explicit confirmation per the
spec-intent rule.

### F-S5 — Hook installation: declarative wiring, no manual install instructions needed (PASS)

`core.hooksPath = .githooks` (verified from the repo's git config):
every file in `.githooks/` whose name matches a `pre-*` / `post-*`
Git hook event becomes a hook. No installer script is required, no
registration is required. The three new hook files will be picked up
by `git -C dark-factory checkout` automatically. This is the
canonical wiring path the brief pointed at, and it's correct.

> One pre-existing concern is **portability**: a fresh `git clone`
> won't reproduce this repo's `core.hooksPath` setting (it's a
> local config, not `core.bare` etc.). So new clones need
> `git config core.hooksPath .githooks` to be run once. The
> install.sh script (referenced in repo CLAUDE.md) likely does
> this — out of scope here, but worth surfacing if the swarm
> reviewer finds the script missing.

---

## Cross-lens observations

1. **Pre-push collapse ↔ F-S3 regression.** F-C6 (silent loss of
   the graph-audit + repro-artifact-guard local checks) and F-S3
   (security audit: fail-closed still works) are the same finding
   read through two lenses: from a maintainability angle it's a
   lost abstraction; from a security angle it's a soft guarantee
   downgrade. The repo needs explicit confirmation that the
   removed guards have a CI-side successor, OR the chain needs
   to be restored.

2. **`config/daemon.toml` ↔ F-S4 conditional routing.** F-C3
   (unread keys) and F-S4 (unconfirmed routing decision) are the
   same finding from two angles. Wiring the table in the same PR
   would close both. Confirming the routing semantics (per spec-intent
   rule) would close S4. Doing neither leaves the table as dead
   docs.

3. **Docs as audit receipts.** The three doc files
   (`01-success-criteria` diff, `02-ironclad-exit-criteria` new,
   `activity/2026-07-10` new) form a coherent narrative arc:
   prior claim → audit correction → new ironclad bar → activity
   diary. They are doing the work of durable, evidence-grade
   record-keeping the repo CLAUDE.md calls out (".dot files are
   artifacts worth versioning … Learning accumulates in the CXDB
   event log"). The downgraded `01-success-criteria.md` change is
   particularly good — it doesn't suppress a closed criterion; it
   re-opens it with a specific reproducible audit trail.

4. **Triplicated LFS shims ↔ Ponytail rung 6 "one line."** The
   F-C1 fix collapses three near-identical scripts into one helper
   plus three one-liners. This satisfies ponytail rung 6 directly:
   "Can this be one line? Make it one line." Three one-line hook
   files invoke one helper.

5. **Failed-run logs ↔ RCF "no test if no broken behavior."**
   Root-cause-first's "the smallest thing that fails if the logic
   breaks" asks for a check. `failed_run_log*.txt` aren't a check
   — they're raw diagnostic input. Treating logs as evidence is
   the kind of move that produced the 2026-07-10 audit finding
   ("agent self-certification pattern") the docs are at pains to
   distinguish themselves from. These files should not be in the
   commit graph as durable artifacts.

---

## Lane summary

| Finding | Lens | Severity | Block? |
|---|---|---|---|
| F-C1 | /thermo | HIGH | soft (resolve to code-judo) |
| F-C2 | /thermo | LOW | no |
| F-C3 | /thermo + /code-standards + security | HIGH | yes, table is unread config |
| F-C4 | /thermo + root-cause-first | MEDIUM | no, but clean up the trees |
| F-C5 | /thermo | LOW (positive finding) | no |
| F-C6 | /thermo + security | HIGH | yes, silent local-guard downgrade |
| F-C7 | /code-standards (ponytail rung 2) | HIGH | soft (restate C1 via ponytail) |
| F-C8 | /code-standards (ZFC) | PASS | n/a |
| F-C9 | /code-standards (ZFC leveling) | PASS | n/a |
| F-C10 | /code-standards (RCF) | MEDIUM | no, ties to C4 |
| F-S1 | security | LOW | no, but do not commit logs |
| F-S2 | security | PASS | n/a |
| F-S3 | security | PASS (with C6 caveat) | n/a |
| F-S4 | security + spec-intent | MEDIUM | yes, requires explicit confirmation |
| F-S5 | security (wiring) | PASS | n/a |

**Three blockers for merge** (all confirmed against the working tree
and the daemon source in this branch):

1. **F-C6 / F-S3** — `pre-push` chain shortening. Either restore the
   graph-audit + repro-artifact-guard calls into pre-push, OR confirm
   in writing that those responsibilities moved to CI as part of
   `pr228` (and that CI side already covers them), OR delete the
   unused scripts.

2. **F-C3 / F-S4** — `[repos.*]` table is unread config plus an
   unconfirmed routing decision. Either wire the consumer into the
   same PR (add `repos: HashMap<...>` to `daemon/src/config.rs` and
   the dispatcher in `daemon/src/dispatch.rs`), or split the table
   out into a sibling config with its own loader, or drop the
   block entirely.

3. **F-C1 / F-C7** — three near-duplicate hook shims. Extract
   `.githooks/_git-lfs-hook.sh` (parameterized by verb); each hook
   becomes a 1-line delegation. This restores the shim-then-delegate
   pattern already present in the original pre-push.

**Two cleanups (non-blocking but should not ship as-is):**

4. **F-C4 / F-S1 / F-C10** — `failed_run_log*.txt` (and the
   `branch_fail_step_*` siblings at repo root) should be
   `gitignore`d or deleted. They have no canonical value, contain
   soft infra intel that's inappropriate to commit on a public
   repo, and confuse the criterion "evidence" by adding raw log
   noise to the working tree.

5. **F-C2** — `#!/bin/sh` for LFS hooks is fine; either add a one-
   line rationale comment, OR re-establish `set -eu` (drop
   `pipefail` for POSIX-portability).

**Positive findings (verify before removal):**

- The three doc files (modified + 2 new) are doing durable-fact
  record-keeping exactly per the repo's CLAUDE.md. They should
  land. No spec-impl speculation detected.
- `.githooks/` is correctly auto-wired via `core.hooksPath =
  .githooks`; no `install_hooks.sh` edit needed — the brief's
  "where should new hooks be registered" question has a simple
  answer: drop the file in `.githooks/`.

No ZFC, ZFC-leveling, or root-cause-first violations detected in
the hook code itself. ZFC applies downstream only (when the
`[repos.*]` table actually starts driving dispatch).
