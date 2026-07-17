# PR #281 review — Lane A: production code (/thermo + /code-standards)

Reviewed at branch `pr281-review`, head `e70b1ec6` (diff `origin/main...pr281-review`).

## Scope

- `runner/skeptic_gate.py` (1000 lines) — pure verdict-parsing/SHA-binding/comment-formatting logic.
- `runner/skeptic_gate_cli.py` (1119 lines) — orchestrator: `gh` API calls, reviewer subprocess invocation, publish/read-back.
- `.github/workflows/skeptic-gate.yml` (327 lines) — `workflow_call`/`workflow_dispatch` target, binary pinning, checkout-at-pinned-SHA.

Files read in full (not excerpted). Cross-referenced against `runner/handler_verdict.py`, `runner/handler_sandbox.py`, `runner/handler_parallel_reviewer.py`, `runner/_git.py` for canonical-layer/reuse questions (those files are NOT in scope for findings, only used as comparison).

## /thermo findings

**F1 — BLOCKER — `_publish_failure` never posts to the right PR (silently broken feature)**
`runner/skeptic_gate_cli.py:1039-1054` (`_publish_failure`) and `:1057-1065` (`_pr_number_for_desc`).

```python
def _publish_failure(repo, head_sha, body, context, description) -> None:
    ...
    try:
        post_or_update_comment(repo, _pr_number_for_desc(description), body)
    except Exception:
        pass

def _pr_number_for_desc(description: str) -> int:
    m = re.search(r"PR #(\d+)", description)
    if m:
        return int(m.group(1))
    return 0
```

`_publish_failure` has no `pr_number` parameter — it recovers the PR number by regex-matching the literal string `"PR #<N>"` out of `description`. Its two call sites (`main()` lines ~654-661 and ~682-689, the diff-capture-failed and diff-too-large early-exit paths) pass `description=f"diff capture failed: {str(exc)[:80]}"` — a string that **never** contains `"PR #<N>"`. So `_pr_number_for_desc` always returns `0`, and every diff-capture-failure path attempts to post the diagnostic comment to issue `#0` in the repo. That call will fail (or, worse, silently no-op against whatever issue #0 resolves to), and the surrounding `except Exception: pass` swallows it. Net effect: the commit status still correctly flips to `failure` (so merge protection isn't bypassed), but the promised "explain why the diff couldn't be reviewed" PR comment silently never appears for these two paths. `args.pr_number` is available in `main()`'s scope and is never threaded through — the fix is to add a `pr_number: int` parameter to `_publish_failure` and drop `_pr_number_for_desc` entirely (it has zero real callers producing a matching string and zero test coverage — confirmed via `grep -n "_publish_failure\|_pr_number_for_desc" tests/test_skeptic_gate.py` → no hits).

**F2 — STRONG — redundant SHA regex pair, self-acknowledged as vestigial**
`runner/skeptic_gate.py:185-190` (`_SHA_RE` 7-64 hex vs `_FULL_SHA_RE` exactly-40-hex) and `:280-333` (`parse_verdict` body).

`_FULL_SHA_RE` requires exactly 40 hex chars; `_SHA_RE` accepts 7-64 hex chars — a strict superset of the 40-char case. Both are run with `findall` against the same single `HEAD_SHA:` line, so `shas` and `short_shas` are always identical when the field is well-formed. The code even says so in a comment at line 330-332: *"`short_shas` is `shas` here (same regex anchored), so this list has length 1; keep the dedupe explicit to guard against future drift."* That's speculative generality kept alive by a comment rather than removed — two regexes, two count checks (`len(shas) != 1 or len(short_shas) != 1`), and a dedupe (`len(set(short_shas)) != 1`) all doing the same job `_FULL_SHA_RE` alone already does. Code judo: delete `_SHA_RE`/`short_shas` and keep only `_FULL_SHA_RE`; no behavior changes, ~10 fewer lines, one less regex to reason about when auditing the anti-injection contract.

**F3 — STRONG — sequential reviewer loop leaves no margin inside the job's own timeout**
`runner/skeptic_gate_cli.py:714-749` (the `for reviewer_name, model in reviewers:` loop, each call using `invoke_reviewer(..., timeout=900)` at default) vs `.github/workflows/skeptic-gate.yml:119` (`timeout-minutes: 30`).

Reviewers run one after another, not concurrently, despite the module docstring/comments repeatedly emphasizing "multi-reviewer independence." With the default 2-reviewer list (`codex`, `gemini`) and the default 900s per-reviewer timeout, a single slow-but-legitimate reviewer call can consume 15 minutes; two slow calls consume the entire 30-minute job budget before checkout, binary verification, diff fetch, comment/status publish, and read-back even get their share. This isn't a correctness bug (a timeout still fails closed), but it's an availability risk baked into the design: any two normal (not even pathological) reviewer runs near their timeout will spuriously fail the gate for a well-formed PR. Recommend either running reviewers concurrently (they're already independent, sandboxed subprocesses — `concurrent.futures.ThreadPoolExecutor` or two `subprocess.Popen` + join is a small change) or lowering `timeout` well below `(30 min / num_reviewers)` with a code comment tying the two constants together so they can't silently drift apart again.

**F4 — NIT — fragile list-mutation-by-value-equality**
`runner/skeptic_gate_cli.py:751-820` (the per-reviewer CLI→identity + provenance binding loop).

```python
for r in per_reviewer:
    ...
    per_reviewer[per_reviewer.index(r)] = r2
```

`SkepticResult` is `@dataclass(frozen=True)`, so `list.index(r)` searches by field-value equality, not object identity. In every currently-reachable code path the replacement objects differ by their embedded `reviewer` field (so today this doesn't misfire), but that's incidental, not structural — a future edit that constructs two reviewers' failure results with identical `reason`/`comment_body` text would silently overwrite the wrong entry. Prefer `for i, r in enumerate(per_reviewer):` and assign `per_reviewer[i] = r2` directly; removes the dependency on the dataclass's auto-generated `__eq__` for a purpose it wasn't designed for.

**F5 — NIT — both files sit at/over the "1k-line rule" boundary**
`runner/skeptic_gate.py` is exactly 1000 lines; `runner/skeptic_gate_cli.py` is 1119. Neither is unreasonable given how much of the bulk is docstring/comment (the module-level docstrings and the extensive "per post-audit comment NNNN" inline commentary account for a large fraction of both files — a rough line-type split would clarify how much is real logic vs. narrative). Not a blocker on its own, but combined with F1-F3 above, a split is worth considering: `skeptic_gate.py` could separate "parsing/binding" (parse_verdict, bind_to_pr, verify_provenance, bind_reviewer_identity) from "comment/prompt rendering" (format_comment, build_prompt) into two modules; `skeptic_gate_cli.py` could separate "gh API I/O" (gh_api, find_existing_bot_comment, post_or_update_comment, set_commit_status, read_back_comment) from "reviewer subprocess invocation" (_reviewer_env, _build_reviewer_cmd, _extract_codex_message, invoke_reviewer) from "CLI orchestration" (main + helpers). Would make F1/F2/F4 individually easier to spot on future review.

## Architecture note (not a blocker, flagged for confirmation)

This is a fully standalone system — not a DOT node type (nothing registered in `TYPE_REGISTRY`/`REGISTRY` in `runner/handlers.py`), not driven by `runner/engine.py`, invoked only from the new GitHub Actions workflow. The repo already has an in-pipeline multi-reviewer/shadow-review mechanism (`runner/handler_parallel_reviewer.py` + `runner/handler_dispatch.py`: `_resolve_gate_backend`, `_execute_gate`, `_start_shadow_gate_review`/`_finish_shadow_gate_review`) and a separate verdict-parsing/SHA-binding module (`runner/handler_verdict.py`: `_parse_verdict`, `_verify_head_sha_echo`, `_HEAD_SHA_ECHO_RE`). The new skeptic gate re-implements verdict parsing, SHA binding, and multi-reviewer aggregation from scratch with a stricter, purpose-built 6-field contract (`VERDICT`/`HEAD_SHA`/`REPO`/`PR_NUMBER`/`REASON`/`IDENTITY`, no-prose, exactly-once) rather than the existing marker-based free-form parser. Given the stated purpose — a GitHub Actions merge-protection gate bound to the live PR head, run entirely outside any `dark-factory` pipeline invocation — a separate contract is defensible (the existing `handler_verdict._parse_verdict` accepts many verdict synonyms and doesn't do 6-field structured no-prose binding, which this use case explicitly needs). Flagging so the reviewer/maintainer can confirm this is an intentional layer split (CI-merge-gate vs. in-pipeline-gate) rather than something that should have reused `handler_verdict.py` — CLAUDE.md's "canonical layer" tenet is at least worth a one-line acknowledgment in either module's docstring pointing at the other, so a future reader doesn't have to rediscover the split by grepping.

## /code-standards findings

**Ponytail (reuse ladder)**
- No existing `gh api` wrapper existed anywhere under `runner/` before this PR (`git grep -n "gh_api\|def _gh(" pr281-review -- 'runner/*.py'` → no hits outside the new files) — writing `gh_api()` fresh in `skeptic_gate_cli.py` is justified, not a violation.
- The allowlist-based `_reviewer_env` (`skeptic_gate_cli.py:141-154`) is a legitimately different security posture from the denylist-based `_sanitized_env` in `runner/handler_sandbox.py:96-104` (allowlist is strictly stronger — needed here because the prompt embeds an attacker-influenced PR diff going to an external subprocess). Not a duplication problem; worth a one-line comment noting the deliberate choice of allowlist-over-denylist relative to the existing helper, for a future reader who might otherwise "fix" the inconsistency by merging them.
- F2 above is the one true "should have reused/simplified rather than hand-rolled a second mechanism" finding in this lane — reusing `_FULL_SHA_RE` alone instead of introducing `_SHA_RE` as a second overlapping check.

**ZFC (Zero-Framework Cognition)**
No violations found. The two places that look like they *could* be keyword-routing are both legitimately exempt:
- `COMMIT_PREFIX_TO_IDENTITY` (`skeptic_gate.py:110-119`) — pure deterministic prefix match on a commit-subject convention the repo's own `CLAUDE.md` mandates ("Commit provenance tag"), not semantic classification of free-form prose. The module docstring (lines 61-71) explicitly argues this and the argument holds up under inspection: it's a fixed lookup table over a fixed set of prefixes the repo controls, functionally identical to a config/path-resolution table.
- The "no-prose" check in `parse_verdict` (`skeptic_gate.py:238-277`) is syntax-shape validation (does a line start with `UPPER_CASE:`?), not judgment about what the line *means* — falls under the "pure syntax parsing" exemption.
- `REVIEWER_CLI_TO_IDENTITY` and the `_parse_reviewers` allow-list (`codex`/`gemini` only) are fixed enum-style config, not routing on interpreted text.

**Root-cause-first**
- The gate enforces invariants (SHA equality, byte-for-byte read-back, ALL-reviewers-must-pass, distinct-reviewer-identity, CLI↔identity binding) rather than special-casing symptoms — this is the intended pattern for this file and it's followed well in the parts that work.
- However, the file's own commentary is a visible record of **patch-by-audit-comment**: nearly every non-trivial check carries a `"Per post-audit comment 4953064910"` or `"...4953116428"` citation (I count 9 distinct callouts across the two files) describing a *prior* version that was wrong and what was bolted on to fix it (env stripped `HOME` after the fact, version-check ran before hash-check then got reordered, status-publish order was fail-open then fixed, PR_NUMBER/REVIEWER/IMPLEMENTATION_PROVENANCE read-back fields were "too narrow" then expanded, the 7th-field / duplicate-field anti-injection check was added after a specific bypass was found). Each individual patch looks sound in isolation, but the pattern itself — and the fact that this lane just found a *new*, still-unpatched bug (F1) that these audit cycles didn't catch — is exactly the "reactive-cascade" pattern this repo's own `CLAUDE.md` warns about (fix the symptom the auditor pointed at, ship, wait for the next audit comment). Recommend one consolidated pass reading the whole fail-closed contract top-to-bottom against a written threat list (stale SHA, wrong repo/PR, self-review, duplicate reviewer, mutable PATH, secret leakage, oversized diff, comment/status write failure, read-back mismatch, mid-run SHA drift) rather than continuing to accrete one more `if` per future audit finding.

## Lane summary

| Severity | Count | IDs |
|---|---|---|
| Blocker | 1 | F1 |
| Strong | 2 | F2, F3 |
| Nit | 2 | F4, F5 |

5 findings total (1 blocker, 2 strong, 2 nit), plus one architecture note flagged for confirmation (not counted as a defect — the split may well be intentional).
