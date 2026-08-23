# Dark Factory Code & Comment Standards

This document defines code, comment, and operational standards for the `dark-factory` repository, capturing lessons learned from end-to-end operational lanes (e.g., PR #665 Lane E, PR #666 Lane F, PR #670).

---

## 1. Anchor Comment Syntax Standards

Anchor comments link code lines, modules, tests, or bug fixes back to their corresponding beads, PRs, or tracking issues (e.g. `// PR #665`, `// bead jleechan-qzr3`).

### Core Rule
**Anchor comments must use the target language's native comment syntax:**
- **Rust (`*.rs`)**: MUST use `//` (or `/* ... */`). **NEVER use `#`**. In Rust, `#` is reserved for inner/outer attributes (`#[...]` / `#![...]`); a line starting with `# PR #N` causes compile-time syntax errors in `rustc`.
- **Python (`*.py`)**: MUST use `#`. Never use `//` (which is integer division).
- **Shell (`*.sh`, `*.bash`)**: MUST use `#`.
- **YAML (`*.yml`, `*.yaml`)**: MUST use `#`.
- **TOML (`*.toml`)**: MUST use `#`.
- **JavaScript / TypeScript / C / Go (`*.js`, `*.ts`, `*.go`, `*.c`)**: MUST use `//` or `/* ... */`.

### Automated Enforcement
The pre-push hook `.githooks/pre-push-anchor-comment-guard.sh` (invoking `scripts/check_anchor_comments.py`) scans staged and pushed diffs to prevent invalid comment syntax from entering the repository.

---

## 2. Lane E/F Remediation Candidates Triage

Following the operational recovery of PR #665 (mergeable:null fix, Lane E) and PR #666 (fixture-leak fix, Lane F) merged via `--admin` during a self-hosted runner outage, the E2E remediation items are triaged as follows:

| Candidate | Description | Triage Decision | Remediation / Status |
|---|---|---|---|
| **A) Early Runner Outage Surfacing** | Avoid future `--admin` merges by surfacing runner outages earlier rather than waiting 30+ minutes on `UNSTABLE` `mergeStateStatus`. | **FIX** | Implemented `scripts/check_runner_outage.sh` and `scripts/check_runner_outage.py` to query runner status and post `"RUNNER OUTAGE — consider --admin or wait"` warning if 0 runners are online. |
| **B) Anchor-Comment Syntax Enforcement** | Anchor comments default to `//` in Rust instead of `#` to prevent Rust syntax errors. | **FIX** | Documented standard above and added `.githooks/pre-push-anchor-comment-guard.sh` + `scripts/check_anchor_comments.py`. |
| **C) Legacy Bead `external_ref` Convention** | Legacy follow-up beads (e.g., `jleechan-7re5`, `jleechan-2xlo`) missing `target_repo` / `external_ref` conventions. | **ACCEPT-AS-DEGRADED** | Acknowledged as long-tail cleanup; to be systematically resolved in upcoming skillify passes. |
| **D) Commit Message Evidence Gate Scanning** | Evidence Gate workflow only scanned PR body; interim evidence in commit messages was ignored. | **FIX** | Updated `.github/workflows/evidence-gate.yml` Signal B to scan PR commit messages (`git log`) for canonical `**Evidence**:` markers when absent in PR body. |

---

## 3. Pre-Merge Runner Smoke Check

Before attempting manual `--admin` merges or waiting on queued CI runs:
```bash
# Run runner outage smoke check
scripts/check_runner_outage.sh jleechanorg/dark-factory

# Or with Python / JSON
python3 scripts/check_runner_outage.py --repo jleechanorg/dark-factory --json
```

If 0 online runners are reported (`exit code 3` / `FLEET_DOWN`), operators are alerted immediately to restore runner infrastructure or evaluate emergency `--admin` merge policies.
