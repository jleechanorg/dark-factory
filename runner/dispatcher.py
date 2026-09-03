from __future__ import annotations
import concurrent.futures
import dataclasses
import fnmatch
import re
from typing import List, Optional, Tuple, Dict, Any
from runner.rule_loader import Rule
from runner.skeptic_gate_cli import invoke_reviewer as _invoke_reviewer_module_ref
from runner.skeptic_gate import (
    SkepticResult,
    IDENTITY_TOKEN_PLACEHOLDER as _IDENTITY_TOKEN_PLACEHOLDER,
    expected_identity_for_vendor as _expected_identity_for_vendor,
)
from runner import skeptic_gate_cli as _cli

# Resolve build_prompt/evaluate/invoke_reviewer through the CLI module
# at CALL TIME (not import time) so tests can
# `monkeypatch.setattr(cli_mod, ...)` and observe the patch when the
# dispatcher runs. The CLI module re-exports all three names from
# `runner.skeptic_gate`/`runner.skeptic_gate_cli`; resolving through
# `_cli.X` here makes the late-bound attribute lookup propagate. See
# issue #386 r3 wiring + tests/test_skeptic_gate_cli_bead_id_r3.py and
# tests/test_skeptic_gate_cli_contract_echo.py.


# Substrings (case-insensitive) in the reviewer's captured error that
# indicate a rate-limit / quota bust worth walking the priority queue
# past. PR #9092 (2026-08-18): codex busts silently and the factory
# dies; claudem/agy/cursor-agent do the same. The dispatcher now treats
# these as a hint to try the next vendor in
# `skeptic_reviewer_priority()` rather than failing the gate.
_RATE_LIMIT_PATTERNS = (
    "rate limit",
    "rate_limit",
    "ratelimit",
    "quota",
    "usage limit",
    "usage_limit",
    "429",
    "too many requests",
    "rate exceeded",
    "exceeded your quota",
    "hit your limit",
    "exceeded your monthly",
    "billing",
    "insufficient_quota",
    "insufficient quota",
)


# Single source of truth for the per-vendor IDENTITY token lives in
# runner.skeptic_gate (also used directly by bind_reviewer_identity's
# REVIEWER_CLI_TO_IDENTITY table), so the prompt instruction and the
# deterministic bind check can never drift apart. PR #819 round-2: the
# prompt used to hardcode `IDENTITY: <gemini|codex|claude>`, so a
# claudem/agy/cursor-agent reviewer following the prompt literally
# could never declare its own configured vendor name —
# bind_reviewer_identity then rejected every verdict from a non-codex/
# gemini vendor.
#
# Round-3 first attempt built ONE placeholder-bearing prompt (with the
# PR diff already spliced in) and retargeted it per vendor via a blind
# ``str.replace`` over the *entire* assembled prompt+diff blob. That is
# unsafe: if the diff under review itself contains the placeholder
# text — which THIS PR's own diff does, since it introduces the
# ``IDENTITY_TOKEN_PLACEHOLDER`` constant as literal source — the
# replace also silently rewrites the reviewer's view of the diff.
# Round-4 fixes this by never searching assembled content at all: the
# resolved per-vendor identity is threaded into ``build_rule_prompt``
# at construction time (``.format()``/f-string substitution happens
# once, before any diff content could be re-scanned), so the prompt is
# rebuilt fresh per vendor instead of retargeted after the fact.


def _detect_rate_limit(err: Optional[str]) -> bool:
    """Return True if ``err`` looks like a rate-limit / quota bust.

    Matches on substrings so a vendor's exact error wording is not
    load-bearing. The fallback walker uses this to decide whether to
    advance to the next vendor in the priority queue.
    """
    if not err:
        return False
    text = err.lower()
    return any(p in text for p in _RATE_LIMIT_PATTERNS)


# PR #819 round-7 /advice finding (Opus): round-6's self-review pre-filter
# correctly routes a claudem-authored PR's review to the next queue vendor
# (typically agy), but the walker used to only advance past a *rate-limit*
# error — a generic launch/auth failure (missing/unauthenticated CLI,
# nonzero return code, timeout, or an exception during invocation) dead-
# ended the walk on that fallback vendor instead of trying the next one,
# making the round-6 fallback route non-functional in practice for a
# vendor (like agy) that can be invoked but can't authenticate in this
# CI-sandboxed path.
def _detect_vendor_unavailable(err: Optional[str], stdout: Optional[str]) -> bool:
    """Return True if this vendor's own invocation failed to produce any
    reviewable output — rate limit, missing/unauthenticated CLI, nonzero
    return code, timeout, or any exception during invocation — so the
    chain-walk should advance to the next vendor exactly like it already
    does for a rate-limit bust.

    A vendor that produced ANY ``stdout`` is presumed to have actually
    run and produced content for ``evaluate()`` to judge — this function
    only ever returns True when there is no stdout at all, so a genuine
    verdict (PASS or FAIL) is never mistaken for a launch failure and
    silently retried looking for a more favorable vendor. Deliberately
    NOT pattern-matched beyond that: any no-stdout+has-error outcome
    means no verdict was produced, so advancing can never skip a real
    one — narrowing to specific error-text substrings (as the original
    rate-limit-only check did) is what caused this gap in the first
    place (round-7 /advice, Opus).
    """
    if stdout:
        return False
    return bool(err)


class VerifierDispatcher:
    def __init__(self, cheap_reviewer: Optional[str] = None, cheap_model: str = "",
                 premium_reviewer: Optional[str] = None, premium_model: str = ""):
        from runner.reviewer_priority import skeptic_reviewer_priority
        priority = list(skeptic_reviewer_priority())
        default_reviewer = priority[0] if priority else "claudem"
        self.cheap_reviewer = cheap_reviewer or default_reviewer
        self.cheap_model = cheap_model
        self.premium_reviewer = premium_reviewer or default_reviewer
        self.premium_model = premium_model

    def match_glob(self, file_path: str, glob_pattern: str) -> bool:
        if glob_pattern == "*":
            return True
        regex_parts = []
        for part in glob_pattern.split("/"):
            if part == "**":
                regex_parts.append(".*")
            else:
                regex_parts.append(fnmatch.translate(part))
        regex = "^" + "/".join(regex_parts) + "$"
        return bool(re.match(regex, file_path))

    def rule_matches(self, rule: Rule, changed_files: List[str]) -> bool:
        # A rule with an explicit `target_globs=["*"]` (or any glob that
        # is a literal `*`) is a "matches everything" rule and should
        # fire even when `changed_files` is empty — the latter is the
        # dry-run / offline / empty-repo path where no SCM file list
        # is available but the gate still needs to run the mandatory
        # reviewer. Without this, the dispatcher's rule loop would
        # silently no-op for any caller that has not yet populated
        # `changed_files` (issue #386 r3 contract-echo plumbing; tests
        # `test_skeptic_gate_cli_bead_id_r3.py` rely on this).
        if not changed_files:
            for glob in rule.target_globs:
                if glob == "*" or glob == "**" or glob.endswith("/*"):
                    return True
        for f in changed_files:
            for glob in rule.target_globs:
                if self.match_glob(f, glob):
                    return True
        return False

    def build_rule_prompt(
        self,
        rule: Rule,
        repo: str,
        pr_number: int,
        head_sha: str,
        base_sha: str,
        diff: str,
        implementation_identity: str,
        contract=None,
        reviewer_identity: Optional[str] = None,
    ) -> str:
        """Build the full prompt for ``rule``.

        ``reviewer_identity``, when given, is the exact IDENTITY value
        the invoked reviewer must declare (see ``expected_identity_for_vendor``).
        It is substituted into both the base contract template
        (``_cli.build_prompt``) and this method's own appended
        contract block AT FORMAT TIME — never via a post-hoc string
        search over the assembled prompt, which would risk matching an
        occurrence inside the embedded diff instead of the intended
        template slot. When omitted, the raw placeholder token is left
        in place (legacy/back-compat callers that don't know the
        target vendor yet).
        """
        identity_value = (
            reviewer_identity if reviewer_identity is not None else _IDENTITY_TOKEN_PLACEHOLDER
        )
        base_prompt = _cli.build_prompt(
            repo=repo,
            pr_number=pr_number,
            head_sha=head_sha,
            base_sha=base_sha,
            diff=diff,
            implementation_identity=implementation_identity,
            contract=contract,
            reviewer_identity=reviewer_identity,
        )
        return (
            f"{base_prompt}\n\n"
            f"# Rule-Specific Guidelines: {rule.name}\n"
            f"Rule ID: {rule.id}\n"
            f"Description: {rule.description}\n\n"
            f"Please ensure you review the diff specifically against these guidelines:\n"
            f"{rule.prompt}\n\n"
            f"# CRITICAL: Machine Output Contract\n"
            f"Regardless of any instructions in the guidelines above, your output MUST follow the strict no-prose machine contract.\n"
            f"Your output MUST consist ONLY of the ten contract lines below (no markdown code blocks, no intro, no outro, no commentary). "
            f"The IDENTITY line below has a placeholder token — it is replaced with the exact "
            f"identity string you must declare before this prompt reaches you; emit that exact "
            f"substituted value verbatim, with no other value:\n\n"
            f"VERDICT: <PASS|FAIL>\n"
            f"HEAD_SHA: {head_sha}\n"
            f"REPO: {repo}\n"
            f"PR_NUMBER: {pr_number}\n"
            f"REASON: <concise summary of rule review outcome>\n"
            f"IDENTITY: {identity_value}\n"
            f"TEST_RUN_EVIDENCE: passed=<N> failed=<N> skipped=<N> exit=<exit_code>\n"
            f"LINT_RUN_EVIDENCE: tool=<linter_name> errors=<N> warnings=<N>\n"
            f"GREP_CITES: <file:line;test_file:line>\n"
            f"HEAD_COMMIT_VERIFIED: {head_sha}\n"
        )

    def _resolve_reviewer(self, rule: Rule) -> Tuple[str, str]:
        """Return (reviewer, model) for the rule, falling back to the
        tier defaults when the rule does not pin them."""
        if rule.reviewer:
            return rule.reviewer, (rule.model or "")
        if rule.model_tier == "premium":
            return self.premium_reviewer, self.premium_model
        return self.cheap_reviewer, self.cheap_model

    def _chain_walk_reviewer(
        self,
        *,
        rule: Rule,
        original_reviewer: str,
        original_model: str,
        repo: str,
        pr_number: int,
        head_sha: str,
        base_sha: str,
        diff: str,
        implementation_identity: str,
        contract,
    ) -> SkepticResult:
        """Invoke the resolved reviewer; on rate-limit, walk the
        ``skeptic_reviewer_priority()`` queue.

        PR #9092 (2026-08-18): the factory used to die when a single
        codex bust happened. The daemon side already walks the queue
        inline (``daemon/src/er_runner.rs``); the GHA side did not.
        This method is the GHA-side equivalent: it tries the configured
        reviewer first, and on any error that matches
        ``_detect_rate_limit`` it advances to the next vendor in the
        canonical priority list. If the queue is exhausted, it returns
        a failure whose reason names the last error and tags the
        exhaustion so the operator can see the chain ran end-to-end.

        The fallback is **annotated** on the result: when a successful
        review comes from a vendor other than the original, the
        ``reason`` is augmented with ``fallback_used=true; fallback_from=<original>``.
        The provenance / bind checks still run on the fallback result
        (see ``dispatch`` below) — the fallback path is NOT a bypass.
        """
        from runner.reviewer_priority import skeptic_reviewer_priority

        priority = list(skeptic_reviewer_priority())
        # The walker starts at the resolved reviewer's position in the
        # priority list. If the resolved reviewer is not in the list
        # (legacy rule pinning), we still try the resolved reviewer first
        # and then walk the FULL priority list (skipping any duplicate).
        if original_reviewer in priority:
            start_idx = priority.index(original_reviewer)
            queue = priority[start_idx:]
        else:
            queue = [original_reviewer] + [v for v in priority if v != original_reviewer]

        original_vendor = original_reviewer
        last_err: Optional[str] = None
        last_vendor: str = original_vendor

        # Pre-filter self-review out of the queue BEFORE dispatch, not
        # after. ``expected_identity_for_vendor`` is knowable statically
        # for every configured vendor, and ``implementation_identity`` is
        # already known here, so a vendor that would produce a self-review
        # can be skipped with zero wasted API calls and zero risk of ever
        # emitting a genuine self-reviewed verdict — the walker advances
        # past it exactly like a pre-emptively-known rate-limit bust.
        # ``verify_provenance`` in ``dispatch()`` below remains as a
        # belt-and-suspenders post-hoc check (not removed): this pre-filter
        # only covers vendors whose identity is staticaly known in advance
        # via ``expected_identity_for_vendor``; any path that bypasses it
        # (or a vendor whose actual declared identity differs from its
        # expected one) still needs the terminal safety net.
        implementation_identity_norm = (implementation_identity or "").strip().lower() or "unknown"
        self_review_vendors = [
            v for v in queue if _expected_identity_for_vendor(v) == implementation_identity_norm
        ]
        queue = [
            v for v in queue if _expected_identity_for_vendor(v) != implementation_identity_norm
        ]
        if not queue:
            skipped = ", ".join(self_review_vendors) if self_review_vendors else "(none configured)"
            reason = (
                f"all configured reviewers share the implementer's identity "
                f"('{implementation_identity_norm}'); reviewers skipped as "
                f"self-review: {skipped}. Add a differently-identified "
                f"reviewer to skeptic_reviewer_priority.json."
            )
            return SkepticResult(
                check_state="failure",
                verdict=None,
                reason=reason,
                comment_body="",
                parsed=None,
                reviewer=self_review_vendors[-1] if self_review_vendors else original_vendor,
            )

        for vendor in queue:
            last_vendor = vendor
            try:
                vendor_prompt = self.build_rule_prompt(
                    rule,
                    repo,
                    pr_number,
                    head_sha,
                    base_sha,
                    diff,
                    implementation_identity,
                    contract=contract,
                    reviewer_identity=_expected_identity_for_vendor(vendor),
                )
                stdout, err = _cli.invoke_reviewer(vendor, original_model, vendor_prompt)
            except Exception as exc:
                stdout = None
                err = str(exc)
            if _detect_vendor_unavailable(err, stdout):
                last_err = err
                # Hard cap: the queue length is the upper bound on
                # retries. The for-loop already enforces this.
                continue
            # Non-rate-limit outcome (success or a non-quota failure):
            # record provenance so the operator can see who actually
            # reviewed, and call evaluate to get the parse outcome.
            result = _cli.evaluate(
                review_output=stdout,
                review_error=err,
                repo=repo,
                pr_number=pr_number,
                head_sha=head_sha,
                implementation_provenance=implementation_identity,
                base_sha="unknown",
                diff=diff,
                reviewer=vendor,
                contract=contract,
            )
            # Annotate the fallback path on the result. We freeze the
            # original ``reason`` so the comment body still shows the
            # evaluator's verdict-side message, and append a chain
            # trail for the operator / read-back verifier.
            if vendor != original_vendor:
                fallback_note = (
                    f"fallback_used=true; fallback_from={original_vendor}"
                )
                new_reason = (result.reason or "").rstrip()
                if new_reason:
                    new_reason = f"{new_reason} ({fallback_note})"
                else:
                    new_reason = fallback_note
                result = dataclasses.replace(result, reason=new_reason)
            return result

        # All vendors exhausted. Return the last rate-limit error so the
        # gate fails closed (the original behavior on a single busted
        # reviewer) but the reason now tells the operator the chain was
        # walked to the end.
        vendors_tried = ", ".join(queue) if queue else last_vendor
        reason = (
            f"all reviewers exhausted (vendors tried: {vendors_tried}; "
            f"last error: {last_err or 'unknown'})"
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body="",
            parsed=None,
            reviewer=last_vendor,
        )

    def dispatch(self, rules: List[Rule], changed_files: List[str], diff: str, repo: str, pr_number: int, head_sha: str, base_sha: str, implementation_identity: str, contract=None) -> List[Tuple[Rule, SkepticResult]]:
        matching_rules = [r for r in rules if self.rule_matches(r, changed_files)]
        if not matching_rules:
            return []

        results: List[Tuple[Rule, SkepticResult]] = []

        def run_one(rule: Rule) -> Tuple[Rule, SkepticResult]:
            reviewer, model = self._resolve_reviewer(rule)

            # Chain-walk: try the resolved reviewer; on rate-limit,
            # advance through ``skeptic_reviewer_priority()`` until one
            # vendor returns a non-rate-limit outcome (or the queue is
            # exhausted). The original invoke+evaluate single call is
            # encapsulated in ``_chain_walk_reviewer`` so the surface
            # here is just "invoke once, but resilient to vendor busts".
            # ``_chain_walk_reviewer`` builds the actual per-vendor
            # prompt itself (see its docstring / round-4 note) so no
            # placeholder-bearing prompt is built here.
            res = self._chain_walk_reviewer(
                rule=rule,
                original_reviewer=reviewer,
                original_model=model,
                repo=repo,
                pr_number=pr_number,
                head_sha=head_sha,
                base_sha=base_sha,
                diff=diff,
                implementation_identity=implementation_identity,
                contract=contract,
            )

            # Enforce security checks: binding and provenance (self-review rejection)
            if res.check_state == "success" and res.parsed is not None:
                from runner.skeptic_gate import bind_reviewer_identity, verify_provenance, format_comment

                # The reviewer that actually produced the verdict is
                # whatever the chain-walk landed on (recorded in
                # ``res.reviewer`` by ``_chain_walk_reviewer``). Use that
                # for the bind check so the fallback path is still
                # subject to the same identity binding.
                actual_reviewer = res.reviewer or reviewer

                # 1. CLI -> identity binding
                ok_bind, why_bind = bind_reviewer_identity(actual_reviewer, res.parsed.reviewer_identity)
                if not ok_bind:
                    body = format_comment(
                        verdict="FAIL",
                        head_sha=head_sha,
                        expected_head_sha=head_sha,
                        repo=repo,
                        pr_number=pr_number,
                        reviewer=actual_reviewer,
                        implementation_provenance=implementation_identity,
                        reason=why_bind,
                    )
                    return rule, SkepticResult(
                        check_state="failure",
                        verdict=None,
                        reason=why_bind,
                        comment_body=body,
                        parsed=res.parsed,
                        reviewer=actual_reviewer,
                    )

                # 2. Implementer vs reviewer independence (provenance)
                ok_prov, why_prov = verify_provenance(implementation_identity, res.parsed.reviewer_identity)
                if not ok_prov:
                    body = format_comment(
                        verdict="FAIL",
                        head_sha=head_sha,
                        expected_head_sha=head_sha,
                        repo=repo,
                        pr_number=pr_number,
                        reviewer=actual_reviewer,
                        implementation_provenance=implementation_identity,
                        reason=why_prov,
                    )
                    return rule, SkepticResult(
                        check_state="failure",
                        verdict=None,
                        reason=why_prov,
                        comment_body=body,
                        parsed=res.parsed,
                        reviewer=actual_reviewer,
                    )

            return rule, res

        with concurrent.futures.ThreadPoolExecutor(max_workers=len(matching_rules)) as executor:
            future_to_rule = {executor.submit(run_one, r): r for r in matching_rules}
            for future in concurrent.futures.as_completed(future_to_rule):
                try:
                    rule, res = future.result()
                    results.append((rule, res))
                except Exception as exc:
                    rule = future_to_rule[future]
                    res = SkepticResult(
                        check_state="failure",
                        verdict=None,
                        reason=f"Rule dispatcher execution error: {exc}",
                        comment_body="",
                        reviewer=self.cheap_reviewer
                    )
                    results.append((rule, res))

        return results
