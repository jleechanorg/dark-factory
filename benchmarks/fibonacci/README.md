# Fibonacci benchmark

Tiny sealed-boundary smoke benchmark for Attractor-style runners.

The visible contract is intentionally complete enough for a coding agent to
build against without seeing evaluator internals:

- Read `spec.md`.
- Start from `starter/`.
- Produce a CLI at `fib.py`.
- Run `scripts/run_candidate.sh <candidate-dir>`.
- Score with `scripts/score_candidate.py <candidate-dir>`.

This benchmark is not a substitute for AttractorBench or the Amazon-clone
benchmark. It exists to prove that the benchmark plumbing works on a small,
deterministic task before spending agent time on a large UI/product benchmark.

