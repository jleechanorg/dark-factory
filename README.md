# 🏭 Dark Factory — Attractor-Pattern DOT Pipeline Runner

[![Supported Runtime: Python 3.13](https://img.shields.io/badge/python-3.13-blue.svg)](https://www.python.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Build Status](https://img.shields.io/badge/build-passing-green.svg)]()

A state-of-the-art Python implementation of the **Attractor pattern**: a robust, DOT-based pipeline engine designed to orchestrate complex multi-agent software engineering workflows using directed graphs. By shifting the unit of durability from ephemeral agent logs to version-controlled process graphs (`.dot`), Dark Factory enables fully autonomous, lights-out development pipelines.

---

## 📚 Foundational Research & Inspiration

Dark Factory stands on the shoulders of groundbreaking work in agentic software engineering and Level 5 automation. Our design, paradigms, and benchmark splits are inspired by the following research, reference implementations, and articles:

### 🔬 Core Specifications & Benchmarks
*   StrongDM, **AttractorBench** — [GitHub Repository](https://github.com/strongdm/attractorbench)
    *   *Defines the benchmark paradigm:* Agents ingest a public natural-language specification, while the deterministic evaluation and test suites are held out locally to prevent LLM training-data contamination.
*   jleechanorg, **AttractorBench Fork** — [GitHub Repository](https://github.com/jleechanorg/attractorbench)
    *   *Our public experimental baseline:* Sandbox for validating spec-review workflows and cross-repository agent validation mechanics.

### 📝 Paradigms & Articles
*   Dan Shapiro, **"You don't write the code"** — [Blog Post](https://www.danshapiro.com/blog/2026/02/you-dont-write-the-code/)
    *   *The philosophy of Level 5 Automation:* In the ultimate "Dark Factory," humans never read or write code. Instead, they specify intent and design outcomes. Quality is enforced via observability, automated healers, and adversarial reviews.
*   Simon Willison, **"The Software Factory"** — [Blog Post](https://simonwillison.net/2026/Feb/7/software-factory/)
    *   *The Industrialization of AI Coding:* Shifting the industry mindset from bespoke chat boxes to repeatable, high-volume automated pipeline execution.
*   2389 Research, **"The Dark Factory is a .dot file"** — [Blog Post](https://2389.ai/posts/the-dark-factory-is-a-dot-file/)
    *   *The Durable Artifact Principle:* While the pipeline runner code itself is disposable (*dorodango* — polished, discarded, and rebuilt), the pipeline `.dot` file remains the precious, versioned representation of the software engineering process.

### 🛠️ Reference Implementations
*   StrongDM, **Attractor** — [GitHub Repository](https://github.com/strongdm/attractor)
    *   The original reference harness establishing the foundational Attractor paradigms.
*   Dan Shapiro, **Kilroy** — [GitHub Repository](https://github.com/danshapiro/kilroy)
    *   A pioneering implementation of the Attractor loop.
*   2389 Research, **Smasher** — [GitHub Repository](https://github.com/2389-research/smasher)
    *   An adversarial reviewer and compiler-checked coding harness.
*   2389 Research, **Mammoth** — [GitHub Repository](https://github.com/2389-research/mammoth)
    *   An LLM orchestration and workspace synchronization harness.
*   2389 Research, **Tracker** — [GitHub Repository](https://github.com/2389-research/tracker)
    *   A high-performance pipeline and task state tracker.
*   2389 Research, **dippin-lang** — [GitHub Repository](https://github.com/2389-research/dippin-lang)
    *   A domain-specific language designed for expressing declarative dataflow pipelines.

### 🎯 Advanced Techniques & Methodologies
*   StrongDM, **Techniques** — [Techniques Directory](https://factory.strongdm.ai/techniques)
    *   A curated catalog of tactical strategies for robust agentic execution.
*   StrongDM Techniques, **Direct-to-Unit (DTU)** — [DTU Documentation](https://factory.strongdm.ai/techniques/dtu)
    *   *The DTU Pattern:* Bypassing middle layers to directly generate targeted unit tests and implementation assertions, ensuring immediate execution feedback.

---

## 🔒 The Operator-Agent Isolation Rule

To guarantee rigorous evaluation integrity, Dark Factory strictly enforces **sandboxed agent execution**:

> [!IMPORTANT]
> **Adversarial Separation Constraint:** The spawned implementing agent (the `codergen` worker running Claude Code, Codex, or an Antigravity session) is completely blind to holdout test suites (`holdouts/`, `runner/evaluator.py`, and `_holdout/` paths).
>
> *   **Operational Enforcement:** The runner CLI dynamically invokes the implementing agent under `sandbox-exec` with strict OS-level permissions denying read access to the local holdouts directory.
> *   **Environment Isolation:** All environment variables pointing to holdout files (`DARK_FACTORY_HOLDOUTS`, etc.) are actively stripped from the agent subprocess env via `_sanitized_env()`.
>
> As the **Operator**, you have full access to view tests, event logs, and holdouts. You must ensure that no holdout-derived hints or test code leaks into the prompt templates consumed by the coder nodes.

---

## 📐 Architecture: 3-Layer Convergence

Dark Factory operates as the top layer of a modern, modular agentic stack:

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Layer 3: Pipeline Engine (Dark Factory)                                 │
│ ➔ Parses .dot graphs, traverses nodes, enforces rules, logs history     │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │
                                     ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Layer 2: Agent Loop (CLIs & Orchestrators)                              │
│ ➔ Claude Code, Codex, Antigravity (agy), WorldArchitect (ao)            │
└────────────────────────────────────┬────────────────────────────────────┘
                                     │
                                     ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Layer 1: Unified LLM Client                                             │
│ ➔ OpenClaw Gateway, wafer proxy, thinclaw MCP                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 🚀 State-of-the-Art Execution Backends

Dark Factory ships with first-class support for diverse LLM and agent backends, dynamically selected per node via DOT attributes or global flags:

1.  **`agy` (Antigravity CLI Backend) [New & Recommended]**
    *   Invokes the powerful Antigravity execution engine headlessly and non-interactively using:
        ```bash
        agy --add-dir <cwd> --dangerously-skip-permissions --print --print-timeout <s_s>
        ```
    *   Wraps prompts automatically in a headless instruction wrapper, directing Antigravity to decompose the task, spawn parallel internal subagents if useful, apply direct workspace changes, and exit cleanly without waiting for interactive input.
2.  **`claudew` (Wafer-Driven Claude Backend) [New]**
    *   Leverages the `WAFER_API_KEY` to route requests through a high-performance local wafer proxy.
    *   Sets `ANTHROPIC_BASE_URL="http://localhost:9001"` and defaults to the premium `GLM-5.1` model (configurable via `WAFER_MODEL`) running with high effort (`--effort high`) and a generous 30-minute execution timeout.
3.  **`claude` (Claude Code CLI)**
    *   Directly invokes `claude` with standard prompt text to run autonomous editing and execution tasks.
4.  **`codex` (Codex CLI)**
    *   Uses `codex exec` to drive immediate terminal or workspace updates.
5.  **`ao` (WorldArchitect Agent)**
    *   Spawns an autonomous workspace worker mapped to an active WorldArchitect project (`--ao-project`).
6.  **`mock_llm` / `echo`**
    *   Deterministic test backends used for validation, smoke testing, and continuous integration pipelines.

---

## 📊 Pipeline Catalog

Our pipelines are designed to support a wide range of complexity, from simple test lanes to full-scale multi-stage development pipelines:

### 1. The Gates Pipeline (`pipelines/factory/gates.dot`)
An end-to-end multi-agent pipeline:
*   **Specify:** Parses natural language specs and generates target code designs.
*   **Implement:** Translates designs into physical code modifications.
*   **Gate Verification:** Executes deterministic compiler checks and runs static reviews.
*   **Holdout Evaluation:** Runs sealed verification suites to evaluate performance.

### 2. Minimal Feature & Style Sheets (`pipelines/slim/minimal_feature.dot`)
*   Demonstrates Dark Factory's support for **CSS-like model stylesheets** (`.model.css`).
*   Rather than cluttering every node with explicit models or API parameters, the runner applies custom routing rules dynamically at runtime based on selectors (e.g., matching node names, shapes, or types to specific model profiles).

### 3. Spec Review Benchmark (`benchmarks/attractor-spec-review/`)
Designed to evaluate and refine natural-language specs:
*   `review_slim.dot`: Rapid, single-pass specification audit and smoke check.
*   `review_full.dot`: In-depth spec review featuring multi-stage auditing, full-stack smoke environments, and independent reviewer audits via `codex exec --yolo`.

---

## 🩺 Fail-Closed Observability: CXDB + Healer

To build a true "Dark Factory," errors must be logged, clustered, and resolved programmatically:

```
 ┌────────────────┐       Nodes Execute       ┌────────────────┐
 │  Graph Runner  ├──────────────────────────►│   CXDB Log     │
 └────────────────┘                           └───────┬────────┘
         ▲                                            │
         │ Reads Prescription &                       │ Reads SQLite
         │ Heals Prompt/Config                        ▼ WAL Steps
 ┌───────┴────────┐                           ┌────────────────┐
 │     Human      │◄──────────────────────────┤   Healer Engine│
 └────────────────┘     Emits MD Report       └────────────────┘
```

*   **CXDB SQLite Event Log (`runner/cxdb.py`):**
    *   Every step, node transition, stdout snippet, stderr traceback, metadata chunk, and execution cost metric is recorded in a centralized SQLite database.
    *   Runs in high-concurrency **Write-Ahead Logging (WAL) mode** with a robust `5000ms` busy timeout to guarantee concurrent multi-pipeline instances write safely without database locking errors.
*   **The Healer Engine (`runner/healer.py`):**
    *   Reads the CXDB log and dynamically clusters terminal failures (e.g., `failure`, `exhausted`, `stuck`, `error`) by `(node, outcome, output_hash)`.
    *   Invokes an LLM review backend (`claude` or `echo`) to analyze the precise error context and output.
    *   Generates a structured, highly actionable Markdown report containing a tailored **prescription** (e.g., which prompt to refine, which code logic failed, or which harness configuration is broken) for every cluster.

---

## 📁 Directory Layout

```
dark-factory/
├── pipelines/                      # Durable .dot pipeline graph definitions
│   ├── factory/                    # Production factory runs (gates.dot, hello.dot)
│   └── slim/                       # Minimal templates & styling stylesheets (.model.css)
├── benchmarks/                     # Public NLSpecs, starter assets, and benchmarks
│   ├── amazon-clone/               # Full-stack E-Commerce benchmark setup
│   ├── airbnb-clone/               # Firebase emulator-backed benchmark
│   └── attractor-spec-review/      # Spec review pipelines (review_slim/full.dot)
├── specs/                          # System feature specs (visible to implementing agents)
├── prompts/                        # System-wide prompt templates referenced by DOT nodes
├── runner/                         # The Python DOT Pipeline Engine
│   ├── __init__.py
│   ├── __main__.py                 # Command line interface & parser arguments
│   ├── parser.py                   # PyDot loader & validator (verifies start/exit)
│   ├── engine.py                   # Graph traversal, state tracking, & loop limits
│   ├── handlers.py                 # Core handlers & backends (agy, claudew, claude, ao)
│   ├── cxdb.py                     # SQLite WAL step recording and logging schemas
│   └── healer.py                   # Failure clustering & automatic prescription engine
├── tests/                          # Rigorous unit and integration test suite
├── CLAUDE.md                       # Agent instructions (auto-loaded by Claude Code)
├── AGENTS.md                       # Identical instructions for all other AI agents
└── README.md                       # This premium documentation
```

---

## 🍳 Execution Cookbook

### ⚙️ One-time install (binary, not source)

Dark Factory runs via the **`dark-factory` binary** — not `python -m runner`
from a raw checkout. Install once with [uv](https://docs.astral.sh/uv/):

```bash
git clone https://github.com/jleechanorg/dark-factory ~/projects/dark-factory
cd ~/projects/dark-factory
./install.sh

export DARK_FACTORY_HOME=~/projects/dark-factory
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts
export PATH="$HOME/.local/bin:$PATH"
```

This installs a uv-managed Python 3.13 venv, PyPI deps (`pydot`, `PyYAML`), and
symlinks `dark-factory` + `df-healer` into `~/.local/bin`.

Run pipelines from the **target repo** (implementation workdir = cwd). Graphs and
prompts load from `$DARK_FACTORY_HOME`. See [docs/pipeline-selection.md](docs/pipeline-selection.md)
— **pick the `.dot` for the task**, do not always use the same default.

### 💨 1. Smoke pipeline (no LLM cost)

```bash
cd ~/projects/dark-factory   # or any repo; hello resolves via DARK_FACTORY_HOME
dark-factory --pipeline pipelines/factory/hello.dot --goal "smoke test" --backend echo
```

### 🆕 2. New feature (full loop)

```bash
cd ~/projects/my-app
dark-factory \
  --pipeline pipelines/slim/minimal_feature.dot \
  --goal "Add user-facing feature X" \
  --backend claude \
  --feature my_feature \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

### 🔁 3. In-flight PR iteration (no holdout)

```bash
cd ~/projects/my-app
dark-factory \
  --pipeline pipelines/slim/minimal_pr.dot \
  --goal "Fix failing tests on PR branch" \
  --backend claude \
  --state 'slim.test_command=pytest tests/test_foo.py -v' \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

### 🛡️ 4. Gates-only validation (diff already implemented)

```bash
cd ~/projects/my-app
dark-factory \
  --pipeline pipelines/factory/gates.dot \
  --goal "Validate JWT middleware diff" \
  --backend claude \
  --feature auth_middleware \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

### 🩺 5. Healer failure audit

```bash
df-healer --cxdb ~/.dark-factory/cxdb.sqlite
```

### 🧪 Tests (dev)

```bash
cd ~/projects/dark-factory
.venv/bin/python -m pytest tests/
```

---

*“The pipeline definitions encode the development process itself. Rebuild the code, polish the dorodango, but keep the graph.”*
