# Decision 0001 — Lane choice: pipeline-engine orchestrator (Lane A)

**Date**: 2026-05-23
**Status**: Accepted
**Bead**: [orch-wjmz](https://...)

## Context

`dark-factory` currently does two things that pull in different directions, and the docs / `Makefile` / `bin/` tree are starting to reflect both:

| Lane | Stance | Implication |
|---|---|---|
| **A — Pipeline engine** (Layer 3 of the Attractor stack) | dark-factory **IS the orchestrator**. It runs real LLM workers (Claude, Codex, AO sessions). It is benchmarked **by** AttractorBench via its workers attempting AttractorBench's specs. | `.dot` files are the durable artifact; runner code is *dorodango*; `--backend claude/codex/ao` are first-class; mock-LLM modes are for testing only. |
| **B — AttractorBench target** | dark-factory **IS the agent under test**. It reads only AttractorBench's NLSpec for "unified LLM client + agent loop + pipeline engine," builds itself, exposes `make build` / `make test` / `./bin/conformance`, runs against a deterministic mock-LLM server, and is **scored**. | Stub LLM client, no real subprocesses, scoring focused on convergence to the Attractor canonical shape. |

Both lanes are valid. They are **not** the same project. Without an explicit choice, every new file under `bin/` and `benchmarks/` quietly picks a lane, and the choices drift.

## Decision

**Lane A is primary.** dark-factory is, first and foremost, an Attractor-pattern **pipeline engine** that runs real workers against real specs. The 2389 and Shapiro framing — pipelines as `.dot` files, CXDB as the learning artifact, sealed holdouts in a sibling repo — is the load-bearing identity.

**Lane B is an optional sub-profile**, addressable via a future flag (placeholder name: `--mode bench-self`). When set, the runner uses a deterministic mock-LLM stub instead of dispatching real workers, and exposes the AttractorBench conformance surface (`./bin/conformance score`). This makes dark-factory **also** scoreable by AttractorBench without making that the primary use case.

## Why Lane A primary, not Lane B

1. **The field needs more independent Attractor implementations.** Per 2389, four independent builds (Kilroy, Mammoth, Smasher, Tracker) demonstrated convergence. dark-factory adds a fifth in Python. Demoting it to a scoring target instead of a working orchestrator weakens that signal.
2. **Real-worker orchestration is where the operator gets value today.** `/factory` against `mctrl_test`, the all-nodes-coverage benchmark, the amazon-clone matrix — every existing user-facing flow is Lane A.
3. **Lane B without Lane A is just a benchmark adapter** — useful for AttractorBench's leaderboard, but not a tool. Lane A without Lane B is a tool that happens to be scoreable later.
4. **Layering**: Lane B is implementable as a thin LLM-client stub plugged into Lane A's existing `--backend` machinery. Lane B inside Lane A is cheap. Lane A inside Lane B is impossible.

## Consequences

- New first-class CLI flags (`--backend`, `--ao-project`, `--cxdb`) live on the Lane A runner. Lane B's `bin/conformance` consumes Lane A internals; it does not duplicate them.
- The `Makefile` `test`/`build` targets test the Lane A engine; a future `conformance-self` target will exercise the Lane B sub-profile when implemented.
- The amazon-clone matrix benchmark (4-method adapters) is Lane A: it dispatches real LLM workers via dark-factory's own pipeline. It is **not** an AttractorBench conformance run.
- The all-nodes-coverage benchmark is Lane A: it dispatches a real AO+Sonnet worker against a sealed evaluator. The hidden `__version__` forcing function is part of the **sealed evaluator's contract**, not part of a mock-LLM canned response.
- `bin/conformance score` (currently surveys pipelines + emits a JSON snapshot of the graph shape) is a Lane B precursor: it does NOT run real workers, it summarises the runner's *self-description*. That role is fine as long as it doesn't grow real-LLM dispatch.

## Criteria for breaking this rule

Switch to Lane B as primary only if:

- AttractorBench's NLSpec stabilises to the point where dark-factory's score against it becomes the more important external signal than `/factory` runtime value, **and**
- AttractorBench publishes a leaderboard that the user wants to compete on as a primary metric.

Neither holds today (2026-05-23).

## Open follow-ups (separate beads)

- [orch-sdy0](https://...) — AttractorBench-compatible conformance surface (Lane B's contract). Implement under the Lane A engine, not alongside it.
- [orch-qhez](https://...) — token + cost tracking. Lane A first; Lane B can stub the same fields.
- [orch-1ouv](https://...) — evidence bundle per run. Lane A real-worker runs get full bundles; Lane B mock runs get a structurally-identical bundle for reproducibility.
- [orch-ac6q](https://...) — full Attractor edge semantics (preferred labels, tie-breaks, terminal behaviour). The semantics live on the pipeline engine (Lane A); Lane B inherits.

## References

- StrongDM AttractorBench: <https://github.com/strongdm/attractorbench>
- Dan Shapiro, "You don't write the code": <https://www.danshapiro.com/blog/2026/02/you-dont-write-the-code/>
- 2389, "The Dark Factory is a .dot file": <https://2389.ai/posts/the-dark-factory-is-a-dot-file/>
- Repo isolation contract: [`../../CLAUDE.md`](../../CLAUDE.md)
