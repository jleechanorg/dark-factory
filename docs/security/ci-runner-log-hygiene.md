# CI and Runner Diagnostic Log Hygiene

## Overview

When the `dark-factory` pipeline runner executes tasks or exhausts retries on branch lanes, it emits ephemeral failure diagnostics and logs:

- `failed_run_log*.txt` (e.g. `failed_run_log.txt`, `failed_run_log2.txt`, `failed_run_log_<timestamp>.txt`) — human-readable failure dump containing stdout/stderr streams, runner boot output, environment diagnostics, and error traces.
- `branch_fail_step_*` (e.g. `branch_fail_step_a3k9`, `branch_fail_step__ayz83rw`, `branch_fail_step_hg0iohpa`) — per-step failure markers written under the worktree to identify which pipeline node stopped an execution lane.

## Security & Repository Hygiene Policy

These files contain runtime infrastructure details (such as host OS configurations, runner versions, cloud region data, and runtime environment headers). They are strictly ephemeral diagnostics and **must never be committed to source control**.

The root `.gitignore` enforces this with patterns:
```gitignore
failed_run_log*.txt
branch_fail_step_*
```

## Where CI and Runner Logs Belong Instead

1. **Performance and Run Logs (`~/Library/Logs/dark-factory/`)**:
   - Structured JSONL and human-readable run logs are stored outside the repository tree under `~/Library/Logs/dark-factory/<repo-slug>/<branch-slug>/` (macOS default and Linux standard path).
   - This location survives reboots, worktree teardowns, and retag cycles without polluting the git index.

2. **CXDB Event Store (`~/.dark-factory/cxdb.sqlite`)**:
   - Structured step-by-step execution traces, step hashes, verdicts, and failure classifications are recorded in SQLite WAL database files for Healer analysis.

3. **CI Run Artifacts (GitHub Actions)**:
   - In automated GitHub Actions workflows, ephemeral logs and test reports should be uploaded via `actions/upload-artifact` with standard retention periods rather than written into the repository worktree.

4. **Evidence Gists & Public Attestations**:
   - Sanitized verification bundles and test evidence required for gate promotions are published to public Gists referenced in pull request descriptions.
