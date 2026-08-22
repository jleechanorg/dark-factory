# Nextsteps — Dark Factory — 2026-08-21

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Technical design](#technical-design)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Outcomes**: Formulated and validated the complete technical design for 100% Agent Orchestrator (AO) standardization and file-based prompt indirection (`.factory/prompt.md`). Unanimously approved by multi-model second opinion panel (`/advice` & `/secondo`: Cerebras Qwen 3, Gemini 3 Flash, Perplexity Sonar Pro, Grok 4 Fast, and Senior Principal Systems Architect).
- **Risks / Blockers**: None. Token authentication on `ai-universe-b3551` was refreshed and verified.
- **Top Priorities**: Execute Task 1 (`jleechan-s779.1` — Prompt indirection pipeline) followed by Task 2 (`jleechan-s779.2` — Coder harness deprecation).
- **Beads**: [jleechan-s779](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779) (Parent Epic), [jleechan-s779.1](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.1), [jleechan-s779.2](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.2).

## Context

Dark Factory operates on the Attractor Pattern (Level 5 lights-off engineering). Standardizing 100% on Agent Orchestrator (AO) as the sole execution plane establishes a clean boundary: Dark Factory owns Layer 1 (DOT graph execution, Rust 7-gate lifecycle verifier, sealed holdouts, CXDB event ledger), while AO owns Layer 2 (workspace provisioning, process supervision, concurrency admission queues, and zombie reaping).

## Bead index

| Bead | Title | Status | Link |
| :--- | :--- | :---: | :--- |
| **jleechan-s779** | `[architecture] Standardize 100% on AO via prompt indirection` | `open` (P2) | [jleechan-s779](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779) |
| **jleechan-s779.1** | `[architecture] Standardize prompt indirection (.factory/prompt.md) in handlers.py & adapters.rs` | `open` (P2) | [jleechan-s779.1](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.1) |
| **jleechan-s779.2** | `[architecture] Deprecate and remove redundant custom coder harnesses` | `open` (P2) | [jleechan-s779.2](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.2) |

## Technical design

### 1. The Prompt Indirection Protocol
- **Problem**: `ao spawn` caps inline `--prompt` arguments to 4096 characters and flattens newlines into spaces.
- **Solution**: Dark Factory writes the complete resolved prompt specification to `<worktree>/.factory/prompt.md`.
- **Invocation**:
  ```bash
  ao spawn -p <project> \
           --agent antigravity \
           --issue <bead_or_pr> \
           --prompt "Execute the task specified in .factory/prompt.md"
  ```
- **Fidelity**: The coder agent reads `.factory/prompt.md` upon boot, preserving 100% of formatting, line breaks, and multi-thousand-line contexts.

### 2. Integration Points
- **`runner/handlers.py`**: In `_codergen`, resolve prompt templates into `.factory/prompt.md` and pass pointer prompt when `--backend ao` is selected.
- **`daemon/src/adapters.rs`**: In `CliSessions::run_spawn_process`, persist candidate bead descriptions and specs to `.factory/prompt.md` before calling `ao_spawn_command`.
- **`daemon/scripts/ao-spawn-v013-bridge.mjs`**: Ensure the bridge cleanly passes through `-p <project>` and accepts pointer prompts without length issues.

## Work queue

1. **Implement Prompt Indirection in Handlers & Adapters**:
   - **Goal**: Write prompt payloads to `.factory/prompt.md` and invoke AO with pointer prompt.
   - **Acceptance Criteria**: Dispatches with prompt lengths >4096 characters succeed across all pipeline nodes and daemon intakes.
   - **Files**: `runner/handlers.py`, `daemon/src/adapters.rs`, `daemon/scripts/ao-spawn-v013-bridge.mjs`.
   - **Bead**: [jleechan-s779.1](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.1).

2. **Deprecate Legacy Custom Subprocess Coder Code**:
   - **Goal**: Clean up unused or redundant ad-hoc background subprocess scripts that bypass AO.
   - **Acceptance Criteria**: Coder execution routes exclusively through AO; no uncoordinated tmux sessions created by runner.
   - **Files**: `runner/handlers.py`, `install.sh`.
   - **Bead**: [jleechan-s779.2](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779.2).

3. **Verify End-to-End Regression Suite**:
   - **Goal**: Run targeted daemon and runner unit tests to confirm prompt indirection and AO lifecycle conformance.
   - **Acceptance Criteria**: `cargo test --test tick_integration` and `pytest tests/test_engine.py` pass.
   - **Bead**: [jleechan-s779](https://github.com/jleechanorg/dark-factory/issues/jleechan-s779).

## PR / merge state

- 🟢 [PR #655](https://github.com/jleechanorg/dark-factory/pull/655) `[OPEN]`: Synced with `origin/main`.

## Learnings pointer

- Logged to `~/roadmap/learnings-2026-08.md` § `2026-08-21 — Standardize 100% on AO via Prompt Indirection (.factory/prompt.md)`.

## Roadmap pointer

- Appended to `roadmap/activity/2026-08-21.md` and indexed in `roadmap/README.md`.
