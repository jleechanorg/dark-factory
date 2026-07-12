---
name: auto-factory
description: Guide for running and monitoring the auto-factory (Level 5 DOT pipeline runner). Instructs on how to make the factory adopt PRs via the 'factory' label and monitor its background daemon and tmux remediation panes.
---

# Auto-Factory (/af) Guide

The `dark-factory` auto-factory is a Level 5 DOT pipeline runner that automatically drives pull requests to green, runs safety gates, and prepares them for merge. 

## How to Trigger the Auto-Factory

To make the auto-factory pick up and process a task:

1. **For existing Pull Requests (PRs)**:
   Add the `factory` label to the GitHub PR. The background daemon will automatically intake and adopt it.
   
2. **For new tasks (larger features/bugs)**:
   Create or update a GitHub issue in the target repository with the `factory` label.
   
3. **For smaller tasks**:
   Create a local bead with the `factory` label:
   ```bash
   br create "My task title" --body "My task description" --label factory
   ```

---

## How to Monitor the Auto-Factory

Once a task or PR is labeled, the background daemon handles the state transitions and remediation autonomously. You do not need to run commands; instead, monitor the progress:

### 1. Check Daemon Status
Check if the daemon is running and check its current tick:
```bash
systemctl --user status ai.dark-factory.daemon.service
```

### 2. Tail Daemon Telemetry Logs
Filter and monitor real-time lifecycle transitions (excluding verification loop spam):
```bash
tail -f ~/Library/Logs/dark-factory/daemon.jsonl | grep -v VERIFICATION_PENDING
```

### 3. Track Remediation Workers
If verification fails, the daemon automatically dispatches remediation workers (AO sessions) in background tmux panes. 
* List active sessions:
  ```bash
  tmux list-sessions
  ```
* Inspect a worker's console output in real-time:
  ```bash
  tmux capture-pane -t <session_name> -p
  ```

---

## Key States
* **`ATTESTED`**: The PR/bead has been adopted and is undergoing safety gate checks.
* **`READY`**: The PR passed all gates and is green (ready for merge approval).
* **`DISPATCHED`**: The PR failed verification and an autonomous worker has been spawned in a tmux session to fix it.
* **`HUMAN_HELD`**: Parked for human intervention (e.g. branch conflicts or recovery limits reached).

