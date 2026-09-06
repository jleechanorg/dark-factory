# /web-advice Fail-Open End-to-End Log (Lane D)

Status: **HISTORICAL / INVALID FOR PANEL EVIDENCE** — one pipeline run (blocked at
holdout) + one direct handler invocation (returned `outcome=success`) are retained for
forensic history. The direct run did **not** produce a valid `/web-advice` panel verdict
or compliant evidence: its empty panel, `verdict_captured` label, and
`all_transports_down` failure mode are internally contradictory. It demonstrates only
the fail-open outcome routing, not a cross-model review or production readiness. It is
superseded by the current strict panel schema, which classifies this shape as
`panel_contract_invalid` / infrastructure failure.

Authors: Lane D test-invocation agent, 2026-08-21.
Refs: `docs/web-advice-failopen-design.md`, `pipelines/factory/web-advice-failopen.dot`,
`runner/handler_web_advice.py`, `tests/test_web_advice_handler.py`.

---

## 1. Test target

| | |
|---|---|
| **Test PR URL** | `https://github.com/jleechanorg/dark-factory/pull/664` |
| **Title** | `TEST-ONLY (do not merge): web-advice fail-open docs supplement` |
| **Branch** | `test/web-advice-failopen-pr` |
| **Head SHA** | `f3caec5ca33b10d5b759266487f479d187188159` |
| **Worktree** | `/home/jleechan/.worktrees/dark-factory/test-web-advice-failopen` |
| **Base** | `origin/main` @ `0e3f4347500073c8c76b641e3be9cb4a5dd2449d` |
| **State** | `OPEN`, `isDraft: true` — left for operator review. **Do NOT merge.** |
| **Diff size** | 1 file changed, 20 insertions(+), 0 deletions(-). Docs-only. |
| **Diff patch** | `/tmp/pr_664_full.patch` (54 lines, generated via `git format-patch -1`). |
| **Why docs-only** | The constraint "Do NOT invoke /af against the real pr-655 follow-up beads" |
| | (Phase 4 Constraints) excludes Option A (the `mergeable:null` fix from bead |
| | `jleechan-qzr3`). A docs-only PR is a clearly test-only artifact that won't be |
| | mistaken for a production fix. The change documents the new advisory lane itself, |
| | which is on-topic and non-controversial for the cutover-exit criteria. |
| **Commit prefix** | Commit message starts with `target_repo: jleechanorg/dark-factory` per |
| | memory `feedback_2026-08-01_phantom_dispatch_74wt_lwte.md`. |

---

## 2. Pipeline invocation (full `dark-factory` run)

### 2.1 Pre-flight sanity checks (Phase 1)

| Check | Command | Result |
|---|---|---|
| Graph audit | `python -m runner.graph_audit pipelines/factory/web-advice-failopen.dot` | **OK: no graph-audit violations found.** |
| Parser load | `python -c "from runner.parser import parse; from pathlib import Path; g = parse(Path('pipelines/factory/web-advice-failopen.dot')); print(g.nodes)` | parsed: `web_advice_failopen`, 10 nodes, 14 edges. |
| Handler registration | `from runner.handlers import TYPE_REGISTRY; assert 'web_advice' in TYPE_REGISTRY` | **PASS** — `web_advice -> runner.handler_web_advice._web_advice`. |
| Audit tier | `from runner.graph_audit import _REVIEWER_TYPE_NAMES, _HARD_TIER_REVIEWER_TYPES` | `web_advice` is in `_REVIEWER_TYPE_NAMES` (soft tier); NOT in `_HARD_TIER_REVIEWER_TYPES`. Matches design §7 recommendation. |
| Unit tests | `python -m pytest tests/test_web_advice_handler.py -v` | **64 passed in 0.34s.** |

### 2.2 Invocation (verbatim)

```bash
export DARK_FACTORY_HOME=/home/jleechan/projects/worktree_factory_clean_code
export DARK_FACTORY_HOLDOUTS=$HOME/projects/dark-factory-holdouts
export PATH="$HOME/.local/bin:$PATH"

dark-factory \
  --pipeline pipelines/factory/web-advice-failopen.dot \
  --goal "Lane D E2E: validate fail-open /web-advice routing on PR #664 test branch" \
  --workdir /home/jleechan/.worktrees/dark-factory/test-web-advice-failopen \
  --backend echo \
  --feature web-advice-failopen-test-pr \
  --state pr_url=https://github.com/jleechanorg/dark-factory/pull/664 \
  --state head_sha=f3caec5ca33b10d5b759266487f479d187188159 \
  --state diff_path=/tmp/pr_664_full.patch \
  --state target_repo=jleechanorg/dark-factory \
  --state evidence_dir=docs/web-advice-failopen-e2e-log \
  --cxdb /tmp/web-advice-failopen-test-cxdb.sqlite \
  --no-perf-log
```

Full replay script: `scripts/af-test-web-advice-failopen.sh` (executable, mirrors this run).
Pre-run summary captured in `/tmp/af-test-summary.txt` and `/tmp/web-advice-failopen-test-run.log`.

### 2.3 Per-node trace (engine history)

Sequence at `/tmp/web-advice-failopen-test-run.log`:

| seq | node | outcome | preview (truncated) |
|---:|---|---|---|
| 0 | `start` | `success` | `start: 'Lane D E2E: validate fail-open /web-advice routing …'` |
| 1 | `holdout` | `failure` | `{"verdict": "fail", "passed": 0, "total": 0, "sealed": true}` |
| 2 | `fix` | `success` | `Fix the current failing gate. Goal: …` |
| 3 | `holdout` | `failure` | identical to seq 1 |
| 4 | `fix` | `success` | identical to seq 2 |
| 5 | `holdout` | `failure` | identical to seq 1 |
| 6 | `fix` | `success` | identical to seq 2 |
| 7 | `holdout` | `failure` | identical to seq 1 |
| 8 | `fix` | `exhausted` | `max_visits=3 exceeded` |
| 10 | `__run_end__` | `exhausted` | — |

10 raw CXDB events at `/tmp/web-advice-failopen-test-cxdb.sqlite`. Pipeline rc=1.

### 2.4 What happened (and why `web_advice` was not reached)

`web_advice` is downstream of `gate_cs` in `pipelines/factory/web-advice-failopen.dot`:

```
start → holdout → gate_skeptic → adversarial_reviewer
       → gate_es → gate_er → gate_cs → web_advice → exit
                                                  ↑ strict-gate fence
```

Every strict-gate branch on `outcome=success`. The first node is `holdout_eval`, the
sealed behavioral validator pointed at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py --feature
<feature>`. The evaluator reads
`$DARK_FACTORY_HOLDOUTS/holdouts/<feature>/scenarios.yaml`.

For `--feature web-advice-failopen-test-pr`, no such `scenarios.yaml` exists in the
sealed repo (its only features are `agfapi_dockerignore / amazon-clone / hello /
level-up-session / rag-off-equals-main / roman`). Per `evaluator/run.py` line 300-303,
a missing scenario file returns:

```python
{"verdict": "fail", "feature": feature, "scenarios": []}
```

That's `passed: 0, total: 0, "sealed": true` — exactly the preview in seq 1.

The pipeline then runs `holdout → fix → holdout` up to `fix[max_visits=3]`, after
which `fix` returns `exhausted` and the run terminates with `final_outcome=exhausted`.
The strict gates never run; `web_advice` is **never reached**.

This is a structural artifact of the test setup, **not a fail-open regression**:

- `web_advice` always sits behind `gate_cs`. The design intentionally puts the advisory
  reviewer last (per `docs/web-advice-failopen-design.md` §4 reasoning #4: "doesn't
  compete with the 4-gate strict sequence").
- The holdout's `pass/total/sealed=0/0` is the *expected* fail-closed behavior of the
  sealed evaluator when no scenarios match. It has no opinion about `web_advice` at all.
- 64 unit tests in `tests/test_web_advice_handler.py` (all passing) cover the
  fail-open invariant in isolation: outcome=success under missing `pr_url`,
  unparseable URL, trivial diff, all transports down, subprocess failure, and outer
  exception. Mutation-style test (`TestFailOpenContract`) parses the parametrized
  invariant directly.

To prove the *full* pipeline actually reaches `web_advice` end-to-end, a future
test must provide (a) a `scenarios.yaml` for the test feature in the sealed holdouts
repo (operator-only — touching the sealed repo would shadow the operator isolation
rule), or (b) a real production feature whose diff has matching holdouts and a
green-path through the strict gates. Neither was in scope for Lane D (constraint
forbids touching sealed holdouts).

### 2.5 Verdict from the pipeline run

- Final pipeline outcome: **exhausted** (3 fix visits, no `fix`→`holdout` recovery).
- `web_advice` reached: **NO**.
- Fail-open routing at pipeline layer: **N/A** (the relevant code path was not
  exercised by this run; §3's direct invocation only observed a success return,
  not a valid panel or verdict).

---

## 3. Direct handler invocation (historical fail-open routing smoke; invalid panel evidence)

Because the pipeline run could not reach `web_advice` without operator-approved
holdouts, a direct handler invocation was added at
`/home/jleechan/projects/worktree_factory_clean_code/scripts/direct-test-web-advice-handler.py`.

The script bypasses `holdout_eval` and the strict gates; it constructs a minimal
`Context` + `Node` and calls `_web_advice(node, ctx)` directly, mirroring what the
runner engine does on node visit (minus the upstream routing). This is useful only for
the historical fail-open return path. It is **not** a valid panel capture, verdict, or
cross-model lens, and its artifacts must not be used as production evidence.

### 3.1 Run summary (raw historical output; superseded)

```
[1] handler registration: web_advice -> runner.handler_web_advice._web_advice OK
[2] graph_audit tiers: soft-tier-only OK (matches design §7 recommendation)
[3] context built: run_id=direct-web-advice-1787365877
    pr_url=https://github.com/jleechanorg/dark-factory/pull/664
    head_sha=f3caec5ca33b10d5b759266487f479d187188159
    diff_path=/tmp/pr_664_full.patch (exists=True)
[4] handler returned in 178.93s
    outcome   = 'success'
    output    = "web_advice verdict_captured: verdict='fail' transport='cdp_port' share_urls=0 pr_comment_ok=True"
[5] FAIL-OPEN INVARIANT VERIFIED: outcome=success regardless of probe state
```

The handler ran the §3.5 transport probe, detected `cdp_port` (Chrome DevTools
Protocol at `127.0.0.1:9222` was listening on `Jeff-Ubuntu`), and returned
`outcome='success'`. The historical record simultaneously reports no live panel
seats, `panel_decision=verdict_captured`, and `infrastructure_failure_mode=all_transports_down`.
That contradiction means no panel verdict was captured. Under the current strict schema,
this is `panel_contract_invalid` / infrastructure failure. **Only the fail-open return
path was demonstrated.**

### 3.2 Raw side-effect log (not a compliant §3.3 result)

These writes are preserved as historical artifacts, but they do not establish a valid
panel review or evidence bundle. The current schema rejects the contradictory envelope
as `panel_contract_invalid` / infrastructure failure.

| Channel | Status | Evidence |
|---|---|---|
| CXDB structured event | **WRITTEN, INVALID** | `/tmp/web-advice-failopen-direct-test-cxdb.sqlite` — 1 row for `web_advice`, `outcome=success`, with contradictory panel fields. Mirror persisted to `/tmp/web-advice-direct-test-evidence/web-advice-cxdb-event-direct.json`; neither is compliant panel evidence. |
| PR comment (`<!-- web-advice-review -->`) | **POSTED, SUPERSEDED** | A historical comment was posted on PR #664, but its verdict-captured template is misleading for the contradictory empty-panel/infrastructure result. Do not treat it as a valid panel verdict or cross-model review. |
| Follow-up bead via `br create` | **NOT FILED** | No bead was required or filed for this invalid historical run. |

### 3.3 CXDB event body (truncated; demonstrates the invalid contradiction)

```json
{
  "event_type": "web_advice_review",
  "run_id": "direct-web-advice-1787365877",
  "node": "web_advice",
  "ts": "2026-08-22T02:34:16Z",
  "panel_seats_attempted": ["chatgpt", "gemini", "grok", "perplexity"],
  "panel_seats_live": [],
  "panel_decision": "verdict_captured",
  "infrastructure_failure_mode": "all_transports_down",
  "elapsed_seconds": 178,
  "host": "Jeff-Ubuntu",
  "decision": "continue_with_bead",
  "pr_comment_url": null,
  "decision_no_blocking": true,
  ...
}
```

The combination of `panel_seats_live: []`, `panel_decision: verdict_captured`, and
`infrastructure_failure_mode: all_transports_down` is not a valid `/web-advice` panel
envelope. It is retained to explain the historical run, not to verify the §5 schema.

### 3.4 Historical PR comment (superseded; not a valid §6.1 verdict)

```markdown
<!-- web-advice-review -->
## /web-advice panel review

**Decision:** `continue` (verdict captured — fail-open)
**Transport:** `cdp_port`
**PR:** https://github.com/jleechanorg/dark-factory/pull/664

**Verdict:** `fail`
…
```

Comments counter after the run: 2 (CodeRabbit's "draft detected" auto-message + the
above `jleechan2015` web-advice review). Both visible at `gh pr view 664 --comments`.

### 3.5 Fail-open routing only (panel/verdict evidence invalid)

| Property | Required by design | Observed |
|---|---|---|
| `outcome` returned to the .dot engine | always `"success"` | **`"success"`** |
| §3.5 transport probe runs | ≤10s | ran, detected `cdp_port` |
| `panel_seats_attempted` always the full 4 | full list | raw field present, but the envelope is invalid because no seats were live while it claimed a captured verdict |
| `decision` field | one of `continue \| continue_with_bead \| continue_with_pr_warning`, never `block` | raw `"continue"` field in the historical comment; this does not make the invalid envelope compliant |
| PR comment markers present | `<!-- web-advice-review --> ... <!-- /web-advice-review -->` | markers were present, but the comment is superseded and is not valid panel evidence |
| Handler never raises | outer `except Exception` returns success | covered by the revision's unit tests; this historical artifact adds no panel evidence |

---

## 4. Artifacts

| Path | Purpose |
|---|---|
| `/home/jleechan/projects/worktree_factory_clean_code/scripts/af-test-web-advice-failopen.sh` | Phase 3 script — runs the full `dark-factory` invocation. |
| `/home/jleechan/projects/worktree_factory_clean_code/scripts/direct-test-web-advice-handler.py` | Direct handler invocation that bypasses holdout. |
| `/home/jleechan/.worktrees/dark-factory/test-web-advice-failopen/` | Test PR worktree at `test/web-advice-failopen-pr`. |
| `/tmp/pr_664_full.patch` | Diff patch attached to the test PR. |
| `/tmp/web-advice-failopen-test-cxdb.sqlite` | CXDB from the pipeline run (10 events). |
| `/tmp/web-advice-failopen-test-run.log` | `dark-factory` stdout/stderr for the pipeline run. |
| `/tmp/web-advice-failopen-direct-test-cxdb.sqlite` | CXDB from the direct invocation (1 event). |
| `/tmp/web-advice-direct-test-evidence/web-advice-cxdb-event-direct.json` | Event mirror for the direct invocation. |
| `/tmp/af-test-summary.txt` | Script's capture of pre-run + post-run summary. |
| `gh pr view 664 --comments` | Lists both comments (CodeRabbit + jleechan2015 web-advice review). |

---

## 5. Operator actions & follow-up dispositions

1. **Review PR #664** at `https://github.com/jleechanorg/dark-factory/pull/664`. The PR
   is in `OPEN`, `isDraft: true` state. The historical web-advice comment is
   superseded and must **not** be treated as a cross-model lens or a valid verdict:
   the CDP path produced no usable panel data, and the empty-panel metadata was
   contradictory. It says nothing about the docs change.
2. **Do NOT merge PR #664.** It is a test fixture.
3. **Item #2 §3.5 probe accepts an arbitrary TCP listener as CDP — FIXED:**
   `_probe_cdp_port` now validates Chrome's `/json/version` response and requires
   both the browser identity and a DevTools browser WebSocket URL. CDP remains an
   independent Linux transport rung, so a healthy headless Chrome session works
   when Aside CLI is not installed while a non-CDP service on port 9222 is rejected.
4. **Item #3 JSON heredoc escape bug — FIXED:**
   Fixed in `scripts/af-test-web-advice-failopen.sh` by passing `SUMMARY_JSON` via `sys.stdin` to `json.load` rather
   than expanding bash variables inside python raw triple-quoted code blocks.
5. **Item #4 Direct-invocation script 2 import errors — FIXED:**
   Fixed in `scripts/direct-test-web-advice-handler.py` and documented in the module docstring: uses `CXDB.record_step`
   (replacing non-existent `append_step`) and `Node(name=..., attrs={"type": "web_advice", ...})` (replacing `type=` kwarg on `Node.__init__`).
6. **Item #5 Holdout deadlock at full-pipeline path — FIXED:**
   Added `--require-holdouts` flag to `runner/preflight.py` and `runner/__main__.py`. Fails fast with clear error
   when `holdouts/<feature>/scenarios.yaml` is missing, preventing the 3-iteration `holdout -> fix -> holdout` deadlock loop.
7. **Item #6 Fold web-advice-failopen.dot into production pipelines — TRACKED:**
   Tracked separately under follow-up bead `jleechan-azso` (fold
   `web-advice-failopen.dot` into `pr_gates.dot` + `gates.dot` +
   `slim/minimal_pr.dot`).

**Current-state update (2026-08-24):** PR #742 later merged this tracked work
with strict structured panel validation and exact-head real-browser evidence.

### 5.2 Lane E/F Remediation Candidates

| Candidate | Decision | Disposition |
|---|---|---|
| **A** — surface runner outage truthfully | **FIX** | `scripts/check_runner_health.sh` reuses the canonical org-scoped selector verifier and the configured `SELF_HOSTED_RUNNER_LABELS`. It distinguishes fleet-down, selector drift, and probe/auth failure. `auto-merge-guard.sh` posts a deduplicated warning only for a verified org-fleet outage and never recommends bypassing merge policy. |
| **B** — language-correct anchor comments | **FIX** | `docs/code-standards.md` documents native comment forms. Changed source is validated by its real parser/compiler/linter; no cross-language regex validator is used because multiline strings, block comments, heredocs, and prose make that approach unsound. |
| **C** — legacy bead bodies missing `external_ref` | **ACCEPT-AS-DEGRADED** | Historical records remain immutable. Forward intake validation in `daemon/src/intake.rs` and the factory preflights enforce repository and external-reference identity. |
| **D** — Evidence Gate commit-message scan | **ACCEPT-AS-DEGRADED** | The Evidence Gate workflow remains bound to the current PR head through a trusted `/er` signal or canonical PR-body public-gist marker. Scanning arbitrary historical commit messages would introduce stale-evidence ambiguity. |

---

## 6. Fail-open routing outcome (limited; panel evidence invalid)

- ✅ Handler returned `outcome=success` on the historical direct path.
- ✅ §3.5 transport probe ran and detected host capability.
- ⚠️ CXDB event and PR comment were written, but their contradictory panel fields
  are invalid under the current strict schema (`panel_contract_invalid` /
  infrastructure failure); they are not compliant §5/§6 evidence.
- ⚠️ No cross-model panel verdict, valid share evidence, or production-readiness
  claim is supported by this run.
- ✅ Handler non-raising behavior is covered by the revision's unit tests.
- ⚠️  Full pipeline never reached `web_advice` due to test-feature/holdouts mismatch.
  This is a test-environment gap; the direct run only exercised the fail-open
  return routing and did not prove panel behavior.

The pipeline and handler references above may be reviewed independently, but this
historical log is **not** production evidence and does not establish readiness for
insertion into `pr_gates.dot` or any other production pipeline. A fresh run with a
valid strict-schema panel envelope and compliant evidence is required.
