# PR #281 review — 3-lens adversarial verification (E6)

Verified against `pr281-review` head `e70b1ec6` (`.github/workflows/skeptic-gate.yml`,
`runner/skeptic_gate.py`, `runner/skeptic_gate_cli.py`, `tests/test_skeptic_gate.py`,
`docs/skeptic-gate.md`, `daemon/src/adapters.rs`, `daemon/src/verifier.rs`, all pulled
via `git show pr281-review:<path>` into `/tmp/` for line-accurate reproduction).

Default posture: REFUTE unless the citation reproduces exactly and the severity
claim survives independent reasoning. Every finding below was checked against
live file content, not against the lane authors' prose.

---

## Lane A — production code (`runner/skeptic_gate.py` + `skeptic_gate_cli.py`)

### F1 — BLOCKER: `_publish_failure` posts to issue #0 — CONFIRMED, DOWNGRADED to STRONG

**Evidence lens**: Reproduced exactly. `_publish_failure` (`skeptic_gate_cli.py:1039-1054`)
has signature `(repo, head_sha, body, context, description)` — no `pr_number` param.
`_pr_number_for_desc` (`:1057-1065`) regexes `"PR #(\d+)"` out of `description` and
returns `0` on no match. Both call sites (`:655`, `:683`) pass
`description=f"diff capture failed: {str(exc)[:80]}"`, which never contains
`"PR #<N>"` — confirmed by reading both call sites in `main()` verbatim. Grep for
`_publish_failure|_pr_number_for_desc` against `tests/test_skeptic_gate.py`
returns zero hits — no test exercises this path. CONFIRMED as written.

**Severity lens**: The lane's own writeup concedes "the commit status still
correctly flips to `failure` (so merge protection isn't bypassed)" — I verified
this is true: `set_commit_status(..., state="failure", ...)` in `_publish_failure`
runs unconditionally before the comment-post attempt, and it's a separate
try/except from the comment-post. So the actual production consequence is
narrower than "BLOCKER" implies: the fail-closed security guarantee (a bad diff
capture can never produce a false PASS) is intact; only the diagnostic PR
comment silently never appears on two specific early-exit paths (diff-capture
error, oversized diff). That's a real correctness bug in the observability
path, not a security/merge-protection defect. DOWNGRADE BLOCKER → STRONG. Still
must-fix (a maintainer debugging "why did my PR gate fail with no comment" will
be confused), but it does not block merge-worthiness of the gate's actual
guarantee the way Lane C's F1/F2 do.

**Design lens**: proposed fix (thread `pr_number` through, delete
`_pr_number_for_desc`) is correct and has no side effects — `args.pr_number` is
already in scope at both call sites.

### F2 — STRONG: redundant `_SHA_RE`/`_FULL_SHA_RE` pair — CONFIRMED, DOWNGRADED to NIT

**Evidence lens**: Reproduced exactly. `_SHA_RE` (7-64 hex) and `_FULL_SHA_RE`
(exactly 40 hex) both `findall` against the same `HEAD_SHA:` line
(`skeptic_gate.py:185-190`); `parse_verdict` already rejects unless
`len(shas) != 1 or len(short_shas) != 1` (`:289`), and separately requires
`re.fullmatch(r"[0-9a-f]{40}", sha)` on the `_FULL_SHA_RE` match (`:292-293`).
Given that gate, `short_shas` can only ever equal `shas` when the input passes
at all — confirmed by tracing the logic, not just trusting the inline comment
at `:330-332` that says so.

**Severity lens**: This is pure dead-weight code with zero behavioral risk —
no bug exists today and none is reachable via any adversarial input (the
40-hex gate already fully subsumes the 7-64-hex gate). DOWNGRADE STRONG →
NIT: it's a maintainability/audit-surface cleanup, not a design flaw with
production consequences.

**Design lens**: fix (delete `_SHA_RE`/`short_shas`, keep only `_FULL_SHA_RE`)
is correct, ~10 fewer lines, no behavior change.

### F3 — STRONG: sequential reviewer loop has no timeout margin — CONFIRMED, no change

**Evidence lens**: Reproduced exactly. `for reviewer_name, model in reviewers:`
(`skeptic_gate_cli.py:716`) calls `invoke_reviewer(...)` synchronously per
iteration; `invoke_reviewer`'s default `timeout: int = 900` (`:471`) is never
overridden at the call site (`:724-730`, no `timeout=` kwarg passed). Workflow
`timeout-minutes: 30` (`skeptic-gate.yml:119`). Default `SKEPTIC_REVIEWERS_JSON`
is `[["codex",""],["gemini","gemini-2.5-pro"]]` — exactly 2 reviewers. 2 × 900s
= 1800s = the *entire* 30-minute budget, before checkout, binary verification,
diff fetch, or publish/read-back get any share.

**Severity lens**: Confirmed as a genuine availability risk, correctly scored
STRONG (not a correctness bug — a timeout still fails closed via the outer job
timeout — but a real false-negative generator against well-formed PRs). No
change.

**Design lens**: `concurrent.futures.ThreadPoolExecutor` for the two
independent, already-sandboxed subprocess calls is sound — nothing in the loop
body depends on sequential ordering (no early exit, no shared mutable state
across iterations besides list-append). Confirmed safe to parallelize.

### F4 — NIT: value-equality list mutation — CONFIRMED, no change

**Evidence lens**: `SkepticResult` is `@dataclass(frozen=True)`
(`skeptic_gate.py:156-157`), and `per_reviewer[per_reviewer.index(r)] = r2`
appears 3 times in the CLI→identity binding loop (`skeptic_gate_cli.py:769,
793, 817`). Confirmed `list.index()` uses the dataclass's generated `__eq__`
(field-value equality), not identity. Confirmed today's replacement objects
always differ by `reviewer` field in every reachable path, so no live bug —
matches the lane's own characterization exactly.

**Severity/Design lens**: NIT is correct — no live bug, `enumerate()` fix is
free and removes the (currently latent) foot-gun. No change.

### F5 — NIT: both files at/over 1k-line boundary — CONFIRMED, no change

`wc -l` on the extracted files: `skeptic_gate.py` = 1000 exactly,
`skeptic_gate_cli.py` = 1119. Confirmed exactly as stated.

### Architecture note (standalone system, not a DOT node) — CONFIRMED, no change

Confirmed via `git diff --stat origin/main...pr281-review -- runner/`: both
files are wholly new (2119 insertions, 0 deletions, 0 modifications to any
existing `runner/*.py`) — no wiring into `TYPE_REGISTRY`/`REGISTRY`. This is a
correctly-flagged note, not a defect (lane A itself says so); confirmed
accurate.

### Code-standards sub-findings — spot-checked, CONFIRMED
- `git grep -n "gh_api\|def _gh("` against pre-PR `runner/` — confirmed no prior
  wrapper existed (the two files are wholly new per the diff-stat above), so
  "justified, not a violation" holds.
- `COMMIT_PREFIX_TO_IDENTITY` exists at `skeptic_gate.py:110` as stated,
  ZFC-exemption reasoning (fixed lookup table, not semantic classification)
  holds up under inspection — confirmed no keyword/intent routing.
- "9 distinct post-audit-comment callouts": `grep -c "Per post-audit comment"`
  → 2 in `skeptic_gate.py` + 7 in `skeptic_gate_cli.py` = 9. Confirmed exact.
- `jleechan-c0mo` bead exists in `.beads/issues.jsonl` with the exact title
  quoted ("auto-factory: bound permanent Unknown-only gate reports with
  explicit escalation") — confirmed it addresses handling permanent-Unknown
  state, not adding a caller trigger, matching the lane's characterization.

### Lane A tally after verification
| ID | Lane verdict | My verdict |
|---|---|---|
| F1 | Blocker | **CONFIRMED, downgraded → Strong** |
| F2 | Strong | **CONFIRMED, downgraded → Nit** |
| F3 | Strong | CONFIRMED, unchanged |
| F4 | Nit | CONFIRMED, unchanged |
| F5 | Nit | CONFIRMED, unchanged |

---

## Lane B — `tests/test_skeptic_gate.py`

### F1 — dead ternary with typo'd attribute — CONFIRMED, no change

`tests/test_skeptic_gate.py:100` reproduces byte-for-byte:
```python
assert parsed.reviewr_identity if False else parsed.reviewer_identity == "codex"
```
`grep -n "reviewr_identity"` → exactly this one line. Real field is
`reviewer_identity` (`skeptic_gate.py:137`). Confirmed dead code, harmless,
nit-level. No change.

### F2 — coverage asymmetry on the GitHub-API glue — CONFIRMED, no change (and cross-confirms Lane A F1)

**Evidence lens**: `grep -n "get_pr_head_sha_via_api|post_or_update_comment|
read_back_comment|find_existing_bot_comment|_publish_failure|
_pr_number_for_desc|set_commit_status|gh_api" tests/test_skeptic_gate.py`
reproduces the lane's claim: `find_existing_bot_comment` → **zero** occurrences
anywhere in the test file (confirmed via separate `grep -c` = 0).
`post_or_update_comment` appears only as a `monkeypatch.setattr` target (4
occurrences, all assignments, never a direct call to the real function).
`_publish_failure`/`_pr_number_for_desc` → zero occurrences, confirming these
functions — which carry the confirmed Lane A F1 bug — have no test coverage
at all.

Confirmed the specific claim about `_cli_argv()`: the shared helper
(`:825-839`) unconditionally appends `"--dry-run"` to `base` with no
conditional logic, and `_publish_failure` is only called
`if not args.dry_run:` at both call sites (verified in Lane A). So
`test_adversarial_diff_oversize_fails_closed` (which builds its argv via
`_cli_argv()` with no override) structurally cannot reach `_publish_failure`
— confirmed by reading the test body directly (no `dry_run` override present).

Also confirmed the two `gh_api`-mocking tests
(`test_get_implementation_identity_uses_head_sha_direct_lookup` and
`..._falls_back_to_pr_commits_when_head_missing`) replace `cli_mod.gh_api`
wholesale with a fake, exercising `get_implementation_identity`'s branching
logic but never `gh_api`'s own subprocess/JSON-parsing implementation.

**Severity lens**: MODERATE is the right call — the security-critical
deterministic core (`parse_verdict`, `bind_reviewer_identity`,
`verify_provenance`) is genuinely tested against independent literals (no
self-certification issue there, confirmed). The untested half is the
GitHub-API I/O glue, and this lane's own gap-finding is validated by the fact
that Lane A independently found a real, live bug (`_pr_number_for_desc`
returning 0) sitting exactly inside the untested surface this finding flags.
That's a real-world instance of the exact failure mode F2 warns about — I
treat this as reinforcing evidence for MODERATE, not grounds to upgrade to a
blocker (the bug found is an observability gap, not a security bypass, per
Lane A's corrected severity).

### F3 — 7× boilerplate repetition — CONFIRMED, no change

All 7 named tests (`test_cli_forced_pass_with_both_reviewers`,
`test_cli_forced_fail_with_missing_reviewer`,
`test_cli_provenance_fails_self_review`,
`test_adversarial_status_failure_is_fail_closed`,
`test_status_publish_order_pending_then_success`,
`test_status_readback_mismatch_overwrites_to_failure`,
`test_status_overwritten_failure_never_becomes_success`) confirmed to exist at
the cited line numbers via `grep -n "^def test_..."`. Fixture-extraction
suggestion is sound DRY advice, not a design regression. No change.

### Lane B tally after verification
| ID | Lane verdict | My verdict |
|---|---|---|
| F1 | Nit | CONFIRMED, unchanged |
| F2 | Moderate | CONFIRMED, unchanged (cross-confirmed by Lane A F1) |
| F3 | Nit | CONFIRMED, unchanged |

---

## Lane C — workflow + docs + integration

### F1 — BLOCKER: `runs-on` missing `fromJson()` — CONFIRMED, no change

**Evidence lens**: `.github/workflows/skeptic-gate.yml:118` reproduces exactly:
```yaml
runs-on: ${{ vars.SELF_HOSTED_RUNNER_LABELS }}
```
No `fromJson()`, no `||` fallback. `docs/skeptic-gate.md:116` documents the
*intended* form: `fromJson(vars.SELF_HOSTED_RUNNER_LABELS || '["self-hosted",
"self-hosted-mikey"]')`. Confirmed the shipped YAML and the docs diverge
exactly as the lane states.

**Severity lens**: Reasoned through GitHub Actions semantics independently:
`vars.*` always resolves to a plain string in the `${{ }}` context;
without `fromJson`, a JSON-array-shaped string like
`["self-hosted","self-hosted-mikey"]` (including the brackets and quotes) is
interpreted as one single, literal runner label. No self-hosted runner is
ever registered with that exact bracket-and-quote-containing label string, so
the job has no eligible runner and sits queued indefinitely. This is
independently reproducible reasoning, not a repetition of the lane's claim —
CONFIRMED BLOCKER, unambiguous, no downgrade. A job that can never be
scheduled is about as blocking as a finding gets.

**Design lens**: fix is exactly what the docs already describe as intended —
zero design risk, this is a shipped/intended mismatch, not a debatable
judgment call.

### F2 — BLOCKER: unbound `$SELF_HOSTED_RUNNER_LABELS` under `set -u` — CONFIRMED, no change

**Evidence lens**: Reproduced exactly. Job-level `env:` block
(`skeptic-gate.yml:120-133`) defines `SKEPTIC_REVIEWERS_JSON`,
`SKEPTIC_STATUS_CONTEXT`, `SKEPTIC_EXPECTED_ACTOR`, the three
`SKEPTIC_CODEX_*`, the three `SKEPTIC_GEMINI_*`, and `SKEPTIC_REVIEWER_PATH` —
confirmed `SELF_HOSTED_RUNNER_LABELS` is absent from this list. The step's own
`env:` (`:139-140`) adds only `GH_TOKEN`. The step body
(`:142-157`) opens with `set -euo pipefail` and then does
`for var in ... "$SELF_HOSTED_RUNNER_LABELS"` — confirmed byte-for-byte.

**Severity lens**: Independently reasoned: under bash `set -u`, referencing a
variable that was never assigned in the shell's environment (not merely
empty-but-set) triggers "unbound variable" and the shell exits immediately —
and because the `for var in <list>` construct expands *all* list elements up
front before the loop body runs, expansion of the unset
`$SELF_HOSTED_RUNNER_LABELS` element crashes the step before any of the
intended `[ -z "$var" ]` checks execute, including the checks for the
already-defined `SKEPTIC_CODEX_*`/`SKEPTIC_GEMINI_*` vars. Confirmed:
this is a real crash, independent of F1, though as the lane notes, F1
currently masks it (the job never gets scheduled to run this step at all).
CONFIRMED BLOCKER, correctly identified as a co-requisite fix with F1 — not a
reason to lower severity, since it will immediately manifest the moment F1 is
fixed in isolation.

**Design lens**: fix (add `SELF_HOSTED_RUNNER_LABELS:
${{ vars.SELF_HOSTED_RUNNER_LABELS }}` to env) is correct and minimal.

### F3 — HIGH: no caller/trigger exists — CONFIRMED, no change

Confirmed via `on:` block (`skeptic-gate.yml:70-104`): only `workflow_call`
and `workflow_dispatch`, no `pull_request`. Confirmed via
`git grep -n "skeptic-gate.yml" -- .github/workflows/` that the only match
across all 4 workflow files in the repo is the file's own header comment
(`:10`, describing the intended `uses:` syntax) — no `ci.yml`,
`evidence-gate.yml`, or `hermes-pr-tag-listener.yml` actually calls it.
Confirmed via `.beads/issues.jsonl` grep that no bead named
"skeptic-caller"/"skeptic_caller" exists, and the closest match
(`jleechan-c0mo`) addresses handling permanent-Unknown gate state, not
shipping the caller workflow — matches the lane's characterization exactly.
HIGH (not Blocker, since F1/F2 already fully block execution independent of
this gap) is the right severity call. No change.

### F4 — confirmed-clean integration point — CONFIRMED, no change

`daemon/src/adapters.rs:1026`: `if c.name.to_lowercase().contains("skeptic")`
confirmed present, synthesizing `"skeptic check run: verdict: pass"` /
`"...fail"` from check-run state. `daemon/src/verifier.rs` confirmed defines
`GateName::Skeptic` (`:32`, `:60`). Correctly scored as sound reuse, not a
defect. No change.

### F5 — confirmed-clean design (SHA-binding) — CONFIRMED, no change

Reproduced all three cited mechanisms: (1) API-vs-caller SHA equality check
at `skeptic_gate_cli.py:629-638` (`api_head` vs `event_sha`, `return 2` on
mismatch); (2) pre-publish re-check at `:831-840` (`api_head_2` re-fetch,
abandons publish on mid-run SHA drift); (3) `verify_published_comment`
read-back call at `:925`. All three confirmed present and functioning as
described — the SHA-binding design is genuinely sound. No change.

### F6 — LOW: two early exception paths unwrapped — CONFIRMED, no change

Confirmed `get_pr_head_sha_via_api(repo, args.pr_number)` (`:629`) and
`get_implementation_identity(...)` (`:693`) both run without a surrounding
`try/except`, unlike the explicitly-wrapped diff-capture block
(`:641-690`). Confirmed the `pending` status isn't published until step 9a
(`:865-871`), well after both unwrapped calls — so an exception there
produces a raw traceback with no custom status/comment, relying on the outer
GH Actions job-failure semantics (independently confirmed via F4's bridge:
`adapters.rs` maps check-run `FAILURE` state to a fail verdict) to still fail
closed. LOW is correct — no false-PASS path exists, purely a
"traceback-only, no readable PR comment" UX gap. No change.

### Minor accuracy note (not a finding)

Lane C's own scope line describes `.github/workflows/skeptic-gate.yml` as
"328 lines, new"; `wc -l` on the extracted file returns 327 (matching Lane
A's citation of the same file). Trivial off-by-one in Lane C's own scope
statement, unrelated to any finding's substance — noting for completeness,
not counted against the lane.

### Lane C tally after verification
| ID | Lane verdict | My verdict |
|---|---|---|
| F1 | Blocker | CONFIRMED, unchanged |
| F2 | Blocker | CONFIRMED, unchanged |
| F3 | High | CONFIRMED, unchanged |
| F4 | Confirmed-clean (not a defect) | CONFIRMED, unchanged |
| F5 | Confirmed-clean (not a defect) | CONFIRMED, unchanged |
| F6 | Low | CONFIRMED, unchanged |

---

## Final tally

| Lane | Finding | Original severity | Verified severity | Disposition |
|---|---|---|---|---|
| A | F1 `_publish_failure` posts to #0 | Blocker | **Strong** | Confirmed, downgraded |
| A | F2 redundant SHA regex | Strong | **Nit** | Confirmed, downgraded |
| A | F3 sequential reviewer timeout budget | Strong | Strong | Confirmed |
| A | F4 value-equality list mutation | Nit | Nit | Confirmed |
| A | F5 1k-line boundary | Nit | Nit | Confirmed |
| B | F1 dead ternary/typo | Nit | Nit | Confirmed |
| B | F2 GitHub-API coverage asymmetry | Moderate | Moderate | Confirmed |
| B | F3 7× test boilerplate | Nit | Nit | Confirmed |
| C | F1 `runs-on` missing `fromJson()` | Blocker | Blocker | Confirmed |
| C | F2 unbound `$SELF_HOSTED_RUNNER_LABELS` | Blocker | Blocker | Confirmed |
| C | F3 no trigger/caller exists | High | High | Confirmed |
| C | F6 two unwrapped exception paths | Low | Low | Confirmed |

**Confirmed (as originally scored): 9. Confirmed but severity-downgraded: 2
(Lane A F1 Blocker→Strong, Lane A F2 Strong→Nit). Refuted: 0.**

Every citation across all three lane documents reproduced exactly against the
live `pr281-review` (e70b1ec6) source — no phantom line numbers, no
misquoted code, no fabricated grep results. The two downgrades are severity
judgment calls (both findings are real, correctly diagnosed, and correctly
fixed by the lanes' own proposed patches) — neither is a refutation of
substance. The workflow cannot execute as shipped (Lane C F1+F2, both
independently confirmed as hard mechanical bugs, not judgment calls) and even
if it could, nothing calls it yet (Lane C F3, confirmed via grep across all
4 workflow files and the beads store). Lane A's one real blocker-labeled bug
is real but narrower in blast radius than "blocker" implies — merge
protection itself is never compromised, only a diagnostic comment on two
early-exit paths silently fails to post.
