# Research: supervisor/worker-spawn security patterns for a Go AO spawn

Standalone research (not tied to reading PR #842's diff) on established practice for a
supervisor process that spawns external worker CLI subprocesses into a git
workspace and talks to multiple AI provider APIs.

## 1. Validating a reused workspace's repo + revision identity before trust

No CI system treats a reused checkout directory as trustworthy by default — the
universal pattern is **clean-or-verify, not assume**:

- **GitHub `actions/checkout`**: defaults `clean: true`, running
  `git clean -ffdx && git reset --hard HEAD` before fetching into a possibly-reused
  `$GITHUB_WORKSPACE`, rather than trusting prior contents
  ([actions/checkout](https://github.com/actions/checkout)). GitHub's newer
  same-repository (`$/`) reference syntax identifies the repo by the commit
  actually being executed — again re-deriving identity from the live commit,
  not from a workspace-directory label
  ([GitDash](https://gitdash.dev/blog/github-actions-self-repository-syntax)).
- **Buildkite agent**: explicitly documents that it reuses a previous checkout
  *after cleaning* by default, and calls this **unsafe for untrusted-source
  builds** (e.g. third-party PRs) unless `BUILDKITE_CLEAN_CHECKOUT=true` forces a
  fresh clone. Sequence is pre-checkout hooks → fetch/clone → `git clean` of the
  working directory ([Buildkite docs](https://buildkite.com/docs/pipelines/configure/git-checkout)).
  For multi-job builds needing an absolute-guarantee pin, the
  `runreal-checkout-buildkite-plugin` pattern resolves the ref **once** and
  records it in build metadata so every later job reuses that exact value
  instead of re-resolving a possibly-moved branch
  ([runreal-checkout-buildkite-plugin](https://github.com/runreal/runreal-checkout-buildkite-plugin)).
- **Jenkins**: no built-in check that a reused agent workspace's `origin` still
  matches the configured SCM URL — this is a known real-world gap, worked around
  operationally via "Wipe out repository & force clone" / "Clean before checkout"
  SCM extensions ([jenkinsci/git-plugin](https://plugins.jenkins.io/git/),
  [workflow-scm-step-plugin](https://github.com/jenkinsci/workflow-scm-step-plugin/blob/master/README.md)).
  This is cited below as the negative example: **absence** of automatic identity
  validation is a documented pain point, not a model to copy.

**Synthesized principle**: a supervisor reusing an existing workspace must
independently re-derive both repo identity (e.g. `git remote get-url origin` /
`git rev-parse --show-toplevel` matched against the expected repo) and revision
(`git rev-parse HEAD` matched against the expected SHA/branch) from the
workspace's live git state immediately before trusting it — never from a cached
label, directory name, or prior run's metadata. Treat a mismatch as fail-closed
(reject/re-clone), mirroring `actions/checkout`'s reset-before-trust default and
Buildkite's "unsafe to reuse without cleaning" warning. This directly matches
this repo's own re-pin discipline (`~/.codex/AGENTS.md` § Verify before
reporting: "Re-pin mutable state at the moment you verify it, never from
prose").

## 2. Exit-code-vs-stdout-parsing ordering

Consensus across language ecosystems is **check the exit code first; treat
stdout as authoritative only after confirming zero-exit** — this is the
`CWE-252` "Unchecked Return Value" class applied to subprocess results:

- **Python `subprocess`**: official docs and community guidance converge on
  `subprocess.run(..., check=True)` (raises `CalledProcessError` on non-zero
  exit) or explicit `returncode` inspection / `check_returncode()` *before*
  reading `.stdout` — "parsing `stdout` before verifying the exit code is a form
  of unchecked return value... blindly parsing that output can lead to silent
  data corruption" ([Python docs](https://docs.python.org/3/library/subprocess.html);
  AWS CodeGuru unchecked-return-value detector:
  [docs.aws.amazon.com](https://docs.aws.amazon.com/codeguru/detector-library/c/unchecked-return-value)).
- **Best-effort exception, scoped narrowly**: the recognized *exception* is not
  "parse regardless of exit code" wholesale — it's wrapping a single best-effort
  read (e.g. `git rev-parse --short HEAD` for a version string) in a
  try/except around the specific failure, falling back to a default rather than
  propagating. Libraries like `subx` make this explicit by returning a
  structured `(stdout, stderr, ret)` tuple and requiring the caller to check
  `ret` before deciding whether to trust `stdout` — the check is still
  mandatory, just not exception-based (`subx` on PyPI).
  The other recognized pattern is bash `trap ... ERR` for cleanup after a
  detected non-zero exit (e.g. failed `git pull` before it can deploy stale
  code) — cleanup runs *because* the exit code was checked and found bad, not
  as an excuse to skip the check.

**Synthesized principle**: for a "spawned session `<id>`" success-signal line,
exit code is the primary authoritative gate — parse stdout for the session ID
only after confirming `returncode == 0`. A narrower, explicitly-labeled
best-effort branch (parse stdout regardless of exit code purely to extract a
partial ID for *cleanup/kill* purposes on a known-failed spawn) is a
recognized, separate pattern — but it must not be conflated with the
success-determination path; success can never be inferred from stdout content
alone on a non-zero exit.

## 3. Provider credential scrubbing for multi-provider child-process spawns

- **Core risk (inheritance-by-default)**: every subprocess inherits the
  parent's full environment unless the parent actively constructs a scoped
  child environment — "an agent with `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
  `STRIPE_SECRET_KEY`... will pass all five keys to any subprocess it spawns"
  ([env.dev](https://env.dev/guides/env-vars-security)).
- **Allowlist over denylist**: OWASP-aligned guidance and general secure-coding
  consensus favor allowlisting the specific keys a child process needs over
  denylisting known-bad ones — a denylist only blocks variable *names* you
  already thought of, while new provider keys or renamed variables silently
  bypass it. "Filter using an allowlist of keys it's allowed to print, not a
  denylist" ([OWASP Secure Product Design Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Secure_Product_Design_Cheat_Sheet.html);
  [AquilaX / OWASP secrets summary](https://aquilax.ai/blog/owasp-secrets-management-environment-variables)).
- **Real-world glob-denylist failure mode**: a concrete regression in
  `vnx-orchestration` (multi-provider agent orchestrator) shows why broad
  denylist patterns fail — a `*_KEY` scrub-exception was too broad, so
  `OPENAI_API_KEY`/`SUPABASE_KEY`/etc. survived into a lane an external model
  could direct via `run_command`, reopening exactly the cross-credential-leak
  vulnerability the scrub was meant to close
  ([vnx-orchestration PR #1762](https://github.com/Vinix24/vnx-orchestration/pull/1762)).
  This is the closest found real-system precedent to "one provider's
  credentials must never leak into another provider's child process
  environment," and it argues for **explicit per-provider allowlists built from
  scratch per spawn**, not a shared denylist regex that must anticipate every
  provider's naming convention.
- **Least privilege**: OWASP's general least-privilege framing — "each process,
  service should access only the secrets it needs... scope secrets per service"
  — applied here means each spawned worker's environment should be constructed
  fresh with only that provider's required vars (e.g. build a MiniMax child env
  with only `MINIMAX_API_KEY` + pinned `ANTHROPIC_BASE_URL`/`ANTHROPIC_MODEL`,
  never inheriting `CLAUDE_CONFIG_DIR` or another host's Claude login), rather
  than inheriting the daemon's full env and trying to subtract the wrong keys.

**Synthesized principle**: build each child process's environment as an
explicit allowlisted set (provider's own required vars only, no inherited
credential vars by default), rather than starting from the daemon's full
environment and denylisting known-bad names — a denylist strategy has a
documented real-world failure mode (the `vnx-orchestration` `*_KEY` glob
regression) of under-scrubbing exactly the class of secret it exists to
protect. This matches this repo's own existing rule
(`CLAUDE.md` § CLI account scoping: "remove inherited variables and build a
scoped child environment... scrub inherited Claude and provider authentication
variables").

## Directory convention note

`docs/*.md` in `worktree_factory_repair_contract` follows a mix of
plain-topic names (`claim-system.md`) and dated research/investigation names
(`cli-fallback-audit-2026-06-12.md`, `multirepo-dispatch-investigation-2026-07-11.md`,
`pr-review-sweep-2026-07-06.md`); this file follows the dated-research
pattern at `docs/research-go-spawn-security-2026-09-18.md`.
