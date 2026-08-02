# Free-Form Cold-Review Screen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing cold-review benchmark to run and sealed-score a blinded three-arm screen comparing current v2 with two short free-form prompts.

**Architecture:** Reuse `benchmarks/scripts/run_controller_cold_review.py` for immutable inputs, detached worktrees, Codex transport, arm randomization, concurrency, and artifacts. Add an experiment-only raw-transcript path for free-form arms while leaving production controller validation unchanged. Extend the existing sealed scorer with a transcript-native judgment schema in which a blinded model owns finding extraction and semantic matching; deterministic code validates bindings and computes metrics only.

**Tech Stack:** Python 3.13, stdlib `argparse`/`subprocess`/`concurrent.futures`, pytest, Codex JSONL transport, existing dark-factory public benchmark and sealed holdout scorer.

## Global Constraints

- Production pipelines and their v1 selector remain unchanged.
- Use exactly these cases: `wa-8603-r1`, `wa-8612-r2`, `wa-8613-r1`.
- Use exactly three arms: current pinned v2, free-form traceability, and free-form adversarial.
- Reviewer model is `gpt-5.6-luna`, reasoning effort `high`, timeout `900`, and case workers `3` for all arms.
- Arms are serial within each case; the three cases run concurrently with observed concurrency recorded.
- Free-form responses have no gates, verdict line, JSON schema, or machine response contract.
- Application code performs no keyword splitting, heuristic finding extraction, semantic classification, or intent routing. The blinded evaluator model owns transcript finding extraction and semantic matching.
- Sealed rubric content never enters public files, reviewer prompts, or public artifacts.
- Invalid transport or binding attempts are preserved and never silently retried in place.
- Every commit message includes CLI and model attribution.

---

### Task 1: Add a three-arm transcript screen to the existing public runner

**Files:**
- Modify: `benchmarks/scripts/run_controller_cold_review.py`
- Test: `tests/test_controller_cold_review_benchmark.py`

**Interfaces:**
- Consumes: existing `load_manifest`, `validate_case`, `_detached_worktree`, `_build_controller_codex_transport`, `parse_codex_jsonl`, and `parse_codex_usage`.
- Produces: `build_screen_plan(cases, *, seed, model, reasoning_effort, timeout_seconds) -> tuple[dict, dict]`.
- Produces: `run_screen(manifest, *, case_ids, repo, output_dir, seed, model, reasoning_effort, timeout_seconds, workers) -> None`.
- Produces: three `cold-review-transcript-run-v1` blinded bundles named `blinded-arm-1-bundle.json` through `blinded-arm-3-bundle.json`.

- [ ] **Step 1: Write RED tests for exact arms, prompts, controls, and randomization**

Add tests that assert the exact variant set and prompt text from the approved spec, three opaque arms per case, identical controls/input order, deterministic seeded order, and no variant names in the public plan:

```python
def test_screen_plan_has_three_blinded_variants_with_identical_controls():
    manifest = load_manifest(MANIFEST)
    cases = [case for case in manifest["cases"] if case["id"] in SCREEN_CASE_IDS]
    public, private = build_screen_plan(
        cases,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
    )
    assert len(public["runs"]) == 9
    assert len(private["arms"]) == 9
    assert "freeform" not in json.dumps(public).lower()
    assert "cold-review-v2" not in json.dumps(public).lower()
    for case in cases:
        runs = [row for row in public["runs"] if row["case_id"] == case["id"]]
        assert [row["arm"] for row in runs] == ["arm-1", "arm-2", "arm-3"]
        assert len({json.dumps(row["controls"], sort_keys=True) for row in runs}) == 1
```

- [ ] **Step 2: Run the plan test and verify RED**

Run:

```bash
.venv/bin/python -m pytest tests/test_controller_cold_review_benchmark.py::test_screen_plan_has_three_blinded_variants_with_identical_controls -q
```

Expected: FAIL because `build_screen_plan` and the screen constants do not exist.

- [ ] **Step 3: Implement the exact prompt constants and generalized private plan**

Keep both experimental prompts as module constants in the existing benchmark runner. Add a private mapping field named `review_variant`; public rows contain opaque arm IDs only. Use a shared internal plan builder so the existing two-arm A/B behavior is not duplicated or changed.

```python
SCREEN_CASE_IDS = ("wa-8603-r1", "wa-8612-r2", "wa-8613-r1")
SCREEN_VARIANTS = (
    "control-v2",
    "freeform-traceability",
    "freeform-adversarial",
)
FREEFORM_PROMPTS = {
    "freeform-traceability": "Review this PR independently. ...",
    "freeform-adversarial": "Try to prove this PR is wrong. ...",
}
```

Copy the exact four-sentence prompt bodies from `docs/superpowers/specs/2026-08-02-free-form-cold-review-screen-design.md`; do not paraphrase them.

- [ ] **Step 4: Run the plan test and verify GREEN**

Run the Step 2 command. Expected: PASS.

- [ ] **Step 5: Write RED tests for free-form transport and transcript artifacts**

At the public `run_screen` seam, monkeypatch `subprocess.run` with Codex JSONL containing a final agent message and a `turn.completed` usage record. Assert:

```python
assert completed.kwargs["cwd"] == neutral_dir
assert completed.kwargs["env"].get("DARK_FACTORY_HOLDOUTS") is None
assert "PROMPT_ID:" not in completed.kwargs["input"]
assert "BEGIN_CONTROLLER_ENVELOPE_BASE64" in completed.kwargs["input"]
assert bundle["schema_version"] == "cold-review-transcript-run-v1"
assert set(bundle["cases"][0]) == {
    "case_id", "base_sha", "head_sha", "diff", "diff_sha256",
    "case_sha256", "transcript", "transcript_sha256", "metrics",
}
```

Also assert nonzero transport exit creates a receipt and raises `BenchmarkError`, missing usage is invalid, holdout variables are removed, prompt/input digests are recorded, and no free-form semantic parsing occurs.

- [ ] **Step 6: Run the transport tests and verify RED**

Run:

```bash
.venv/bin/python -m pytest tests/test_controller_cold_review_benchmark.py -k 'screen or freeform' -q
```

Expected: FAIL because `run_screen` does not exist.

- [ ] **Step 7: Implement minimal raw free-form execution inside the existing runner**

For the control arm, continue using `create_review_request(..., review_contract="cold-review-v2")` and `run_controller_review`. For a free-form arm:

1. build the same canonical v2 envelope from the immutable `ReviewInputs`;
2. append the existing Base64 envelope delimiter block to the exact short prompt, without the v2 template or response contract;
3. reuse `_gate_subprocess_args` and `_build_controller_codex_transport`;
4. run `subprocess.run` in the neutral directory with `_sanitized_env()`;
5. use `parse_codex_jsonl` and `parse_codex_usage` for syntax and metrics only; and
6. write prompt, envelope, raw JSONL, response, receipt, and digest-bound run record.

Do not add a shared abstraction unless a second caller needs it. Do not change `run_controller_review` to allow unvalidated production responses.

The three-arm bundle contains complete transcripts and no application-derived findings or verdict:

```python
entry = {
    "case_id": case_id,
    "base_sha": case["base_sha"],
    "head_sha": case["head_sha"],
    "diff": inputs.diff_text,
    "diff_sha256": case["diff_sha256"],
    "case_sha256": case_sha256,
    "transcript": transcript,
    "transcript_sha256": _sha256(transcript.encode()),
    "metrics": _token_metrics(receipt),
}
```

- [ ] **Step 8: Add CLI selection without creating another runner**

Add `screen` to the existing command choices and repeatable `--case-id`. `screen` requires exactly the three approved case IDs, explicit `--model`, and a new output directory. Existing `validate` and `run` semantics remain unchanged.

- [ ] **Step 9: Run public benchmark tests and boundary checks**

Run:

```bash
.venv/bin/python -m pytest tests/test_controller_cold_review_benchmark.py tests/test_benchmark_boundary.py tests/test_review_controller.py tests/test_gate_subprocess_dispatch.py -q
.venv/bin/python benchmarks/scripts/check_boundary.py
git diff --check
```

Expected: all PASS and no sealed strings in public files.

- [ ] **Step 10: Commit Task 1**

```bash
git add benchmarks/scripts/run_controller_cold_review.py tests/test_controller_cold_review_benchmark.py
git commit -m "[codex/gpt-5.6-luna] feat: add free-form cold review screen"
```

---

### Task 2: Add transcript-native semantic judgments to the sealed scorer

**Files:**
- Modify: `/Users/jleechan/projects/worktree_cold_review_holdouts/evaluator/cold_review_recall.py`
- Test: `/Users/jleechan/projects/worktree_cold_review_holdouts/tests/test_seal.py`

**Interfaces:**
- Consumes: a `cold-review-transcript-run-v1` bundle containing a nonempty subset of sealed rubric cases.
- Produces: validation and scoring for `cold-review-transcript-judgments-v1`.
- Produces: optional CLI `--output PATH` that writes the exact JSON result also emitted on stdout.

- [ ] **Step 1: Write a RED test for the transcript judgment schema**

Use a synthetic one-case rubric and bundle. The judgment case schema is exact:

```json
{
  "case_id": "case-a",
  "case_sha256": "...",
  "transcript_sha256": "...",
  "reports_no_actionable_findings": false,
  "transcript_findings": [
    {"id": "tf-1", "text": "Concrete defect", "supported": true}
  ],
  "expected_matches": [
    {"expected_finding_id": "sealed-1", "transcript_finding_ids": ["tf-1"]}
  ]
}
```

Assert P0/P1 and actionable recall, unsupported counts, implicit false PASS, latency, tokens, and digests.

- [ ] **Step 2: Run the transcript scoring test and verify RED**

Run:

```bash
python3 -m pytest tests/test_seal.py -k transcript -q
```

Expected: FAIL because the transcript schemas are unsupported.

- [ ] **Step 3: Implement structural validation and deterministic arithmetic only**

Add schema dispatch based on the exact bundle and judgment `schema_version`. For transcript scoring:

- require bundle cases and judgment cases to be the same nonempty subset of rubric cases;
- validate case, diff, transcript, metrics, bundle, judgment, and rubric digests;
- require every expected finding in each selected rubric case to have exactly one match row;
- require unique transcript finding IDs and classify every transcript finding with one explicit `supported` boolean;
- allow matches only to known, supported transcript finding IDs;
- calculate recall and unsupported rates deterministically; and
- define false PASS as `reports_no_actionable_findings == true` when the selected rubric case has actionable findings.

Do not inspect transcript text in application code except for type/nonempty validation. Do not infer findings, severity, or support with keywords, regex, or condition chains.

- [ ] **Step 4: Add RED tests for malformed and incomplete judgments**

Cover unknown transcript finding IDs, duplicate IDs, incomplete expected rows, mismatched case/transcript digests, incomplete transcript support classification, unsupported matched findings, non-subset cases, and a known-P0/P1 implicit false PASS.

- [ ] **Step 5: Run transcript tests and verify RED, then implement GREEN**

Run the Step 2 command before and after the minimal validation additions. Expected final result: PASS.

- [ ] **Step 6: Add and test optional score artifact output**

Add `--output PATH`. Write the exact sorted JSON line printed to stdout plus a trailing newline. Test that the file bytes parse to the same result and carry the same `score_sha256`.

- [ ] **Step 7: Run all sealed tests**

```bash
python3 -m pytest tests/ -q
git diff --check
```

Expected: all PASS.

- [ ] **Step 8: Commit Task 2**

```bash
git add evaluator/cold_review_recall.py tests/test_seal.py
git commit -m "[codex/gpt-5.6-terra] feat: score free-form review transcripts"
```

---

### Task 3: Execute, judge, score, and record the real screen

**Files:**
- Runtime artifacts: `/Users/jleechan/.dark-factory/benchmarks/controller-cold-review/<new-run-id>/`
- Modify through `br`: `.beads/issues.jsonl` for `jleechan-gcwh.5` notes only

**Interfaces:**
- Consumes: the public `screen` command, three opaque bundles, sealed rubric, and transcript scorer.
- Produces: three judgment JSON files, three score JSON files, a deblinded comparison, and a Bead update.

- [ ] **Step 1: Verify both repositories and exact heads**

```bash
git -C /Users/jleechan/projects/worktree_cold_review_v2 status -sb
git -C /Users/jleechan/projects/worktree_cold_review_holdouts status -sb
git -C /Users/jleechan/projects/worktree_cold_review_v2 rev-parse HEAD
git -C /Users/jleechan/projects/worktree_cold_review_holdouts rev-parse HEAD
```

Only the known untracked `.githooks/pre-commit` may remain in the public worktree. Stop on any other unexpected change.

- [ ] **Step 2: Run the real three-arm screen**

Use a fresh output directory and recorded seed:

```bash
.venv/bin/python benchmarks/scripts/run_controller_cold_review.py screen \
  --manifest benchmarks/controller-cold-review/cases.json \
  --case-id wa-8603-r1 \
  --case-id wa-8612-r2 \
  --case-id wa-8613-r1 \
  --repo /Users/jleechan/projects/worldarchitect.ai \
  --output /Users/jleechan/.dark-factory/benchmarks/controller-cold-review/<new-run-id> \
  --seed 20260802 \
  --model gpt-5.6-luna \
  --reasoning-effort high \
  --timeout 900 \
  --workers 3
```

Sample live process counts and verify `concurrency.json` records `observed_max_cases: 3`.

- [ ] **Step 3: Dispatch three blinded transcript judges in parallel**

Dispatch Luna/Terra judge subagents with disjoint opaque bundle paths. Each judge may read only its bundle, the sealed rubric, and the transcript scorer schema. It must not read the private arm map, prompts, sibling bundles, raw artifacts, or production variant identities. Each writes `cold-review-transcript-judgments-v1` JSON and asserts `blind_to_prompt_identity: true`.

- [ ] **Step 4: Score each opaque arm and persist results**

```bash
python3 evaluator/cold_review_recall.py --bundle <arm-bundle> --judgments <arm-judgments> --output <arm-score>
```

Run from `/Users/jleechan/projects/worktree_cold_review_holdouts`. Any scorer exit `2` invalidates the screen. Exit `1` records a known-P0/P1 false PASS and prevents advancement.

- [ ] **Step 5: Deblind only after all judgments and scores are bound**

Read the private arm map after all three score files exist. Produce a comparison with per-case and aggregate recall, false PASS, unsupported findings, invalids, tokens, and latency for the named variants. Apply the advancement rule exactly as written in the approved design; ties advance but are not wins.

- [ ] **Step 6: Independently review the run artifacts**

Terra reviews binding equality, arm-order randomization, model/control equality, concurrency proof, judge blinding, score completeness, and absence of sealed leakage. Luna reviews metric arithmetic and advancement-rule application. Any disagreement is reported rather than averaged away.

- [ ] **Step 7: Record the result in Beads**

Use `br update jleechan-gcwh.5 --notes ...` with run path, exact heads, prompt digests, score digests, metrics, advancement decision, limitations, and next step. Keep `.5` open unless the full seven-snapshot acceptance criteria are met. Keep `.6` blocked and production on v1.

- [ ] **Step 8: Commit and push tracker state**

```bash
br sync --flush-only
git add .beads/issues.jsonl
git commit -m "[codex/gpt-5] chore: record free-form review screen"
git push
```

- [ ] **Step 9: Final verification**

```bash
.venv/bin/python -m pytest tests/test_controller_cold_review_benchmark.py tests/test_review_controller.py tests/test_gate_subprocess_dispatch.py -q
python3 -m pytest /Users/jleechan/projects/worktree_cold_review_holdouts/tests/ -q
git diff --check
```

Report exact commit URLs, artifact paths, test counts, invalid attempts, and the screen-only limitation. Do not open or merge a production rollout PR.
