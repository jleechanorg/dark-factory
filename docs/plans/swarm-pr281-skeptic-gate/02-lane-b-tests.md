# Lane B — tests/test_skeptic_gate.py (PR #281, head e70b1ec)

## Scope

Reviewed `tests/test_skeptic_gate.py` (1858 lines, entirely new) against the
production modules it exercises: `runner/skeptic_gate.py` (1000 lines,
deterministic parse/bind/aggregate logic) and `runner/skeptic_gate_cli.py`
(1119 lines, the GitHub-API-facing orchestrator). Both source files are also
new in this PR. Read via `git show pr281-review:<path>` since the local
worktree is on `pr228`.

Lenses applied: `/thermo` (test judo, self-certification, coverage
asymmetry) and `/code-standards` (ponytail rung 2 reuse, real-vs-fake).

## Findings

### F1 — Dead/obfuscated assertion left in `test_parse_verdict_full_contract_passes` (nit, correctness-adjacent)

**Location:** `tests/test_skeptic_gate.py:100`

```python
assert parsed.reviewr_identity if False else parsed.reviewer_identity == "codex"
```

**Evidence:** `grep -n "reviewr_identity" tests/test_skeptic_gate.py` returns
exactly this one line. `reviewr_identity` (missing the second `e`) is not an
attribute on `ParsedVerdict` — the real field is `reviewer_identity`
(`runner/skeptic_gate.py:137`). Because the ternary's condition is the
literal `False`, Python never evaluates the true-branch
(`parsed.reviewr_identity`), so the line does not currently raise
`AttributeError` and is behaviorally equivalent to
`assert parsed.reviewer_identity == "codex"`. But it reads as if it might be
testing something conditional, references a nonexistent attribute, and is
strictly harder to review than a plain assert — this is very likely a
merge/edit artifact (a failed self-correction that left dead code behind)
rather than an intentional test.

**Fix:** replace with `assert parsed.reviewer_identity == "codex"`.

### F2 — Coverage asymmetry: the GitHub-API glue in `skeptic_gate_cli.py` has zero direct test coverage of its own logic (moderate, matches the repo's known gate self-certification concern)

**Evidence:** every CLI-level test in this file monkeypatches the API-facing
functions wholesale rather than exercising their internals:

```
grep -n "get_pr_head_sha_via_api\|post_or_update_comment\|read_back_comment\|find_existing_bot_comment\|_publish_failure\|_pr_number_for_desc\|set_commit_status\|gh_api" tests/test_skeptic_gate.py
```

- `get_pr_head_sha_via_api`, `get_pr_diff`, `post_or_update_comment`,
  `read_back_comment`, `set_commit_status` — appear **only** as
  `monkeypatch.setattr(cli_mod, "<name>", <fake>)` targets. Their real
  bodies (the `gh api` / `gh pr diff` subprocess calls, JSON parsing, the
  `MAX_DIFF_BYTES` check inside `get_pr_diff` itself, the PATCH-vs-POST
  branch in `post_or_update_comment`) never run in the suite.
- `find_existing_bot_comment` (the pagination + `MARKER` lookup that makes
  the "idempotent upsert" claim in the module docstring true) — **zero**
  occurrences anywhere in the test file, direct or indirect, because its
  only caller (`post_or_update_comment`) is always replaced.
- `_publish_failure` and `_pr_number_for_desc` — **zero** occurrences.
  `_publish_failure` is the real-mode side-effect path taken when diff
  capture fails (`skeptic_gate_cli.py:654-661`, `682-689`), but
  `test_adversarial_diff_oversize_fails_closed` builds its argv via the
  `_cli_argv()` helper, which unconditionally appends `--dry-run`
  (`tests/test_skeptic_gate.py:825-839`). Since `_publish_failure` is only
  called `if not args.dry_run`, this test can never reach it — the module's
  own "fail loudly and post a FAIL status/comment when the diff can't be
  captured" behavior is asserted only via stderr text, never via the actual
  publish call.
- `gh_api` (the one function that shells out to the real `gh` binary and
  parses its JSON) is reassigned wholesale in
  `test_get_implementation_identity_uses_head_sha_direct_lookup` and
  `..._falls_back_to_pr_commits_when_head_missing` — but always to a full
  replacement, never exercised for its own subprocess/error-handling logic
  (non-JSON stdout, non-zero rc, pagination assembly).

**Why this matters here specifically:** the repo's own memory
(`feedback_2026-07-06_gate_self_certification.md`) flags "a check whose
expected value comes from its own template can't fail" as a known
anti-pattern for this codebase's gates. This isn't quite that — the pure
functions in `skeptic_gate.py` are tested against independently-constructed
literal strings, which is correct. But the *side-effecting* half of the
gate (the part that actually talks to GitHub and would be the observable
surface of a real self-certification bug — e.g. an off-by-one in the
comment-pagination lookup silently always creating a new comment instead of
updating, or `_publish_failure` swallowing an error and leaving the PR
green) is validated only by asserting the mocks were called with the right
arguments, never by exercising the real implementation against a fake `gh`
executable or a stubbed `subprocess.run`. Given the module's stated design
goal is "fail-closed against GitHub API weirdness," the two paths most
likely to silently regress into fail-open (`find_existing_bot_comment`
dedup logic, `_publish_failure`) are exactly the ones with no test at all.

**Fix suggestion:** add one test that stubs `subprocess.run` (not
`gh_api` itself) to exercise `gh_api`'s JSON/error-handling branches, one
that calls `find_existing_bot_comment` against a stubbed paginated
`gh_api` to prove the MARKER-based dedup logic works, and one CLI-level
test that omits `--dry-run` and forces `get_pr_diff` to raise, asserting
`_publish_failure` → `set_commit_status(state="failure", ...)` actually
fires (currently only inferable from the printed stderr string).

### F3 — Test-judo: near-identical CLI-test setup boilerplate repeated 7 times (nit, maintainability)

**Location:** `test_cli_forced_pass_with_both_reviewers`,
`test_cli_forced_fail_with_missing_reviewer`,
`test_cli_provenance_fails_self_review`,
`test_adversarial_status_failure_is_fail_closed`,
`test_status_publish_order_pending_then_success`,
`test_status_readback_mismatch_overwrites_to_failure`,
`test_status_overwritten_failure_never_becomes_success`
(lines ~885-1858).

**Evidence:** each of these 7 tests repeats the same 3-4 line block:

```python
monkeypatch.setattr(cli_mod, "get_pr_head_sha_via_api", lambda repo, pr, head_sha="": "abcdef...ef12")
monkeypatch.setattr(cli_mod, "get_pr_diff", lambda repo, pr, head_sha="": "...")
monkeypatch.setattr(cli_mod, "get_implementation_identity", lambda repo, pr, head_sha="": "claude")
```
plus a near-identical `fake_cmd` closure that shells out to `python3 -c` to
emit `_reviewer_stdout(...)`. This is a legitimate `pytest` fixture
extraction opportunity (e.g. `patched_cli_context(monkeypatch)` yielding the
three patches, with `fake_cmd` as a shared helper parametrized by a
verdict/identity map) — not a blocker, but the seven near-duplicate blocks
are exactly the kind of repetition `/thermo` test-judo asks to consolidate,
and `tests/conftest.py` is the natural home for it if other new CLI-gate
test files show up later.

## Lane summary

3 findings, 0 blockers. **F1** (dead ternary with a typo'd attribute name)
is a one-line cleanup. **F2** is the substantive one: the deterministic,
pure-function core (`skeptic_gate.py`) is thoroughly and honestly tested
against independent literals, but the GitHub-API-facing half of the gate
(`skeptic_gate_cli.py`'s `gh_api`, `find_existing_bot_comment`,
`post_or_update_comment`, `_publish_failure`) is exercised only by mocking
those functions out of existence, leaving the actual idempotent-upsert and
diff-capture-failure-publish logic with no test proving it does what the
module docstring claims. **F3** is a straightforward DRY/fixture-extraction
opportunity across 7 CLI tests. No self-certification anti-pattern found in
the pure-function tests — expected values there are independent literals,
not values derived from the code under test.
