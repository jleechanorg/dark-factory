# Gemini / Antigravity Global Baseline & Repository-Specific Guidelines

This document serves as the canonical repository-level configuration, guidelines, and operating procedures for Gemini and Antigravity coding sessions in the `dark-factory` codebase. It aligns with the general principles in `CLAUDE.md`/`AGENTS.md` but is strictly customized for the Antigravity agentic workflow, safety gates, and local environment.

---

## 1. Introduction & Core Design Principles

The `dark-factory` repository implements the **Attractor Pattern**—a Level 5 automated pipeline runner that processes multi-stage AI workflows using directed DOT graphs. 

### Key Tenets
* **Level 5 Automation (The Dark Factory)**: Lights-off engineering. Humans own specs, holdouts, validation economics, and outcome audits. Agents write code, and independent review agents/sealed evaluators audit the behavior. Human diff review is a fallback, not the quality gate.
* **Durable Artifacts vs. Dorodango**: The DOT graphs under `pipelines/` are the durable process code worth sharing. The Python code in `runner/` is treated as throwaway ("dorodango" — polish, discard, rebuild from spec). The true system intelligence accumulates in the **CXDB** event logs.
* **Sealed Evaluator Isolation**: Implementing agents (under test) must **NEVER** read `holdouts/`, `runner/evaluator.py`, or any `_holdout/` test source files. This is enforced at runtime by `sandbox-exec` and by stripping `$DARK_FACTORY_HOLDOUTS` from the coder's environment. As an Antigravity agent, you must respect this boundary and never look inside these hidden directories.
* **Deprecate human interactive hat**: Shift all coding LLM work to the auto-factory. Humans only create GitHub issues/beads and write comments on PRs for feedback. The auto-factory autonomously processes and drives branches to green without human-interactive coding sessions.
* **Agent Orchestrator (AO) Reference-Only Contract**: The factory uses upstream Golang `agent-orchestrator` (`https://github.com/strongdm/agent-orchestrator` / `jleechanorg/agent-orchestrator`), NOT `agent-orchestrator-ts`. The AO repository is strictly read-only reference material. Never modify or submit PRs against AO; all session parsing and reaping logic must be implemented inside `dark-factory`. Hard Gate: Agents may NEVER write or modify AO code unless the user explicitly commands `AO CODE APPROVED`.
* **AO CLI Wrapper Fidelity**: Host CLI wrappers for `ao` or any external tool must never modify, strip, or suppress machine-readable flags (such as `--json`, `--format`, `--porcelain`). Wrappers must pass through all arguments transparently to preserve downstream parsing contracts.
* **AO Project Scoping**: All `ao status` and session management queries from `dark-factory` must be project-scoped using `-p <project>` to avoid scanning all registered host repositories and hitting rate limits or incurring unnecessary latency.


---

## 2. Technology Stack & Key Assets

| Component | Technology / Location | Description |
| :--- | :--- | :--- |
| **Runtime** | Python 3.13 | Canonical language and runtime for the runner engine. |
| **Pipeline Graphs** | `pipelines/` & DOT Language | Traversal maps defining the sequence of nodes and edge logic. |
| **Runner Engine** | `runner/` | PythonDOT traversal engine with Parser, Engine, and Handler registries. |
| **Event Logging** | `runner/cxdb.py` (SQLite) | Event log tracking every execution step under WAL mode to avoid database locking. |
| **Failure Diagnosis** | `runner/healer.py` | Audits CXDB, clusters failures, and issues actionable repair prescriptions. |
| **Spec validation** | `benchmarks/attractor-spec-review/` | Copy of the attractor spec-validation benchmark used for validating specs. |
| **Node.js** | Node 22 (nvm) | Node 22 (`~/.nvm/versions/node/v22.22.0/bin/node`) is the canonical runtime for any Node scripts or MCP integrations in this environment. |

---

## 3. Mandatory Context Verification Hooks

You **MUST** run context and header verification checks on every turn to ensure branch alignment and prevent context corruption.

### Hook 1: Header Check Verification
Run the following script immediately after starting any work:
```bash
python3 ~/.claude/commands/header_check.py
```

### Hook 2: Response Header Generation
At the absolute end of **EVERY** response to the user, you must run the header script and append its exact stdout as the final element (after all text and code blocks):
```bash
$(git rev-parse --show-toplevel)/.claude/hooks/git-header.sh --with-api
```

---

## 4. Git & Branch Management

> [!IMPORTANT]
> **ZERO-TRUST STATE VERIFICATION**
> Never act on, review, or recommend changes for closed or merged Pull Requests. Apply the following rules rigorously.

### PR State Lock Rules
1. **Dynamic Check Before Any Action**: Before checking out, analyzing, or spawning subagents for a PR, query its state dynamically:
   ```bash
   gh pr view <PR_NUMBER> --json state
   ```
2. **60-Second Time Lock**: Do not rely on PR state information older than 60 seconds. Re-query dynamically before finalizing any lists or recommendations.
3. **Explicit State Tagging**: Every PR reference in your output must include its state tag in uppercase (e.g. `🟢 [PR #123](url) [OPEN]` or `🔴 [PR #456](url) [MERGED]`).
4. **Immediate Halting**: If a PR is `CLOSED` or `MERGED`, instantly terminate all planned implementation, reviews, or subagent tasks associated with it.

### PR Referencing and Target Constraints
* **Full URL Markdown Links**: Always write the full GitHub URL when referring to a PR or commit. Never use bare numbers or SHAs.
  * **Correct**: `[PR #6420](https://github.com/jleechanorg/worldarchitect.ai/pull/6420)`
  * **Incorrect**: `PR #6420` or `#6420`
* **Target PR Remote**: **NEVER** open or target a PR against upstream third-party repositories. Always target `jleechanorg/dark-factory`.
* **PR Title Prefixing**: Any PR created, modified, or pushed to by Antigravity must be prefixed with `[antig]`.
* **Standard Pushing Only**: Use explicit remotes (`git push origin <branch>`). **NEVER** use `--force` or `-f` to push. Let Git's fast-forward safety protect concurrent human and agent changes.

---

## 5. Testing Boundaries & Anti-Hang Policy

> [!WARNING]
> **LOCAL TESTING CONSTRAINT**
> Do not execute the entire/broad unit test suite locally. Rely on GitHub Actions CI for complete test verification. Only run highly targeted, specific tests directly relevant to your active changes.

### Anti-Hang Test Termination
When stopping Python tests or background servers (like `gunicorn` or dev servers), do not rely on standard graceful `SIGINT` tools. They often hang on file descriptors, causing 5-minute timeouts.
* **Mandatory Procedure**: Use a direct hard-kill command:
  ```bash
  pkill -9 -f "test_script_name" && pkill -9 -f "gunicorn.*mvp_site.main:app"
  ```

---

## 6. PR Greenness & Gate 6 Evidence Standards

A PR is considered merge-ready when all **7-green** gates pass. Never assume success based on the API return status alone; always inspect raw logs (`gh run view <ID> --log | grep "GATE-"`).

### Gate 6 (Evidence Standards) Checklist
Before marking Gate 6 as PASS, you must answer explicitly what the evidence proves vs what it does not. Check the following:
* SHA in the evidence matches current HEAD.
* LLM-layer pass rate is >0%.
* Evidence files/artifacts are successfully uploaded and accessible (not pointing to `/tmp/`).
* Dynamic status check confirms that the PR has not diverged.

### Human Merge Approval Requirement
> [!CAUTION]
> **NEVER** execute `gh pr merge` or merge any PR unprompted, even if all 7 automated gates are green (the canonical gates from `daemon/src/verifier.rs::GateName`: ci_green, no_conflicts, coderabbit, bugbot, comments_resolved, evidence_review, skeptic). The `/code-standards` and `/zfc` checks are separate human-approval-time checks — they are never auto-merged without human `MERGE APPROVED`. You **MUST** wait for the human user to explicitly type `MERGE APPROVED` in the chat before carrying out the merge.

---

## 7. Worktree Protection & Post-Task Self-Cleanup

To prevent disk bloat from accumulating isolated environments and virtualenvs, always clean up your current-branch worktree upon task completion.

### Post-Task Worktree Self-Cleanup Script
Run this cleanup routine before sending your final response:
```bash
WORKTREE_PATH="${HOME}/.gemini/antigravity/worktrees/dark-factory/$(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
if [[ -d "$WORKTREE_PATH" ]]; then
  rm -rf "$WORKTREE_PATH" && echo "Cleaned up $WORKTREE_PATH"
fi
```
* **Worktree Protection**: Do not run `git worktree prune` globally, and never modify or delete worktrees created by human developers.

---

## 8. Install & CLI Commands

### One-time install (binary)

```bash
cd ~/projects/dark-factory && ./install.sh
export DARK_FACTORY_HOME=~/projects/dark-factory
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
export PATH="$HOME/.local/bin:$PATH"
```

Use **`dark-factory`** and **`df-healer`** on PATH — not `python -m runner` from
source. Run from the **target repo** cwd; pipelines resolve from `$DARK_FACTORY_HOME`.

### Pipeline selection

When `--pipeline` is omitted, `/f` and `/factory` default to `pipelines/slim/two_node.dot` (generic worker + fresh fully tooled Codex reviewer). Pass explicit `--pipeline <name>` to opt into non-default pipelines (see [docs/pipeline-selection.md](docs/pipeline-selection.md)). Controller/cold-review graphs and reviewer calibration are explicit opt-ins, not the ordinary default route.

| Task | Pipeline |
|------|----------|
| Smoke | `pipelines/factory/hello.dot` |
| New feature (full) | `pipelines/slim/minimal_feature.dot` |
| PR iteration | `pipelines/slim/minimal_pr.dot` |
| Gates + holdout | `pipelines/factory/gates.dot` |
| PR gates only | `pipelines/factory/pr_gates.dot` |

### Examples

* **Smoke (echo, no LLM)**:
  ```bash
  dark-factory --pipeline pipelines/factory/hello.dot --goal "smoke test" --backend echo
  ```
* **Gated run with CXDB**:
  ```bash
  cd ~/projects/target-repo
  dark-factory \
    --pipeline pipelines/factory/gates.dot \
    --goal "<goal_description>" \
    --backend ao \
    --ao-agent antigravity \
    --feature <feature_name> \
    --cxdb ~/.dark-factory/cxdb.sqlite
  ```
* **Healer**:
  ```bash
  df-healer --cxdb ~/.dark-factory/cxdb.sqlite
  ```
* **Visualize DOT**:
  ```bash
  dot -Tpng pipelines/factory/gates.dot -o gates.png
  ```

### Local tests (dev only)

```bash
.venv/bin/python -m pytest tests/test_engine.py -k green
```

## /af — ZERO direct work; monitoring only (operator hard rule)

When the operator directs work through /af (or sets an /af goal), the session
does **ZERO direct work** — no product code, no factory code, no hand-fixes,
no coding sub-agent lanes. The session's ONLY jobs:

1. File/refine `factory`-labeled beads (SHORT descriptions — AO's spawn
   prompt cap is 4096 characters; a long bead body makes its own dispatch
   fail) with `target_repo:` / `existing_pr:` / `existing_branch:` fields
   when driving an existing PR.
2. Monitor daemon telemetry on `jeff-ubuntu` via SSH (`/home/jleechan/Library/Logs/dark-factory/daemon.jsonl`)
   and report each lifecycle stage.
3. Escalate blockers the factory cannot self-fix to the OPERATOR, naming the
   exact blocker — never hand-fix them. "The factory is broken so I'll do it
   myself" is the forbidden move: it hides factory gaps and makes the
   label→merge E2E proof unfalsifiable (2026-07-11/12 incidents: hand-driven
   PRs masked a dead coder loop for a full day).

## Factory host placement (Linux-only)

`jeff-ubuntu` is the sole Auto-Factory host. Start, stop, inspect, and deploy
the daemon only through `/linux` (`ssh jeff-ubuntu ...`) and its user systemd unit
`ai.dark-factory.daemon.service`. AO worker dispatch runs on that Linux host only.
This Mac is an operator client: do not load or start a Dark Factory LaunchAgent,
a local daemon, or local AO workers from factory intake on macOS.
Always use SSH to Linux for telemetry (`/home/jleechan/Library/Logs/dark-factory/daemon.jsonl`)
and operational control.

