# /goal prompt — drive 14 audit beads in parallel (Claude team)

**Audit date:** 2026-06-27
**Worktree:** `/Users/jleechan/.worktrees/dark-factory/audit-2026-06-27`
**Branch:** `feat/prompt-domain-agnostic-audit-2026-06-27` (off `test-merged`)
**Parent branch:** `test-merged`
**Goal owner:** the next Claude team session

---

## Why this goal exists

A read-only audit of the dark-factory repo on 2026-06-27 found **14 prompt/runner bias findings** across 4 parallel subagent passes. The headline result: the supposedly "domain-agnostic" `prompts/slim/*` and `prompts/catalog/*` templates are secretly **world-architect / D&D-shaped** in their example phrasing, and the runner code has **3 HIGH-severity benchmark-shape biases** (Firebase emulator hardcoded, iOS lint pattern global, Gemini-named evidence defaults).

This goal is to drive all 14 beads to land in **10 file-disjoint lanes** in parallel, each as its own PR through dark-factory's own gates.

---

## Beads (full list, priority-ordered)

| Bead | Title | Severity | Lane |
|---|---|---|---|
| `jleechan-9bi` | `prompts/slim/review.md`: D&D world-architect terms in generic reviewer prompt | HIGH (P0) | A |
| `jleechan-cni` | `prompts/slim/{plan,fix}_attractor.md`: level-up session + world_logic.py in generic attractor templates | HIGH (P0) | B |
| `jleechan-3bu` | `prompts/slim/evidence_review.md`: streaming-app evidence schema baked in as universal | HIGH (P0) | C |
| `jleechan-dt9` | `prompts/slim/{spec_review,evidence_review}.md`: dark-factory runner binding not stated | MEDIUM (P2) | C |
| `jleechan-9z0` | `prompts/{slim/fix.md,catalog/stack_smoke.md}`: pytest + "boots and serves" stack assumptions | MEDIUM (P2) | D |
| `jleechan-je5` | `runner/handler_holdout.py`: Firebase emulator hardcoded as universal infra assumption | HIGH (P1) | E |
| `jleechan-fpi` | `runner/pre_review_lint.py`: ios_video_config lint pattern leaks into every reviewer's prompt globally | HIGH (P1) | F |
| `jleechan-rdz` | `runner/handler_audit.py`: Gemini vendor-named evidence filenames as default fallback | HIGH (P1) | G |
| `jleechan-zr3` | `runner`: physically exclude benchmark README/DESIGN/SCORING from implementing agent view | MEDIUM (P2) | H |
| `jleechan-kca` | Harmonize `attractor-spec-review/reviewer.md` head_sha contract with `slim/spec_review.md` | P3 | C |
| `jleechan-vwv` | `amazon-clone spec.md` §Public-Versus-Held-Back leaks 9 sealed-probe categories | MEDIUM (P2) | I |
| `jleechan-hn4` | `airbnb-clone` vs `amazon-clone`: asymmetric sealed-probe disclosure model | MEDIUM (P2) | I |
| `jleechan-jh5` | Document benchmark prompt-shape asymmetry: airbnb-clone (sprint × 3) vs amazon-clone (slice × parallel × fix) | P4 (docs) | J |
| `jleechan-ear` | Consider adding a 4th smoke benchmark beyond hello/fibonacci/roman | P4 (feature) | J |

---

## Lane file-ownership matrix (10 file-disjoint lanes)

This matrix is the **single-writer contract**. Per `~/.claude/CLAUDE.md` "Stacked-PR single-writer rule": a file listed in two lanes forces a halt, converge, then resume — never patched per-branch. **Do not edit files outside your lane.**

| Lane | Beads | Owned files (single writer) | Cross-lane files (must NOT touch) |
|---|---|---|---|
| **A** slim/review.md | `jleechan-9bi` | `prompts/slim/review.md` | none |
| **B** slim/attractor | `jleechan-cni` | `prompts/slim/plan_attractor.md`, `prompts/slim/fix_attractor.md` | none |
| **C** slim/spec+evidence+attractor-spec-review | `jleechan-3bu`, `jleechan-dt9`, `jleechan-kca` | `prompts/slim/evidence_review.md`, `prompts/slim/spec_review.md`, `prompts/slim/spec_review_attractor.md`, `benchmarks/attractor-spec-review/prompts/reviewer.md` | none |
| **D** slim/fix + stack_smoke | `jleechan-9z0` | `prompts/slim/fix.md`, `prompts/catalog/stack_smoke.md` | none |
| **E** runner/handler_holdout | `jleechan-je5` | `runner/handler_holdout.py` | none |
| **F** runner/pre_review_lint | `jleechan-fpi` | `runner/pre_review_lint.py` | none |
| **G** runner/handler_audit | `jleechan-rdz` | `runner/handler_audit.py` | none |
| **H** runner/sealed-path-exclude | `jleechan-zr3` | `runner/handler_codergen.py`, `runner/handler_sandbox.py` | none |
| **I** benchmark spec isolation | `jleechan-vwv`, `jleechan-hn4` | `benchmarks/amazon-clone/spec.md`, `benchmarks/amazon-clone/visible_acceptance.md`, `benchmarks/airbnb-clone/visible_acceptance.md`, `benchmarks/amazon-clone/README.md`, `benchmarks/amazon-clone/SCORING.md` | none |
| **J** docs + 4th smoke benchmark | `jleechan-jh5`, `jleechan-ear` | `docs/pipeline-selection.md` (or new `docs/benchmark-shape.md`), `benchmarks/<new-benchmark>/{README,spec,visible_acceptance}.md` and `prompts/{plan,implement,fix}.md` | none |

---

## Per-lane acceptance criteria

For each lane, the implementation MUST satisfy the acceptance criteria in the bead body (run `br show <bead-id>` for the full text). The short version:

### Lane A (`jleechan-9bi`) — slim/review.md
Replace all D&D world-architect terms in the reviewer prompt:
- "campaign class", "wizard", "Fighter", "single_organic_level_up" → neutral examples (`--role admin` vs `--role guest`, "category-A scenario vs category-B", "expected outcome failed")
- `streaming_evidence.json`, `llm_request_responses.jsonl` → generic placeholders (`<primary-evidence.json>`, `<run-trace.jsonl>`)
- Tests: `tests/test_prompt_contracts.py` should add a world-architect-term assertion that fails if any of `level-up|level_up|world_logic|wizard|Fighter|campaign class|streaming_evidence` reappears.

### Lane B (`jleechan-cni`) — slim/{plan,fix}_attractor.md
Replace "level-up session", "apply-level-up signal", `level_up_signal`, `world_logic.py`, "source=server 2nd writer" with a generic example (e.g., "version upgrade is atomic across two files" or "checkout is atomic across order+inventory+payment"). Keep the structural anti-attractor checklist intact.

### Lane C (`jleechan-3bu`, `jleechan-dt9`, `jleechan-kca`) — slim/spec+evidence + attractor-spec-review
- `evidence_review.md`: replace `streaming_evidence.json`/`llm_request_responses.jsonl` with placeholders; replace "streaming works" → "real-time feature works"; "rendered narrative prose" → "rendered user-visible content"; "native app" → "desktop/mobile/web app".
- `spec_review.md` + `spec_review_attractor.md`: add a one-line "Caller context" header at the top: *"This prompt is invoked by the dark-factory runner only. The `head_sha: <sha>` line and `verdict: pass|fail` contract are part of the runner's parsing protocol; outside the runner they have no meaning."*
- `benchmarks/attractor-spec-review/prompts/reviewer.md`: harmonize the head_sha contract with the slim form. Pick whichever is closer to slim/spec_review.md's 5-step checklist + add the "Caller context" header.
- All three files: deduplicate where the slim/spec_validation.md ≈ hello/spec_validation.md ≈ slim/spec_review.md overlap (LOW-4 finding); prefer slim/spec_validation.md as the canonical, others reference it.

### Lane D (`jleechan-9z0`) — slim/fix + catalog/stack_smoke
- `prompts/slim/fix.md:33-37`: drop "pytest test discovery", replace with "the framework's test discovery" or "the project's test runner".
- `prompts/catalog/stack_smoke.md:1,9`: replace "boots and serves" with "executes and exposes its primary path under real runtime conditions"; add "start the service / invoke the entrypoint / run the CLI" alternatives.

### Lane E (`jleechan-je5`) — runner/handler_holdout
The `_holdout_eval` handler must NOT assume Firebase. Discover the emulator backend from a manifest (Makefile target, package.json scripts, or `dark-factory.yaml` `emulator.kind` field). Gate `JAVA_HOME=/opt/homebrew/opt/java` injection on `sys.platform == "darwin"`. Gate the Google Cloud cred strip on Firebase-detected.
Add a regression test: a fibonacci worktree (no firebase.json) does NOT reach for Firebase or homebrew Java.

### Lane F (`jleechan-fpi`) — runner/pre_review_lint
The `ios_video_config` pattern must NOT fire globally. Either gate by file path (only when the `*.json` is under a path containing `ios`/`simctl`) or move it out of the global `PATTERNS` list into a benchmark-conditional list keyed on the .dot pipeline's `feature=` attribute.
Add a regression test: a fibonacci worktree with a stray `video=1280` in `fibonacci.json` does NOT trigger the warning.

### Lane G (`jleechan-rdz`) — runner/handler_audit
Default evidence filename probe list renamed to vendor-neutral: `llm_request_responses.jsonl`, `llm_responses.jsonl`, `evidence.jsonl` (no `gemini_*` prefix). Backwards-compatible aliases for `gemini_http_request_responses.jsonl` etc. via `.dark-factory/evidence.yaml`.
Add a test: a worktree with `openai_request_responses.jsonl` + `llm_request_responses.jsonl` gets both probed.

### Lane H (`jleechan-zr3`) — runner/sealed-path-exclude
Audit `runner/handler_codergen.py` + `runner/handler_sandbox.py` to confirm the implementing agent's filesystem view excludes `benchmarks/<name>/{README,DESIGN,SCORING}.md`. If exclusion is via `_sanitized_env` + `_holdout_denied_paths`, add the operator-docs pattern. If not enforced, add it via `sandbox-exec` deny patterns.
Add a test: a fibonacci worktree with a stray README.md containing holdout-path strings does NOT leak them.

### Lane I (`jleechan-vwv`, `jleechan-hn4`) — benchmark spec isolation
- `benchmarks/amazon-clone/spec.md:17` and `:2024-2034`: collapse §Public-Versus-Held-Back and §Held-Back-Evaluator-Guidance into the operator-only `README.md` / `SCORING.md`. Replace the public spec.md sections with a one-paragraph pointer.
- `benchmarks/amazon-clone/README.md` + `SCORING.md`: gain the rubric structure pointer.
- `benchmarks/airbnb-clone/visible_acceptance.md:3` and `benchmarks/amazon-clone/visible_acceptance.md`: harmonize to a single disclosure model. Recommend **A** (visible thresholds: amazon's Lighthouse > 70, axe-core < 5 violations style) so the agent has a concrete target.

### Lane J (`jleechan-jh5`, `jleechan-ear`) — docs + 4th smoke benchmark
- Add a 1-paragraph note to `docs/pipeline-selection.md` (or new `docs/benchmark-shape.md`) explaining that benchmark prompt shape is intentional per-benchmark and not a canonical contract.
- Optional: add a 4th smoke benchmark (small CLI wrapping a JSON/YAML/TOML parser, or a tiny stateful TCP/HTTP echo server) under `benchmarks/<new-benchmark>/`. Cover at least one path the existing 3 don't (I/O, persistence, error-handling). Skip if no obvious gap; this is P4.

---

## Pre-flight: file-overlap check (MANDATORY before any agent dispatches)

Per `~/.claude/CLAUDE.md` "Parallel subagents — prefer when tasks are independent":
> Before fanning out agents onto multiple branches/PRs, compute file overlap: `git diff --name-only <base>...<branch>` per lane (or pairwise `git merge-tree --write-tree`). Lanes sharing ANY mutable file are NOT independent.

**For this goal, the file-ownership matrix above IS the pre-flight.** Before any agent edits, verify with:

```bash
git diff --name-only test-merged...feat/prompt-domain-agnostic-audit-2026-06-27
# Should be EMPTY on initial dispatch — agents haven't run yet.
```

After an agent finishes its lane, re-run per-lane:

```bash
git diff --name-only test-merged...feat/prompt-domain-agnostic-audit-2026-06-27 -- prompts/slim/review.md
# Should list ONLY prompts/slim/review.md for Lane A.
```

If a lane discovers it needs to edit a file outside its ownership, **stop the lane, escalate, rebalance** — do not silently edit.

---

## Coordination protocol

1. **Each lane = one Claude team subagent**, dispatched in parallel.
2. **Each agent works directly in this worktree** (`/Users/jleechan/.worktrees/dark-factory/audit-2026-06-27`), on its own commits.
3. **Each agent opens one PR** from `feat/prompt-domain-agnostic-audit-2026-06-27` to `test-merged`, named `<lane-letter>-<short-title>` (e.g., `A-slim-review-world-architect-strip`).
4. **All PRs go through dark-factory's own gates** (`/f-pr <pr-number>`). The agent must verify 7-green before declaring done.
5. **No force-push, no admin merge**. The user types `MERGE APPROVED` (or `merge approved`) per the merge-safety rule.
6. **Test commands per lane**:
   - Lanes A–D, J: no test changes needed beyond `tests/test_prompt_contracts.py` updates. Run the full pytest to confirm no regressions.
   - Lanes E–H: write the regression test described in the lane's acceptance criteria, then run pytest on the new test plus the affected handler test file.
   - Lane I: no test changes; run `bin/conformance validate` and `tests/test_benchmark_boundary.py`.

---

## Stop-the-line conditions

Halt the whole goal and reconvene if:
- Any lane discovers a needed change in a file it does not own.
- Two lanes produce conflicting edits to a shared file (should be impossible by the matrix, but verify).
- A regression test in one lane fails because of an unrelated lane's work (cascade).
- The dark-factory gate `gate_skeptic` returns a verdict other than `pass` for a PR.
- A bead's acceptance criteria are unachievable without scope expansion beyond the lane's owned files.

---

## Post-completion

After all 10 lanes are merged:
1. Run `br list --status closed --label audit-2026-06-27` to confirm all 14 beads closed.
2. Run `/learn` to capture durable lessons from the parallel dispatch (what worked, what didn't).
3. Update this file's "Status" section with merge SHAs.

---

## Reference (do not edit)

- Subagent transcripts: `/private/tmp/claude-501/-Users-jleechan-projects-dark-factory/<session>/tasks/*.output` (read-only, will overflow context if read whole).
- Audit parent session: this directory's `MEMORY.md` and `CLAUDE.md`.
- Beads: `br show <id>` for full body.

**Status:** INITIAL — no lanes dispatched yet.
