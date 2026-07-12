# Publishability Gate (rule 11, mandatory final stage)

Run AFTER all writer lanes complete, BEFORE PR is called done.

## Gate 1 — Redaction sweep
```bash
rg 'file:///Users|/Users/[a-z]+|ghp_|gho_|x-access-token|serviceAccountKey' \
   docs/plans/swarm-pr228-code-quality/
```
Expected: zero hits. Replace any machine paths or token forms with generic placeholders.

## Gate 2 — Cross-doc consistency
Every numeric claim in the synthesis doc must be diffed against its per-finding source doc.
```bash
# Find all numeric claims
rg -o '[0-9]+ insertions?|[0-9]+ deletions?|line [0-9]+|F[0-9]+' docs/plans/swarm-pr228-code-quality/
```
Disagreements block publish.

## Gate 3 — Freshness re-baseline
Every present-tense claim ("X currently does Y", "X is still broken") must be re-checked against the pr228 branch head SHA (`658a715`). Historical evidence must be explicitly marked `at base <sha>` or `at commit 2a383a1`.

## Gate 4 — Supersession markers
Each finding has exactly one authoritative doc. Earlier superseded drafts carry a banner pointing to the canonical doc.

## Gate 5 — Policy lens
Findings recommendations checked against repo-specific hard law:
- **ZFC** (no keyword/regex/heuristic routing in application code): `_check_stale_artifacts` is a borderline case — confirm flagged.
- **Spec-intent confirmation** (CLAUDE.md global): `config/daemon.toml` adds the `[repos]` table — verify the routing decision was operator-confirmed (search for explicit confirmation in commit history or PR thread). The af-e2e STATE.md mentions this was operator-approved as part of the multi-repo dispatch work.
- **Harness-fix durability**: pre-push hook regression — the fix should be a CI gate (workflow check), not just a script that runs once.

## Gate 6 — Recipe validity
Every copyable command/acceptance-check states the CORRECT expected outcome.
- Negative tests assert red, not green.
- "verify X is broken" steps must show the breaking input, not the passing output.

## Gate 7 — Mechanical hygiene
```bash
git diff --check
```
Must be clean. No whitespace errors, no conflict markers.

## Gate 8 — Confirm no origin/main drift mis-attribution
Final check: search for any claim that uses `git diff origin/main..HEAD` numbers as pr228's contribution. Cite only the actual 2-commit diff (469/+7218/- is DRIFT, not contribution).

## Gate output
For each gate: PASS / FAIL / N/A. FAILs block publish.