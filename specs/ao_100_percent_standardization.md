# Specification: 100% Agent Orchestrator (AO) Standardization & Prompt Indirection

**Status**: PROPOSED / ARCHITECTURE SPECIFICATION  
**Author**: Antigravity / Dark Factory Core Architecture  
**Target Repository**: `jleechanorg/dark-factory`  
**Date**: 2026-08-21  

---

## 1. Executive Summary & Architectural Motivation

Dark Factory is an autonomous pipeline runner based on the **Attractor Pattern** (Level 5 lights-off engineering). It orchestrates complex multi-stage engineering tasks via directed acyclic graphs (`.dot`), enforces deterministic gates and sealed holdout evaluations, and records auditable trajectories in the **CXDB SQLite Event Ledger**.

Historically, Dark Factory faced a split-brain dilemma: whether to maintain its own custom background subprocess coder runner and tmux harnesses, or delegate execution to **Agent Orchestrator (AO)**. Maintaining custom coder harnesses inside Dark Factory introduced significant operational overhead:
1. Duplication of 10,000+ lines of low-level OS process management, tmux detachment/attachment, and git worktree isolation.
2. Fragile host workarounds (macOS Keychain access, Linux Playwright browser cache deduplication, and folder trust injection).
3. Risk of CPU spawn storms and uncoordinated concurrency limits across two separate daemons.

**Decision**: Standardize **100% on Agent Orchestrator (AO)** for all agent lifecycle management, workspace provisioning, and process supervision. Dark Factory remains strictly focused on Layer 1 (DAG pipeline orchestration and 7-gate outcome verification).

---

## 2. Layered Architecture & Separation of Concerns

```
┌─────────────────────────────────────────────────────────────────────────┐
│              LAYER 1: DARK FACTORY (Control Plane)                      │
│                                                                         │
│  • DOT Pipeline Traversal Engine (runner/engine.py)                     │
│  • Rust Verifier & 7-Gate Lifecycle Engine (daemon/src/verifier.rs)    │
│  • Sealed Holdouts & Adversarial Outcome Graders                        │
│  • CXDB SQLite Event Ledger (runner/cxdb.py)                            │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │ File Contract: .factory/prompt.md
                                     │ CLI Dispatch: ao spawn -p <project>
                                     ▼
┌─────────────────────────────────────────────────────────────────────────┐
│            LAYER 2: AGENT ORCHESTRATOR (Execution Plane)                │
│                                                                         │
│  • Git Worktree Lifecycle & .venv Auto-Symlinking                       │
│  • Process Supervision (tmux sessions, liveness polling, grace periods)│
│  • Agent CLI Plugins (Antigravity `agy`, Claude Code, Codex)            │
│  • Concurrency Caps & Bounded Spawn Admission Queue (20 active cap)     │
│  • PR State Reconciling & Stale Zombie Reaping                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 3. The Prompt Indirection Protocol (`.factory/prompt.md`)

### The Constraint
The AO CLI (`packages/cli/src/commands/spawn.ts:159-161`) and REST API (`packages/web/src/app/api/spawn/route.ts:40-44`) enforce a 4096-character limit and replace newlines with spaces on `--prompt` arguments passed over the command line.

### The Protocol
To support rich, multi-thousand-line specifications, prompt templates, and code context without formatting loss or truncation, Dark Factory implements **File-Based Prompt Indirection**:

1. **Prompt Generation**: When a pipeline node or daemon intake dispatches a task, Dark Factory resolves the template and writes the full markdown payload to:
   ```text
   <target_repo_or_worktree>/.factory/prompt.md
   ```
2. **Concise Dispatch**: Dark Factory calls AO with a clean, 1-line pointer prompt:
   ```bash
   ao spawn -p <project_id> \
            --agent antigravity \
            --issue <bead_id_or_branch> \
            --prompt "Execute the task specified in .factory/prompt.md"
   ```
3. **Agent Ingestion**: The agent CLI boots inside the isolated worktree created by AO, reads `.factory/prompt.md`, and begins autonomous execution with full markdown context intact.

---

## 4. Key Integration Contracts

### 4.1 Project-Scoped Queries (`-p <project>`)
All queries from Dark Factory to AO (e.g. `ao status`, `ao session ls`) **MUST** include `-p <project>` to avoid scanning all host repositories, preventing rate-limit exhaustion and unnecessary latency.

### 4.2 Liveness & Startup Grace Period
- Dark Factory relies on AO's `startupGracePeriodMs` (default 30s–120s) to allow agents to initialize without premature termination.
- If a session fails during spawn or crashes, Dark Factory catches the non-zero return code, classifies the error (infra failure vs test failure), and records the event in CXDB.

### 4.3 Worktree & Venv Auto-Configuration
- AO's `workspace-worktree` plugin automatically detects and symlinks `.venv`/`venv` from the repository root into the created worktree.
- AO's `agent-antigravity` plugin pre-seeds `trustedFolders.json` and `antigravity-cli/settings.json` (`trustedWorkspaces`), eliminating interactive trust prompt blocks.

### 4.4 Automated Zombie Reaping & Teardown
- When a GitHub PR reaches `MERGED` or `CLOSED`, AO's `session-reaper` automatically frees domain locks, destroys the ephemeral worktree, and archives session metadata.
- Dark Factory does not need to run manual `git worktree prune` routines.

---

## 5. Implementation Roadmap & Deprecation Plan

| Phase | Milestone | Deliverables |
| :--- | :--- | :--- |
| **Phase 1** | **Standardize Prompt Indirection** | Ensure all `dark-factory` DOT nodes and Rust daemon dispatches write `.factory/prompt.md` and use the 1-line pointer. |
| **Phase 2** | **Deprecate Custom Coder Harnesses** | Remove any legacy ad-hoc tmux/subprocess spawning code in `dark-factory` runner scripts that bypassed AO. |
| **Phase 3** | **Health Probes & Telemetry** | Add pre-spawn AO availability checks (`ao --version`) and map AO session IDs to CXDB run steps. |
| **Phase 4** | **Verification Suite** | Add automated integration tests verifying prompt indirection end-to-end with mock and live AO backends. |

---

## 6. Risk Analysis & Mitigations

| Identified Risk | Severity | Mitigation Strategy |
| :--- | :---: | :--- |
| **AO Daemon Downtime / Stale Lock** | Medium | Dark Factory implements timeout-bounded CLI calls with fallback to Healer failure classification in CXDB. |
| **Worktree Path Resolution Inconsistency** | Low | Explicitly pass `-p <project>` and let AO resolve standard worktree paths under `.ao/worktrees/<project>/<session_id>`. |
| **Missing .factory/prompt.md on Branch Switch** | Low | The daemon ensures `.factory/prompt.md` is written to disk *after* worktree checkout and committed/staged if necessary. |
