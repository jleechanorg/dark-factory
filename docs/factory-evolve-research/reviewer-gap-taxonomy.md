# Reviewer Gap Taxonomy — why in-pipeline reviewers catch less than a codex cold review

Research for a future `/factory-evolve` skill. Compares the factory's wired
reviewer/gate nodes against what an unconstrained `codex` cold review does:
whole-diff, cross-file, adversarial, no scope blinders, a different vendor than
the coder, pinned to the current head SHA.

A codex cold review's edge comes from four properties simultaneously: (a) it
ingests the **entire diff** the human points it at, (b) it reasons **cross-file**
without a hunk boundary, (c) it is **adversarial / different-vendor**, and
(d) its findings are **read in full** by a human, not collapsed to one token.
Each gap category below is a place where one of those four properties is
structurally absent from the factory's reviewer nodes.

Provenance: `runner/handler_verdict.py`, `runner/handler_dispatch.py`,
`runner/handler_universal_prompts.py`, `runner/handler_special_gates.py`,
`runner/handler_holdout.py`, all `pipelines/**/*.dot`, `prompts/slim/review.md`,
`prompts/catalog/review.md`, `prompts/slim/evidence_review.md`.

---

## G1 — reviewer-not-wired-in-graph (whole pipelines with no independent reviewer)

The strongest gap: several graphs that "validate" have **zero** independent
reviewer node — only deterministic gates or nothing at all. A codex cold review
would have looked at the diff; these graphs never ask anyone to.

- `pipelines/factory/gates.dot` and `pipelines/factory/pr_gates.dot` — labeled
  "Full validation" / "PR validation" but contain **no codergen cold-review
  node**. They run `holdout_eval` + `gate_es` + `gate_er` + `gate_code_standards`
  only. There is no fresh-eyes whole-diff reviewer; the templated gates (see G3)
  are the entire review surface.
- `pipelines/slim/minimal_research.dot`, `pipelines/parallel_demo.dot`,
  `pipelines/factory/hello.dot` — no reviewer or evidence gate at all
  (hello is a smoke graph, but minimal_research ships research output unreviewed).
- **Fix idea:** `/factory-evolve` should flag any graph reaching `exit` on a
  code-producing path without at least one `type=codergen class=review` OR
  `type=gate_er/gate_slash` node that is on the *blocking* path (see G2).

## G2 — failed-review-routes-to-exit (verdict recorded but does not block merge)

Even where a reviewer is wired, a FAIL verdict frequently routes to `exit`
instead of a `fix` loop, so the run terminates "done" with a negative review on
record. A codex cold review's findings are read and acted on; here they are
filed and bypassed.

- `pipelines/factory/gates.dot` / `pr_gates.dot` — every gate's failure edge is
  `gate_x -> exit [condition="outcome!=success"]`. A failing `/es`, `/er`, or
  `/code_standards` does not loop back to fix; it just ends the run.
- `pipelines/slim/review_pr.dot` — `review -> exit [condition="outcome!=success"]`
  routes a failed cold review straight to exit, and `evidence -> exit`
  unconditionally (the gate_er verdict cannot block).
- `pipelines/bug_fix.dot` — `evidence -> exit` unconditionally; relies on the
  exit handler to "propagate" a non-success outcome rather than re-entering fix.
- **Fix idea:** distinguish "verdict recorded" from "verdict gating." Evolve
  should require code-producing graphs to route reviewer `outcome!=success` to a
  bounded `fix` loop, not to `exit`; reserve `->exit` for explicitly
  non-blocking advisory nodes.

### G1+G2 status

**Status (2026-06-21): shipped as `runner/graph_audit.py` + CI step. The 6 red graphs (`bug_fix.dot`, `factory/gates.dot`, `factory/pr_gates.dot`, `slim/minimal_research.dot`, `slim/redgreen_claudeaf.dot`, `slim/review_pr.dot`) were rewired to add bounded fix-loops on reviewer failure. CI now blocks any new G1/G2 violator in PRs touching `pipelines/`.**

## G3 — templated/weak reviewer prompt (checklist, not open-ended adversary)

The named gates send a fixed checklist prompt, not an open "find anything wrong"
brief. `UNIVERSAL_CODE_STANDARDS_PROMPT` / `UNIVERSAL_EVIDENCE_REVIEW_PROMPT`
(`handler_universal_prompts.py:111,146`) enumerate 3-4 fixed axes (ZFC, RCF,
clean-code; provenance, metrics, invariants, checksums). A bug outside those
axes is out of scope by construction — the opposite of a cold review's open
mandate. `prompts/slim/evidence_review.md` is similarly evidence-bundle-only.

- **Fix idea:** add an explicit "list any correctness/security/architecture
  issue regardless of the checklist above" clause, and an open-ended
  whole-diff reviewer lane separate from the evidence/standards checklists.

## G4 — no-diff-injection (reviewer must self-fetch the diff; coder-side blinders)

The runner **never computes or injects a `git diff`** into any reviewer prompt.
Gates say "analyze the active repository changes and diff in the current
workspace" and rely on the reviewer agent to run git itself; the codergen
`review` node (`type=codergen`) only receives the rendered template
(`${goal}` + a "use spec.md" instruction) — no diff, no base SHA, no file list.
A codex cold review is *handed* the whole diff; here the reviewer may review the
wrong scope, a stale tree, or nothing if it doesn't think to diff.

- Exhibited by `prompts/slim/review.md`, `prompts/catalog/review.md`, and both
  universal prompts — all describe the diff in prose; none embed it.
- **Fix idea:** have the gate/codergen review handler compute
  `git diff <base_sha>..HEAD` (base already resolved by `_resolve_base_sha`,
  `handler_special_gates.py:43`) and inline it into the prompt, the way a human
  pastes a diff into codex.

### G4 status

**Status (2026-06-21): shipped as `_codergen` post-step diff capture + `${diff}` template substitution in `_render_prompt`. `_codergen` now stashes `git diff` + `git diff --staged` (truncated to 50k chars) in `ctx.state['<node>.diff']` and `ctx.state['_last_diff']` after a successful run. Reviewer prompts can reference `${diff}` to see the implementing agent's real changes. Captured for all backends (echo, mock_llm, claude, codex, agy, ao) and best-effort (silent no-op on git failures).**

## G5 — scope-limited-to-diff-hunk (off-diff contradictions only nominally covered)

`prompts/slim/review.md` step 2 asks for an "off-diff contradiction check," but
it is one bullet inside a diff-scoped checklist and depends on the reviewer
voluntarily searching unchanged files — with no diff or file list provided
(G4), the reviewer has no anchor for "related files that were NOT changed." A
codex cold review reasons across the whole tree by default.

- **Fix idea:** provide the changed-file list explicitly and require the
  reviewer to name, for each changed symbol, the unchanged consumers it checked.

## G6 — verdict-parsing-swallows-nuance (pass|warn collapse, last-token-wins)

`handler_verdict.py:_VERDICT_NORMALIZE` maps `warn -> success` and
`conditional`/`partial` handling is binary; `_parse_verdict` reduces the entire
review to one token and keeps only the **last** marker. A codex cold review's
value is the prose findings; the pipeline keeps the pass/fail bit and the prose
only survives in CXDB `output_head`. A "warn" with three real blockers passes.

- `handler_verdict.py:28-43` (`warn -> success`); `:135-138` (last-marker-wins).
- **Fix idea:** treat `warn`/`conditional`/`partial` as blocking-by-default for
  code-producing graphs, and surface finding count, not just the verdict token,
  into the gating condition.

## G7 — single-vendor-collapse (priority queue can fall back to the coder's vendor)

The adversarial queue (`_DEFAULT_ADVERSARIAL_PRIORITY = codex > minimax > agy >
claude-sonnet`) is real, but cross-vendor is only guaranteed when
`prefer_adversarial=true` is set on the node. The **named gates**
(`gate_es`/`gate_er`/`gate_code_standards` in gates.dot/pr_gates.dot) do **not**
set `prefer_adversarial`, so on a claude run the reviewer can resolve to
claude-sonnet — same vendor as the coder. `_execute_gate` also falls back to
`claude` on any infra failure regardless of original vendor.

- `handler_dispatch.py:_resolve_gate_backend:361` (filter only when
  prefer_adversarial); `_execute_gate:423-425` (claude fallback).
- **Fix idea:** make `prefer_adversarial` default-on for any gate, and record a
  hard `same_vendor_as_coder` flag in metadata that evolve can alarm on.

## G8 — SHA-binding gives false confidence but is not freshness (race window)

`_verify_head_sha_echo` binds the verdict to the HEAD SHA at gate-entry, which is
good — but graphs route gate->gate->exit with no re-check, so a concurrent commit
between `gate_es` entry and `exit` is invisible. The binding proves "graded
*some* HEAD," not "graded the *final* merged HEAD." A human running codex
re-reviews after the last push; the pipeline does not.

- `handler_verdict.py:66-77`; gates.dot linear chain with no terminal re-pin.
- **Fix idea:** re-verify HEAD SHA at the exit node and fail if it moved since
  the last reviewer entry.

## G9 — unit-only / templated evidence accepted (gate_er trusts the bundle's own claims)

`gate_er` / `evidence_review.md` audit an evidence *bundle* (pass rates, SHA,
artifact files) but do not independently re-run anything; a bundle asserting
"3/3 pass" is trusted if the files exist and the SHA matches. Holdout coverage
(`holdout_eval`) is the only behavioral re-execution, and graphs without holdout
(`bugfix_noholdout.dot`, `review_pr.dot`) have no independent behavioral check at
all. A codex cold review reads the actual test code and spots a mock-only proof.

- `prompts/slim/evidence_review.md`; `handler_holdout.py` is the only re-exec path.
- **Fix idea:** require gate_er to re-run at least one cited scenario, or flag
  "evidence accepted without independent execution" when no holdout node exists.

## G10 — visual-evidence-not-inspected (frames captured but pixels never read)

A pipeline captures visual artifacts (screenshots, video, frame extractions) but
no gate reads the pixel content. Evidence review trusts file-level metadata
(count, byte size, codec, event counts) as a proxy for visual correctness. The
transport layer is verified but the presentation layer is not.

**Incident (2026-06-25, worldai_claw PR #250):** iOS evidence capture produced 52
PNG frames and a 51.8s video showing SSE streaming on native iOS. The captions
track and JSONL both confirmed 30 SSE chunks arrived. Every gate — E2E tests,
CodeRabbit, `/er` audit, holdout (never executed) — passed. A manual frame
inspection found three visible UX bugs present in **every single frame**:

1. "No connection to server" error banner persisted throughout active SSE streaming
2. "Open in WorldAI Claw?" native dialog was never dismissed
3. Raw JSON tokens (`}`, `[SESSION_HEADER]`, escaped `\\n`) rendered as narrative

The pipeline verified the wire (30 chunks arrived) but not the screen (what the
user sees). The evidence-review prompt (`evidence_review.md`) checked SHA match,
pass rates, and artifact existence — it never told the LLM reviewer to `view_file`
on the PNGs. The `/es` skill (user-scope) at line 209 already says "extract frames
and look" — but this instruction was not wired into the factory's reviewer prompt.

- `prompts/slim/evidence_review.md` (fixed: step 4 now mandates visual cross-check)
- `prompts/slim/review.md` (fixed: step 3 evidence check now includes visual cross-check)
- **Detection:** look for evidence bundles containing `.png`/`.mp4` files where the
  only assertions are on metadata (file count, `ffprobe` output, `wc -l` on JSONL)
  rather than on content (OCR text, pixel state, element visibility).

---

## Cross-cutting note for /factory-evolve

The single highest-leverage evolve check is **G1+G2 combined**: scan every
`.dot`, find code-producing paths to `exit`, and assert each one passes through
a reviewer whose `outcome!=success` edge loops to `fix` (not `exit`). G4 (inject
the real diff) is the highest-leverage *quality* fix once the wiring exists —
without the diff, even a correctly-wired reviewer is reviewing blind.
