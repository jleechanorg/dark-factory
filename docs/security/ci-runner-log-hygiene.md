# CI and Runner Diagnostic Log Hygiene

## Overview

When the `dark-factory` pipeline runner executes tasks or exhausts retries on branch lanes, it emits ephemeral failure diagnostics and logs:

- `failed_run_log*.txt` (e.g. `failed_run_log.txt`, `failed_run_log2.txt`, `failed_run_log_<timestamp>.txt`) — human-readable failure dump containing stdout/stderr streams, runner boot output, environment diagnostics, and error traces.
- `branch_fail_step_*` (e.g. `branch_fail_step_a3k9`, `branch_fail_step__ayz83rw`, `branch_fail_step_hg0iohpa`) — per-step failure markers written under the worktree to identify which pipeline node stopped an execution lane.

## Security & Repository Hygiene Policy

These files contain runtime infrastructure details (such as host OS configurations, runner versions, cloud region data, and runtime environment headers). They are strictly ephemeral diagnostics and must not be committed to source control.

The root `.gitignore` excludes matching untracked diagnostic paths from standard Git operations:
```gitignore
failed_run_log*.txt
branch_fail_step_*
```

## Where CI and Runner Logs Belong Instead

1. **Performance and Run Logs (`~/Library/Logs/dark-factory/<repo-slug>/<branch-slug>/`)**:
   - Structured JSONL and human-readable run logs are stored outside the repository tree under `~/Library/Logs/dark-factory/<repo-slug>/<branch-slug>/`. This is macOS's standard per-app log directory, and is mirrored as the project-standard log root across hosts (including Linux `jeff-ubuntu`).
   - This location survives reboots, worktree teardowns, and retag cycles without polluting the git index.

2. **CXDB Event Store (`~/.dark-factory/cxdb.sqlite`)**:
   - Structured step-by-step execution traces, step hashes, verdicts, and failure classifications are recorded in SQLite WAL database files (`~/.dark-factory/cxdb.sqlite` or `--cxdb <path>`) for Healer analysis.

3. **CI Run Artifacts (GitHub Actions)**:
   - In automated GitHub Actions workflows, ephemeral logs and test reports should be uploaded via `actions/upload-artifact` with standard retention periods rather than written into the repository worktree.

4. **Evidence Gists & Public Attestations**:
   - Only sanitized verification summaries and test evidence required for gate promotions are published to public Gists. Raw runner boot logs or unredacted environment dumps must never be published to Gists.
