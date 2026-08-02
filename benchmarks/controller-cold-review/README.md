# Controller cold-review benchmark

This public benchmark compares two controller review contracts over seven
immutable revisions from five pull requests. It contains inputs and execution
machinery only; scoring truth belongs outside this repository.

## Immutable inputs

`cases.json` pins each base commit, head commit, head tree, binary diff digest,
sorted changed-file list and digest, evidence manifest digest, and task digest.
The task is a deterministic `git-commit-claims-v1` snapshot: commits in
`base..head` are ordered by `git rev-list --reverse --topo-order`, then each
commit contributes its full SHA, subject, and body. An empty body is rendered
as `(empty)`. Mutable pull-request descriptions are never read.

The diff is the exact stdout of
`git diff --no-ext-diff --binary <base>..<head>` with only its final newline
removed. Changed files are sorted and encoded as canonical compact JSON before
hashing.

## Validate and run

```bash
python benchmarks/scripts/run_controller_cold_review.py validate \
  --manifest benchmarks/controller-cold-review/cases.json \
  --repo /Users/jleechan/projects/worldarchitect.ai

python benchmarks/scripts/run_controller_cold_review.py run \
  --manifest benchmarks/controller-cold-review/cases.json \
  --repo /Users/jleechan/projects/worldarchitect.ai \
  --output /absolute/path/to/cold-review-run \
  --model gpt-5.6-terra \
  --reasoning-effort high \
  --timeout 1200 \
  --workers 2
```

The two randomized arms for a case always run serially with identical explicit
model, reasoning, tool, timeout, and input ordering. Independent cases run in
parallel up to `--workers`; the default is two and can be changed with
`DARK_FACTORY_REVIEW_CASE_WORKERS`. `concurrency.json` records requested and
actually observed maximum case concurrency.

Raw prompt, envelope, response, transport, receipt, findings, and digest-bound
run records live under `raw/`. `private-arm-map.json` is the reveal key and must
not be given to the scorer. The scorer inputs are
`blinded-arm-1-bundle.json` and `blinded-arm-2-bundle.json`; each contains one
identity-neutral narrative transcript and full diff for every case. Token usage
and elapsed latency are copied from the real controller receipt rather than
estimated.
