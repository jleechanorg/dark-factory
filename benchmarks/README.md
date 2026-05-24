# Benchmark contract

Benchmarks in this repository use an Attractor-style visible/sealed split.

## Visible to coder agents

- `spec.md`
- `visible_acceptance.md`
- `starter/`
- public prompts
- pipeline `.dot` files
- public acceptance command output
- redacted scorer summaries

## Hidden from coder agents

- exact hidden scenario IDs and values
- evaluator implementation details
- sealed repository paths
- raw per-scenario failure details
- other candidates' private run artifacts

The visible spec should be complete enough to implement the requested product
fairly. The sealed layer should only hide adversarial examples and scoring
internals, not material user-story requirements.

## Current benchmarks

- `fibonacci/` is the tiny deterministic smoke benchmark for harness plumbing.
- `amazon-clone/` is the larger product benchmark skeleton for multi-method
  comparisons.

