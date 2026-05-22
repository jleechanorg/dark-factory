# Amazon Clone MVP Benchmark

A sealed e-commerce benchmark comparing four orchestration methods for shipping production-grade web applications from natural language specs.

## Methods Compared

| Method | Pipeline | Orchestration Style |
|--------|----------|---------------------|
| **dark-factory** | `pipelines/amazon-clone/main.dot` | CXDB + Healer feedback loop |
| **df-slim** | `pipelines/slim/main.dot` | Minimal pipeline with tight gates |
| **kilroy** | `pipelines/kilroy/main.dot` | Iterative refinement cycles |
| **tracker** | `pipelines/tracker/main.dot` | Human-in-the-loop checkpoints |

## Running Instructions

### Run All Methods

```bash
./scripts/run_all.sh <spec_path> [--output-dir <results_dir>]
```

Runs all four methods against the same spec and produces comparative results.

### Run Single Candidate

```bash
./scripts/run_candidate.sh <method> <spec_path> [--output-dir <results_dir>]
```

Where `<method>` is one of: `dark-factory`, `df-slim`, `kilroy`, `tracker`.

### Individual Pipeline Execution

```bash
# dark-factory
python -m runner \
  --pipeline pipelines/amazon-clone/main.dot \
  --goal "$(cat spec.md)" \
  --backend claude

# df-slim
python -m runner \
  --pipeline pipelines/slim/main.dot \
  --goal "$(cat spec.md)" \
  --backend claude

# kilroy
python -m runner \
  --pipeline pipelines/kilroy/main.dot \
  --goal "$(cat spec.md)" \
  --backend claude

# tracker
python -m runner \
  --pipeline pipelines/tracker/main.dot \
  --goal "$(cat spec.md)" \
  --backend claude
```

## Scoring

See [SCORING.md](./SCORING.md) for the full scoring rubric and pass thresholds.

## Benchmark Structure

```
benchmarks/amazon-clone/
├── README.md           # This file
├── spec.md            # Feature specification
├── visible_acceptance.md  # Acceptance checklist
├── results/           # Output directory for benchmark results
└── scripts/
    ├── run_all.sh     # Run all four methods
    └── run_candidate.sh # Run single method
```

## Sealed Evaluation

The benchmark uses the Attractor pattern's sealed holdout methodology:

- The specification (`spec.md`) is public and given to implementing agents
- The acceptance criteria (`visible_acceptance.md`) is public for transparency
- The scoring rubric (`SCORING.md`) defines pass thresholds
- Implementation details (exact test fixtures, PII injection scenarios) are sealed

## Contact

This benchmark was created to evaluate e-commerce application delivery across multiple orchestration frameworks.