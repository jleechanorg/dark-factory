# Nextsteps — dark-factory — 2026-08-24

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Accomplishments**: 
  - Standardized 100% on Agent Orchestrator (AO) with file-based prompt indirection (`.factory/prompt.md`), bypassing AO CLI 4096-character limit and newline flattening.
  - Successfully merged [PR #714](https://github.com/jleechanorg/dark-factory/pull/714) (`docs(roadmap): sync activity log and nextsteps (2026-08-22)`) into `origin/main`.
  - Conducted full priority triage and live operational audit across the 4 factory follow-up work items.
- **Risks & Blockers**:
  - **Runner Pool Offline (P0 Blocker)**: `gh api /repos/jleechanorg/dark-factory/actions/runners` returns 0 online runners. Self-hosted CI checks cannot execute without runners being brought back online.
- **Key Open Beads / Issues**:
  - [Issue #668](https://github.com/jleechanorg/dark-factory/issues/668) / [`jleechan-azso`](https://github.com/jleechanorg/dark-factory/issues/jleechan-azso) (P1: Fold `web-advice-failopen.dot` into existing pipelines)
  - [Issue #669](https://github.com/jleechanorg/dark-factory/issues/669) / [`jleechan-57ym`](https://github.com/jleechanorg/dark-factory/issues/jleechan-57ym) (P2: Lane D 5 cosmetic/structural items)
  - [Issue #670](https://github.com/jleechanorg/dark-factory/issues/670) / [`jleechan-gagl`](https://github.com/jleechanorg/dark-factory/issues/jleechan-gagl) (P3: Lane E/F remediation candidates)

## Context

This work block concluded the 100% Agent Orchestrator (AO) standardization and roadmap synchronization for `dark-factory`. After merging [PR #714](https://github.com/jleechanorg/dark-factory/pull/714), we audited factory telemetry, remote commit cadence, and active AO sessions on `jeff-ubuntu`. A comprehensive priority triage was performed on the remaining backlog items to prepare an actionable handoff queue for incoming agents.

## Bead index

| Bead | Title | Priority / Status | Link |
| :--- | :--- | :---: | :--- |
| **`jleechan-azso`** | fold web-advice-failopen.dot into pr_gates.dot + gates.dot + slim/minimal_pr.dot | **P1 (OPEN)** | [Issue #668](https://github.com/jleechanorg/dark-factory/issues/668) |
| **`jleechan-57ym`** | Lane D 5 cosmetic/structural items from E2E log §5 | **P2 (OPEN)** | [Issue #669](https://github.com/jleechanorg/dark-factory/issues/669) |
| **`jleechan-gagl`** | Lane E/F remediation candidates from E2E log | **P3 (OPEN)** | [Issue #670](https://github.com/jleechanorg/dark-factory/issues/670) |
| **`jleechan-xn4n`** | Linux container runners lack sqlite3 / runner pool restoration | **P0 (OPEN)** | [Issue #287](https://github.com/jleechanorg/dark-factory/issues/287) |
| **`jleechan-18mu`** | Parent goal bead: PR #655 web-advice follow-up | **P2 (OPEN)** | [br show jleechan-18mu](https://github.com/jleechanorg/dark-factory) |

## Work queue

### 1. **Restore `dark-factory` Self-Hosted Runner Pool (P0 Blocker)**
- **Goal**: Restore dark-factory self-hosted CI runner pool so future PRs can run Evidence Gate, `daemon-tests`, and `test` checks without timing out or requiring `--admin` manual overrides.
- **Current State**: 0 registered / online runners in `dark-factory`.
- **Acceptance Criteria**:
  - `gh api /repos/jleechanorg/dark-factory/actions/runners` returns >= 1 online runner.
  - A sample PR reaches `mergeStateStatus=CLEAN` autonomously through GitHub Actions.
- **Dependencies / Blockers**: Host infrastructure / SRE task on `jeff-ubuntu`.
- **Reference Bead**: [`jleechan-xn4n`](https://github.com/jleechanorg/dark-factory/issues/jleechan-xn4n) / [`jleechan-lssy`](https://github.com/jleechanorg/dark-factory/issues/jleechan-lssy).

### 2. **Fold `web-advice-failopen.dot` into Existing Pipelines (P1 Feature)**
- **Goal**: Integrate the proven `web_advice` fail-open node from `pipelines/factory/web-advice-failopen.dot` into the canonical target pipelines.
- **Target Pipelines**:
  1. `pipelines/factory/pr_gates.dot` (min 5 lines diff)
  2. `pipelines/factory/gates.dot` (min 5 lines diff)
  3. `pipelines/slim/minimal_pr.dot` (min 20 lines diff)
- **Acceptance Criteria**:
  - Unconditional downstream edge from `web_advice` to next node (fail-open guarantee).
  - Graph audit clean (`cargo test` + `graph_audit.py`).
  - PR opened with `[antig]` prefix.
- **Dependencies / Blockers**: Low risk; can be authored locally and tested.
- **Reference Bead**: [`jleechan-azso`](https://github.com/jleechanorg/dark-factory/issues/jleechan-azso) ([Issue #668](https://github.com/jleechanorg/dark-factory/issues/668)).

### 3. **Address Lane D 5 Cosmetic/Structural Follow-ups (P2 Resilience)**
- **Goal**: Triage and resolve the 5 structural items from `docs/web-advice-failopen-e2e-log.md` §5.
- **Items**:
  1. Tighten `cdp_port` probe to check Aside CLI responsiveness rather than bare TCP port 9222.
  2. Fix JSON heredoc escape sequence in `scripts/af-test-web-advice-failopen.sh`.
  3. Update direct-invocation script module docstrings for import compatibility.
  4. Add `preflight --require-holdouts` fail-fast flag.
- **Acceptance Criteria**: All 5 items resolved via code fix + test, or marked `ACCEPT-AS-DEGRADED` with rationale.
- **Reference Bead**: [`jleechan-57ym`](https://github.com/jleechanorg/dark-factory/issues/jleechan-57ym) ([Issue #669](https://github.com/jleechanorg/dark-factory/issues/669)).

### 4. **Address Lane E/F Remediation Candidates (P3 Hygiene)**
- **Goal**: Implement operational ergonomics improvements identified post `--admin` merges.
- **Items**:
  - Surface runner pool outage warnings in PR comments when `mergeStateStatus` stays UNSTABLE > 10m.
  - Document cross-language anchor comment conventions (`//` vs `#`).
  - Clean up legacy beads missing `target_repo` metadata.
- **Acceptance Criteria**: Triage into code change vs documentation.
- **Reference Bead**: [`jleechan-gagl`](https://github.com/jleechanorg/dark-factory/issues/jleechan-gagl) ([Issue #670](https://github.com/jleechanorg/dark-factory/issues/670)).

## PR / merge state

- [PR #714](https://github.com/jleechanorg/dark-factory/pull/714): **MERGED** (`f5d0c7fd` / `425cd517`) — `[antig] docs(roadmap): sync activity log and nextsteps (2026-08-22)`
- [PR #671](https://github.com/jleechanorg/dark-factory/pull/671): **MERGED** (`0f5a077e`) — `feat(daemon): af end-to-end W1 gaps (rev-ffb26 + rev-6i2kp)`
- [PR #667](https://github.com/jleechanorg/dark-factory/pull/667): **OPEN** — `[antig] feat(ao): standardize 100% on AO via prompt indirection (.factory/prompt.md)`
- [PR #666](https://github.com/jleechanorg/dark-factory/pull/666): **MERGED** (`bc8a779d`) — `fix(adapters): gate offline fixture reads behind #[cfg(test)]`
- [PR #665](https://github.com/jleechanorg/dark-factory/pull/665): **MERGED** (`aac9bc23`) — `fix(adapters): preserve mergeable=null as UNKNOWN`
- [PR #664](https://github.com/jleechanorg/dark-factory/pull/664): **OPEN (DRAFT)** (`f3caec5c`) — Lane D /af test fixture (do not merge)

## Learnings pointer

- Appended to [`~/roadmap/learnings-2026-08.md`](file:///home/jleechan/roadmap/learnings-2026-08.md): `## 2026-08-24 — AO 100% Standardization via Prompt Indirection & Runner Pool Blocker Triage`.

## Roadmap pointer

- Appended [`roadmap/activity/2026-08-24.md`](file:///home/jleechan/projects/dark-factory/roadmap/activity/2026-08-24.md) with session summary.
- Updated [`roadmap/README.md`](file:///home/jleechan/projects/dark-factory/roadmap/README.md) `## Recent activity (by day)` section with `2026-08-24` link.
