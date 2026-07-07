# setup-agent-hooks.sh — Parallel Adversarial Review — 2026-07-06

> Three independent review agents ran in parallel against `scripts/setup-agent-hooks.sh` (298 lines)
> and the accompanying `.gitignore` change (`.codex/`, `.cursor/`, `.gemini/`, `.opencode.json`):
> a static shell-correctness reviewer, a sandbox claim-verifier (re-ran every claimed behavior in a
> throwaway git repo), and a per-CLI schema reviewer (checked generated configs against official
> Codex/Cursor/Gemini/OpenCode docs + deployed local configs as ground truth).

## Consolidated verdict

**DO NOT ship as-is. Mechanically excellent, semantically broken.**

The installer does everything its report claims (all 11 runtime claims independently verified PASS),
and all 4 claimed bug fixes are genuinely present in the code. But the four per-CLI templates are
**rotated one CLI off** — three of the four hooks silently never fire, and only the Codex config is
correct. The original report's "Bonus" section claimed the install run *repaired* cross-contaminated
files; in reality the templates were derived from the contaminated files, so the installer
**canonicalized the rotation** and `--check` now certifies the broken state as `[ok]`.

## The rotation (schema reviewer)

| File | Contains | Should contain | Result |
|---|---|---|---|
| `.codex/hooks.json` | `hooks.PreToolUse` (Claude-Code-style shape) | ✓ correct — Codex ≥0.124 auto-reads repo-level `.codex/hooks.json` | **WORKS** |
| `.cursor/hooks.json` | Gemini's `BeforeTool` event, nested matcher shape | Cursor's `preToolUse` (camelCase) + required top-level `"version": 1` | **silently never fires** |
| `.gemini/settings.json` | OpenCode's `$schema: https://opencode.ai/config.json` + `instructions` | Gemini's `hooks.BeforeTool` block (`timeout` in **milliseconds**) | **no hook registered at all** |
| `.opencode.json` | Cursor's `preToolUse`/`subagentStart`/`version:1` shape | `$schema: opencode.ai/config.json` + `instructions` (OpenCode has **no JSON hooks** — real pre-tool hooks require a `.opencode/plugin/*.ts` with `tool.execute.before`) | **silently never fires** |

Ground truth that proves the rotation: the *deployed* `~/.cursor/hooks.json` and
`~/.gemini/settings.json` on this machine already carry the correct schemas; the repo templates
simply hold each other's content.

**Bug "fix" #4 was backwards**: restoring `"$schema": "https://opencode.ai/config.json"` into the
*Gemini* template (script line 156) preserved the contamination — that URL belongs only in
`.opencode.json`, which is simultaneously missing it.

### Correct forms (from official docs)

```jsonc
// .cursor/hooks.json
{"version": 1, "hooks": {"preToolUse": [{"command": "bash ~/.local/bin/conflict-warn-pre-tool.sh"}]}}

// .gemini/settings.json
{"hooks": {"BeforeTool": [{"matcher": ".*", "hooks": [{"type": "command",
  "command": "bash ~/.local/bin/conflict-warn-pre-tool.sh", "timeout": 15000}]}]}}   // ms

// .opencode.json  (JSON path is instruction-injection only)
{"$schema": "https://opencode.ai/config.json", "instructions": "…predict-conflicts --from-prs…"}
// (functional hook requires .opencode/plugin/*.ts with tool.execute.before)

// .codex/hooks.json — keep as-is; optionally rename timeoutSec → timeout (seconds)
```

Sources: cursor.com/docs/hooks, developers.openai.com/codex/hooks,
geminicli.com/docs/hooks/reference/, opencode.ai/docs/config/, opencode.ai/docs/plugins/.

## Static review findings (shellcheck + bash -n clean; edge cases executed, not just read)

| Sev | Location | Defect | Failure scenario |
|---|---|---|---|
| **MAJOR** | lines 116/138/168 | `HOOK_PATH` interpolated raw into JSON heredocs, unescaped/unquoted | Path with `"` → invalid JSON (CLI can't parse its own config); path with spaces → `bash /my dir/hook.sh` splits into 2 args. Fix: JSON-encode via `python3 json.dumps` + shell-quote inside the command string |
| minor | line 156 | Gemini file emits OpenCode's schema URL | See rotation above |
| minor | line 66 | `--hook-path` as last arg → `$2: unbound variable` under `set -u` (crash, exit 1) | `--only` guards `[ $# -ge 2 ]` at line 68; `--hook-path` doesn't |
| minor | lines 68/70/76 | Empty `--only=` silently installs **all four** CLIs | Empty `ONLY` skips the filter — opposite of an empty selection. Reject empty like an unknown name |
| minor | line 239 | Install rewrites files even when content unchanged (mtime churn) | Content-idempotent but not FS-idempotent; dry-run path skips correctly, real path doesn't. Guard the write with a content compare |
| nit | lines 220/248 | `rc` in `run_install` is dead code (never set non-zero) | Relies entirely on `set -e` |
| nit | line 285 | `.opencode.json`'s parent is REPO_ROOT; empty-dir guard saves it, but fragile | Would attempt `rmdir` of repo root if ever empty |
| nit | lines 193-200 | `sentinel_for` has no default case | Unknown name → empty sentinel → `grep -qF ""` matches everything → false `[ok]` |

Verified sound: uninstall preserves parent dirs containing unrelated files (`ls -A` guard, line 286);
git-repo refusal fires before any FS write; `--check` exit semantics 0/1 correct; all `[ cond ] &&
action` idioms are set-e-safe; the four claimed bug fixes (#1 no arithmetic-under-set-e, #2 distinct
per-CLI sentinels, #3 ACTION/DRY_RUN split, #4 gemini $schema escaping) are all present in code.

## Claim verification (sandbox, all 11 PASS)

1. Fresh install writes all 4, `jq -e .` valid — PASS
2. Re-run idempotent, stable hashes — PASS
3. `--check` 0 clean / 1 on modify AND flags BOTH files of a cursor↔gemini swap — PASS
4. `--dry-run` performs zero I/O — PASS
5. `--uninstall --dry-run` neither installs nor removes (bug #3 genuinely fixed) — PASS
6. Uninstall removes 4 + empty parents, preserves parent with unrelated file — PASS
7. Round-trip uninstall→install → identical sha256 — PASS
8. Missing hook script: no `--force` → exit 1, zero writes; `--force` → proceeds — PASS
9. Refuses outside git repo (exit 2) — PASS
10. `--only codex,cursor` writes exactly those two — PASS
11. Real repo: `--check` exit 0; all 4 paths gitignored (`.gitignore:61-64`) — PASS

**Additional limitation found (outside the claims' scope):** `--check` is sentinel-substring-only —
a file overwritten with *invalid JSON containing `"command": "rm -rf ~"`* still passes `--check`
as long as the event-name string survives. `--check` validates file-matches-template, **not**
template-matches-reality and not JSON validity — which is exactly why the rotation went undetected:
each wrong template greps for its own wrong sentinel. A gate that cannot fail is not a gate.

## Required fixes, in order

1. **Re-rotate the three templates** (cursor/gemini/opencode) to the correct schemas above; use the
   deployed `~/.cursor/hooks.json` / `~/.gemini/settings.json` as ground truth.
2. **Escape/quote `HOOK_PATH`** in JSON generation (the MAJOR).
3. **Strengthen `--check`**: `jq -e .` each file + grep for the *documented* per-CLI event name
   (independent of the template), so a wrong template cannot self-certify.
4. Minor arg-parsing fixes (`--hook-path` unbound `$2`, empty `--only`), skip write when unchanged,
   optionally `timeoutSec` → `timeout` in the Codex template.

`.gitignore` change is correct as-is. Sandbox artifacts left at the session scratchpad
(`hooks-sandbox/`) for re-inspection.
