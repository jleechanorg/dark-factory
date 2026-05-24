# Plan Task

Generate a development plan for implementing the Amazon Clone MVP.

## Reference Spec

Read the full specification at: `benchmarks/amazon-clone/spec.md`
Read the public acceptance criteria at: `benchmarks/amazon-clone/visible_acceptance.md`

Treat those two files as the complete visible product contract. Do not add
flows that are not present there, and do not inspect hidden evaluator details.

## Launch Contract

Your plan must ensure the following commands will succeed:

```bash
make build    # Install dependencies, compile/transpile if needed
make test     # Run test suite (must pass)
make run      # Start server on port 3000, must respond to health check
```

## Plan Output

Provide a 2-3 sentence executive summary describing:
- Main files and their purpose
- Overall project structure
- Framework and architecture choices

Then provide a high-level task list (5-10 items) covering:
- Initial project setup
- Core data and UI model
- Frontend components for each visible flow
- Testing strategy
- Deployment configuration
