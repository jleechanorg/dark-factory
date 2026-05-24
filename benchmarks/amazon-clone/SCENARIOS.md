# Amazon Clone MVP - sealed validation contract

The exact behavioral holdout scenarios are sealed and are not stored in this
benchmark tree.

## Public contract

Coder agents receive:

- `benchmarks/amazon-clone/spec.md`
- `benchmarks/amazon-clone/visible_acceptance.md`
- `benchmarks/amazon-clone/starter/`
- public prompts and pipeline graphs

The visible spec and public acceptance document contain the full product
requirements the coder is expected to satisfy.

## Sealed contract

The evaluator runs a fixed set of behavioral checks from a sibling holdouts
repository. Its exact scenario names, input values, Playwright selectors,
assertions, and scoring internals are intentionally hidden from coder agents.

Evaluator output returned to the coder must be redacted to aggregate verdicts,
counts, and high-level failure categories only.

## Launch contract

Candidate artifacts must support:

```bash
make build
make test
make run
```

`make run` must start the application on port 3000.

