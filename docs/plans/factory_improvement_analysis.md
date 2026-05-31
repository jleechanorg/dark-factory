# 📐 Dark Factory: Advanced Architectural Roadmap (Without Coding)

This document presents a comprehensive architectural analysis and improvement roadmap for the **Dark Factory** pipeline engine and orchestrator. It addresses the silent degradation and crash issues observed in recent runs, establishing a path toward Level 5 automation reliability.

---

## 🗺️ Architectural Reliability Model

```mermaid
graph TD
  subgraph Pre-Flight
    p_parse[Parser AST Check] --> p_validate[Pre-Flight Schema Guard]
    p_validate --> p_exist[Resolve Paths & Handlers]
  end

  subgraph Runtime Execution Boundary
    p_exist --> r_start[Run Start]
    r_start --> r_wal[WAL Checkpoint]
    r_wal --> r_exec[Node Handler Execution]
    
    r_exec -- Exception --o r_panic[Global Panic Hook]
    r_exec -- Success --▶ r_route[Edge Condition Evaluator]
    
    r_panic --> r_cxdb[CXDB Panic Step]
    r_cxdb --> r_log[JSONL Log Capture]
    r_log --> r_exit_fail[Exit Code 128]
    
    r_route -- Next Node --▶ r_wal
    r_route -- Exit --▶ r_exit_ok[Exit Code 0]
  end
```

---

## 🔍 Core Improvement Pillars

### 1. 🛡️ Unhandled Exception Boundaries (Global Panic Hook)
* **Problem**: Unhandled exceptions inside node handlers, thread-pools, or SQLite connections can bypass standard `try-except` blocks, silently killing the runner and causing "invisible" degradation that is mis-diagnosed as a one-off.
* **Solution (No-Code Design)**:
  * Implement a **Global Runner Exception boundary** wrapping the entire `runner.engine:run()` loop.
  * Catch any `BaseException` (including keyboard interrupts and system exits) and trigger a deterministic **Panic Hook**:
    1. Log the full traceback to the run's active log file.
    2. Write a synthetic `panic` StepRecord to CXDB (with outcome `error` and traceback preview).
    3. Terminate with a distinctive exit code (`128`) so that the Healer CLI can instantly cluster it as a runner/orchestrator crash.

### 2. 📝 Structured JSONL Logging & Traceability
* **Problem**: Current logs are written as unstructured text files, and subprocess tracebacks can be truncated or dropped upon crash, losing vital context.
* **Solution (No-Code Design)**:
  * Standardize all run logs to **Structured JSON Lines (JSONL)**. Each line should be a self-contained record:
    ```json
    {"timestamp": 1716301234, "event": "node_visit", "node": "implement", "visits": 1}
    {"timestamp": 1716301239, "event": "subprocess_spawn", "node": "test", "cmd": "pytest tests/..."}
    {"timestamp": 1716301245, "event": "node_exception", "node": "test", "exception": "TimeoutExpired", "traceback": "..."}
    ```
  * Maintain a separate complete subprocess stdout/stderr ring buffer. In case of failure, dump the *entire* stdout/stderr buffer to the structured log file.

### 3. 🚦 Static Pre-Flight Graph Validation
* **Problem**: The runner currently starts executing graphs immediately, failing only when it encounters missing handlers or prompt paths mid-run, wasting execution costs.
* **Solution (No-Code Design)**:
  * Introduce a **Pre-Flight Validation Pass** inside `runner/parser.py` that inspects the AST before executing:
    * Assert that every node `type` maps to a registered handler in `TYPE_REGISTRY`.
    * Assert that all `@relative` prompt templates (e.g. `prompts/slim/plan.md`) exist on disk.
    * Assert that all `command="..."` strings are parsed and have valid shell executable binaries.
    * Check edge condition strings for syntax correctness to prevent downstream parsing crashes.

### 4. ⏳ Dynamic LLM Timeouts & Provider Backoff
* **Problem**: Standard timeout defaults (`1200` seconds) are extremely high. When provider APIs hang, the orchestrator silently waits for up to 20 minutes, exhausting system resources.
* **Solution (No-Code Design)**:
  * Classify nodes and establish adaptive timeouts:
    * `tool` / local tests: 60s
    * `codergen` / LLM generation: 180s
    * `review` / deep auditing: 300s
  * Enforce standard exponential backoff on HTTP/JSON layer calls within handlers to absorb temporary provider outages.

### 5. 🔄 WAL Checkpoint Engine & Self-Healing Resume
* **Problem**: Mid-run orchestrator crashes can corrupt the single JSON checkpoint file, requiring manual `--resume` interventions.
* **Solution (No-Code Design)**:
  * Implement **Write-Ahead Logging (WAL) checkpointing** inside the SQLite CXDB itself. 
  * Prior to executing any handler, write the atomic execution frame (`current_node`, `history`, `state`, `visits`) to a local WAL transaction.
  * Upon startup, the runner should automatically query CXDB for any active, incomplete runs. If found, it should offer a self-healing automatic resume from the last stable WAL checkpoint.

---

> [!NOTE]
> This analysis is prepared as a non-code architectural roadmap. All suggested design patterns align with Zero-Framework Cognition (ZFC) and Root-Cause-First engineering principles, ensuring maximum reliability under Level 5 automation.
