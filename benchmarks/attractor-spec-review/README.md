# Attractor Spec Review Benchmark

General-purpose benchmark harness for validating Attractor-style NLSpec quality with an
independent, adversarial reviewer pass.

The benchmark has its own DOT graphs:

- `review_slim.dot` — lean orchestration: `plan -> implement -> public_acceptance -> review -> exit`
- `review_full.dot` — same loop plus an explicit full-stack sanity check node before
  reviewer handoff

All runs use:

- implementation nodes (`plan`, `implement`, `fix`) driven by the normal `codex`
  backend
- an **independent reviewer node** implemented as a `tool` node that invokes
  `benchmarks/attractor-spec-review/scripts/review_with_codex.sh`
- `codex exec --yolo` inside that script so the review pass is a separate
  runtime invocation, with its own process and output contract

The reviewer must return strict JSON with line-by-line findings. This gives a
machine-checkable adversarial review of the spec and blocks empty prose verdicts.

## What this benchmark validates

1. Full `.dot` graph shape under the Dark Factory runner.
2. Deterministic control flow (`max_retries`, conditional branching, and loops).
3. Line-aware spec validation implementation from visible requirements.
4. Independent reviewer evidence: JSON verdict + line-level findings.
5. Optional full-stack file layout checks for `backend/`, `frontend/`, and
   Firestore rule artifacts.

## Directory layout

- `prompts/` — `plan`, `implement`, `fix`, `reviewer`
- `pipelines/` — `review_slim.dot`, `review_full.dot`
- `scripts/` — `prepare_candidate.sh`, `run_candidate.sh`,
  `run_matrix_deterministic.sh`, `review_with_codex.sh`, `fetch_attractorbench_fork.sh`
- `starter/` — baseline files for the implementation target
- `spec.md` — visible feature spec
- `visible_acceptance.md` — public acceptance contract the evaluator expects

## How to run

Prepare a workdir and run slim:

```bash
cd /Users/jleechan/projects/dark-factory
bash benchmarks/attractor-spec-review/scripts/prepare_candidate.sh review-slim /tmp/attractor-review
bash benchmarks/attractor-spec-review/scripts/run_candidate.sh review-slim review-slim-run /tmp/attractor-review
```

Run full:

```bash
cd /Users/jleechan/projects/dark-factory
bash benchmarks/attractor-spec-review/scripts/prepare_candidate.sh review-full /tmp/attractor-review-full
bash benchmarks/attractor-spec-review/scripts/run_candidate.sh review-full review-full-run /tmp/attractor-review-full
```

Run deterministic matrix smoke (no LLM calls, mocked handlers):

```bash
bash benchmarks/attractor-spec-review/scripts/run_matrix_deterministic.sh /tmp/attractor-review-matrix
```

## Forking AttractorBench

If you want to validate against your own copy of upstream specs, run:

```bash
bash benchmarks/attractor-spec-review/scripts/fetch_attractorbench_fork.sh /tmp/attractorbench-fork
```

Optional clone URL override:

```bash
ATTRACTORBENCH_REPO=https://github.com/<your-org>/attractorbench.git \
bash benchmarks/attractor-spec-review/scripts/fetch_attractorbench_fork.sh /tmp/attractorbench-fork
```
