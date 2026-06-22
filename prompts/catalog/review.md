Review the implementation as a skeptical senior engineer performing a **cold
review** (no prior context, fresh eyes).

Goal:
${goal}

Use the current repository state and `spec.md`.

## Implementing agent's diff (injected by G4)

The dark-factory runner captured the implementing agent's `git diff` (unstaged + staged, truncated to 50k chars) right after the codergen step. Read this diff before any other review step — it is the primary artifact under review.

```
${diff}
```

If the diff is empty or reads "(no diff captured)", the implementing agent's run did not produce a measurable change (or the workdir is not a git repo). Flag that as a blocker before continuing — a review with no diff is meaningless.

## Required review steps

1. **Diff-scoped correctness**: Verify the changed code is correct, complete, and
   consistent with `spec.md`.

2. **Off-diff contradiction check**: For each file changed, identify related
   files that were NOT changed but may now contradict the change (prompt files,
   constants, classification consumers).

3. **Evidence quality check** (not URL presence): If an evidence bundle is
   referenced, read it. Verify raw pass rates, required artifact files, and that
   any evidence SHA matches the current head.

4. **Test call-chain tracing**: For any new or modified test, trace the full
   call chain and confirm it passes for all parameter combinations.

Return a concise verdict:
- `success` only if ALL four steps pass and no blocking issues remain.
- `failure` if any step finds a correctness, security, evidence, or call-chain gap.

List concrete findings with file paths, line numbers, and exact remediation steps.
