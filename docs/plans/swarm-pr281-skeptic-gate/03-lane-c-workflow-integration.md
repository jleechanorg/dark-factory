# Lane C — Workflow + Docs + Integration Review, PR #281 (head e70b1ec6)

## Scope

Files reviewed: `.github/workflows/skeptic-gate.yml` (328 lines, new),
`docs/skeptic-gate.md` (272 lines, new). Cross-referenced against
`runner/skeptic_gate.py`, `runner/skeptic_gate_cli.py` (for behavior the
workflow depends on), the repo's other workflows (`ci.yml`,
`evidence-gate.yml`, `hermes-pr-tag-listener.yml`), the 7-gate vocabulary
contract (`tests/scripts/test_auto_merge_guard_gate_vocabulary.sh`,
`daemon/factory-overlay.sh`), and the existing gate-consumption code
(`daemon/src/verifier.rs`, `daemon/src/adapters.rs`).

## Findings

### F1 — BLOCKER: `runs-on` is missing `fromJson()`; the job can never be scheduled

**Location**: `.github/workflows/skeptic-gate.yml:118`

```yaml
runs-on: ${{ vars.SELF_HOSTED_RUNNER_LABELS }}
```

`vars.*` is always a plain string context. Per the workflow's own header
comment (line 48) and `docs/skeptic-gate.md:251`, `SELF_HOSTED_RUNNER_LABELS`
is meant to hold a JSON array string, e.g. `'["self-hosted","self-hosted-mikey"]'`.
Without `fromJson(...)`, `runs-on` receives the **literal string**
`["self-hosted","self-hosted-mikey"]` (brackets, quotes, and all) as a
single runner label. No runner will ever carry that label, so the job sits
"Waiting for a runner to pick up this job..." indefinitely (or GitHub
rejects the run at schedule time if the var is empty). The gate can never
physically execute on any runner as written, independent of anything else
in the PR.

**Evidence**: compared `docs/skeptic-gate.md:116` ("private repo selector
via `fromJson(vars.SELF_HOSTED_RUNNER_LABELS || '["self-hosted","self-hosted-mikey"]')`")
against the actual YAML, which has neither `fromJson()` nor a `||`
fallback. The docs describe the intended behavior; the shipped YAML
doesn't implement it.

**Fix**: `runs-on: ${{ fromJson(vars.SELF_HOSTED_RUNNER_LABELS || '["self-hosted","self-hosted-mikey"]') }}`.

### F2 — BLOCKER: `$SELF_HOSTED_RUNNER_LABELS` is referenced in bash but never exported; unbound-variable crash under `set -u`

**Location**: `.github/workflows/skeptic-gate.yml:118` (declared source of
truth), `:120-133` (job-level `env:` block), `:139-157` ("Verify mandatory
pin vars are set" step)

The job-level `env:` block (lines 120-133) defines `SKEPTIC_REVIEWERS_JSON`,
`SKEPTIC_STATUS_CONTEXT`, `SKEPTIC_EXPECTED_ACTOR`, the three
`SKEPTIC_CODEX_*` vars, the three `SKEPTIC_GEMINI_*` vars, and
`SKEPTIC_REVIEWER_PATH` — but never `SELF_HOSTED_RUNNER_LABELS`. The
step's own `env:` (lines 139-140) only adds `GH_TOKEN`. Yet the step body
does:

```bash
set -euo pipefail
fail=0
for var in \
  "$SKEPTIC_CODEX_BIN" "$SKEPTIC_CODEX_VERSION" "$SKEPTIC_CODEX_SHA256" \
  "$SKEPTIC_GEMINI_BIN" "$SKEPTIC_GEMINI_VERSION" "$SKEPTIC_GEMINI_SHA256" \
  "$SELF_HOSTED_RUNNER_LABELS"
do
```

`set -u` is active (the `u` in `set -euo pipefail`). Expanding an unset
`$SELF_HOSTED_RUNNER_LABELS` at that `for var in ...` line trips bash's
"unbound variable" error immediately — the step dies with a raw bash
error, not the intended `[skeptic-gate] FATAL: required pin/label var is
empty` message the step was written to produce. Every single invocation
of this step fails this way (fail-closed in outcome, but not for the
reason the code claims, and it never reaches the SHA256/version/path
checks that follow).

Both F1 and F2 have to be fixed together: fixing F1 alone (adding
`fromJson()` to `runs-on`) still leaves this step permanently crashing on
`$SELF_HOSTED_RUNNER_LABELS` being unbound.

**Fix**: add `SELF_HOSTED_RUNNER_LABELS: ${{ vars.SELF_HOSTED_RUNNER_LABELS }}`
to the step's (or job's) `env:` block.

### F3 — HIGH: no real trigger exists anywhere; this is not yet automation per repo policy

**Location**: `.github/workflows/skeptic-gate.yml:70-104` (triggers);
absence confirmed across `.github/workflows/ci.yml`,
`.github/workflows/evidence-gate.yml`,
`.github/workflows/hermes-pr-tag-listener.yml` (none reference
`skeptic-gate.yml`).

The workflow only accepts `workflow_call` (needs a trusted caller that
doesn't exist yet) and `workflow_dispatch` (manual, and explicitly
documented as incapable of producing a satisfiable PASS — see
`docs/skeptic-gate.md:47-51`, "Self-PASS is impossible by design"). There
is no `pull_request` trigger (intentionally, per the documented
forgeability argument) and no caller workflow shipped in this PR or
present on `main`.

Per the global CLAUDE.md automation-completeness rule: *"Any PR that adds
an automation script must also add the trigger that calls it
automatically... A script with only manual invocation path is not
automation."* As merged, this workflow **never runs automatically for any
PR**. This is candidly disclosed in `docs/skeptic-gate.md:9-51` ("This PR
cannot self-bootstrap gate-7... Until the caller exists...") — the
disclosure is good practice and the chicken-and-egg problem
(`pull_request` is forgeable, `pull_request_target` can't bootstrap
before this file exists on `main`) is real and structurally sound
reasoning, not an excuse. But disclosure doesn't satisfy the policy: the
gap is real and unresolved.

**Practical consequence**: the "skeptic" key in the canonical 7-gate
vocabulary (`daemon/factory-overlay.sh:284` `REQUIRED_KEYS`,
`daemon/src/verifier.rs:60` `GateName::Skeptic`) will continue reading as
`Unknown` for every PR after this merges, exactly as it did before — no
regression, but no forward progress on gate-7 either, until a second PR
adds `.github/workflows/skeptic-caller.yml` (or equivalent). I found no
bead tracking that specific follow-up (`grep -i skeptic .beads/issues.jsonl`
turned up ~30 skeptic-related beads, closest is `jleechan-c0mo`
"auto-factory: bound permanent Unknown-only gate reports with explicit
escalation," which addresses how to *handle* the permanent-Unknown state,
not *adding the caller* that would resolve it).

**Recommendation**: either (a) ship the caller workflow in this same PR
(it's a small, mechanical addition once the target file's SHA is known
post-merge — acknowledged as impossible pre-merge by the doc's own
argument, so more precisely: land this PR, then immediately follow with
the caller PR before calling gate-7 "done"), or (b) file a tracked bead
for "land `.github/workflows/skeptic-caller.yml`" now so the gap doesn't
silently age out the way `jleechan-c0mo`-adjacent gaps have before.

### F4 — Integration with 7-green vocabulary: sound, reuses existing bridge, no new schema invented

`runner/skeptic_gate_cli.py` posts a GitHub commit status
(`state: pending|success|failure`, `context: "skeptic"`) plus a PR
comment — it does not emit the `pass|warn|fail|unknown` /
`{"verdict":...,"evidence":[...]}` JSON vocabulary directly. That's fine:
a pre-existing, independent consumer already bridges this.
`daemon/src/adapters.rs:1025-1044` scans check-runs/statuses for
`c.name.to_lowercase().contains("skeptic")` and synthesizes a PR comment
(`"skeptic check run: verdict: pass"` / `"...fail"`) from `c.bucket` /
`c.state`, which `daemon/src/verifier.rs` then consumes as
`GateName::Skeptic`. This PR's job name (`name: skeptic` at the workflow
level, job display name `Skeptic (SHA-bound PASS/FAIL)`) will produce a
check-run name containing "skeptic," so the existing bridge picks it up
without any new code. No new schema was invented; this is correct reuse.
Not a finding against this PR — confirmed working as intended (contingent
on F1/F2/F3 being fixed so the job ever actually runs and reports a real
state).

### F5 — MEDIUM: SHA-binding logic itself is sound (confirmed, not a finding)

Checked because it's the PR's stated purpose and PR #7888's history shows
stale-evidence gaming. The mechanism holds up:

- `trusted_code_sha` must be a 40-hex string (validated at
  `skeptic-gate.yml:226-242`), the checkout is pinned to that exact ref
  (`:244-259`), and a post-checkout step verifies `git rev-parse HEAD`
  equals it (`:261-275`) — defense-in-depth against `actions/checkout`
  ever silently resolving something else.
- The PR's *reviewed* head SHA is a separate value from the *code* SHA
  and is handled correctly as such: `skeptic_gate_cli.py` re-resolves the
  authoritative API head SHA (`get_pr_head_sha_via_api`) and refuses if
  the caller-supplied `pr_sha` disagrees (lines 629-638), then re-checks
  the API head again immediately before publishing (lines 832-840,
  "Pre-publish API head re-check") and aborts if it changed mid-run. A
  verdict computed against an old head cannot be replayed onto a newer
  one — both the pre-run and pre-publish checks would catch it.
- The published comment's `HEAD_SHA` field is read back for full
  byte-equality (`verify_published_comment`), so even a successful
  publish is re-verified before the gate is allowed to report `success`.

No defect found in this part of the design. This is the strongest part of
the PR.

### F6 — LOW: fail-closed relies on GH Actions job-failure semantics for some exception paths, not explicit handling

**Location**: `runner/skeptic_gate_cli.py:629` (`get_pr_head_sha_via_api`),
`:693` (`get_implementation_identity`)

Diff capture (`:641-690`) is explicitly wrapped in `try/except` and
force-publishes a `FAIL` comment + status on error. `get_pr_head_sha_via_api`
(the very first call in `main()`) and `get_implementation_identity` are
not similarly wrapped — an exception there produces an unhandled Python
traceback and a non-zero exit **before any custom `pending`/`failure`
commit status is ever posted** (the `pending` status isn't set until step
9a, deep in the flow, after both of these calls). In practice this still
fails closed at the outer layer: an unhandled exception makes the GH
Actions step (and therefore the job) fail, the native check-run reports
`FAILURE`, and `daemon/src/adapters.rs`'s `c.state == "FAILURE"` branch
(see F4) maps that to a fail verdict — so there's no false-PASS path here.
But it's inconsistent with the rest of the file's stated fail-closed
discipline (every other error path publishes an explicit, readable FAIL
comment) and means some failures show up only as a raw traceback in
Actions logs with no PR-visible explanation. Low severity because the
outer safety net holds; worth a follow-up to wrap the whole `main()` body
in a blanket `try/except` that force-publishes FAIL, matching the file's
own documented intent.

## Lane summary

6 findings: 2 blockers (F1 `runs-on` missing `fromJson()` — job can never
be scheduled; F2 `SELF_HOSTED_RUNNER_LABELS` referenced but never
exported to the step's env — unbound-variable crash under `set -u`), 1
high (F3 — no real trigger/caller exists anywhere in this PR or on
`main`; the workflow cannot run automatically for any PR yet, an
automation-completeness gap the PR's own docs candidly acknowledge but
don't close), 1 confirmed-clean integration point (F4 — reuses the
existing check-run-name bridge in `daemon/src/adapters.rs`, no new
vocabulary invented), 1 confirmed-clean design (F5 — SHA-binding /
stale-verdict rejection is correctly implemented with two independent
re-checks plus read-back), 1 low nit (F6 — two early exception paths lack
explicit fail-closed handling, though the outer job-failure semantics
cover the gap in practice).

Net: the SHA-binding and secret-hygiene design (the PR's actual stated
purpose) is well-built and the docs are unusually honest about the
bootstrap limitation. But as shipped, the workflow **cannot execute at
all** (F1+F2 are concrete, mechanical YAML bugs, not judgment calls) and
**would not be invoked even if it could** (F3). Both must be fixed before
this can be called a working gate-7, regardless of how sound the
underlying Python logic is.
