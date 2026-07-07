# Developer Repro Handoffs

This folder is the readable index for developer reproduction packages. Large replay bundles live
under `artifacts/repro-developer/` and are transported with Git LFS; this docs folder explains what
each bundle is proving and how an engineer should replay it.

## Incidents

- `claude-fable-adversarial-review-codex-plan-miss.md` — July 6-7, 2026 reproduction package for
  the case where Claude/fable ran a 53-agent adversarial review that missed plan-level hazards
  later caught by Codex with parallel subagents.

## Storage Policy

- Human-readable findings and replay instructions belong here.
- Sanitized replay archives and encrypted raw archives belong in `artifacts/repro-developer/`.
- Raw plaintext archives and passphrase files must not be committed.
- The raw archive passphrase is shared out of band only with the intended engineer.
