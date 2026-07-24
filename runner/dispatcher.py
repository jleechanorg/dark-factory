from __future__ import annotations
import concurrent.futures
import fnmatch
import re
from typing import List, Tuple, Dict, Any
from runner.rule_loader import Rule
from runner.skeptic_gate_cli import invoke_reviewer as _invoke_reviewer_module_ref
from runner.skeptic_gate import SkepticResult
from runner import skeptic_gate_cli as _cli

# Resolve build_prompt/evaluate/invoke_reviewer through the CLI module
# at CALL TIME (not import time) so tests can
# `monkeypatch.setattr(cli_mod, ...)` and observe the patch when the
# dispatcher runs. The CLI module re-exports all three names from
# `runner.skeptic_gate`/`runner.skeptic_gate_cli`; resolving through
# `_cli.X` here makes the late-bound attribute lookup propagate. See
# issue #386 r3 wiring + tests/test_skeptic_gate_cli_bead_id_r3.py and
# tests/test_skeptic_gate_cli_contract_echo.py.

class VerifierDispatcher:
    def __init__(self, cheap_reviewer: str = "gemini", cheap_model: str = "gemini-2.5-flash",
                 premium_reviewer: str = "gemini", premium_model: str = "gemini-2.5-pro"):
        self.cheap_reviewer = cheap_reviewer
        self.cheap_model = cheap_model
        self.premium_reviewer = premium_reviewer
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

    def build_rule_prompt(self, rule: Rule, repo: str, pr_number: int, head_sha: str, base_sha: str, diff: str, implementation_identity: str, contract=None) -> str:
        base_prompt = _cli.build_prompt(
            repo=repo,
            pr_number=pr_number,
            head_sha=head_sha,
            base_sha=base_sha,
            diff=diff,
            implementation_identity=implementation_identity,
            contract=contract,
        )
        return (
            f"{base_prompt}\n\n"
            f"# Rule-Specific Guidelines: {rule.name}\n"
            f"Rule ID: {rule.id}\n"
            f"Description: {rule.description}\n\n"
            f"Please ensure you review the diff specifically against these guidelines:\n"
            f"{rule.prompt}\n"
        )

    def dispatch(self, rules: List[Rule], changed_files: List[str], diff: str, repo: str, pr_number: int, head_sha: str, base_sha: str, implementation_identity: str, contract=None) -> List[Tuple[Rule, SkepticResult]]:
        matching_rules = [r for r in rules if self.rule_matches(r, changed_files)]
        if not matching_rules:
            return []

        results: List[Tuple[Rule, SkepticResult]] = []

        def run_one(rule: Rule) -> Tuple[Rule, SkepticResult]:
            prompt = self.build_rule_prompt(
                rule, repo, pr_number, head_sha, base_sha, diff, implementation_identity, contract=contract
            )
            reviewer = rule.reviewer
            model = rule.model
            if not reviewer:
                if rule.model_tier == "premium":
                    reviewer = self.premium_reviewer
                    model = self.premium_model
                else:
                    reviewer = self.cheap_reviewer
                    model = self.cheap_model

            stdout, err = _cli.invoke_reviewer(reviewer, model, prompt)
            res = _cli.evaluate(
                review_output=stdout,
                review_error=err,
                repo=repo,
                pr_number=pr_number,
                head_sha=head_sha,
                implementation_provenance=implementation_identity,
                base_sha="unknown",
                diff=diff,
                reviewer=reviewer,
                contract=contract,
            )

            # Enforce security checks: binding and provenance (self-review rejection)
            if res.check_state == "success" and res.parsed is not None:
                from runner.skeptic_gate import bind_reviewer_identity, verify_provenance, format_comment
                
                # 1. CLI -> identity binding
                ok_bind, why_bind = bind_reviewer_identity(reviewer, res.parsed.reviewer_identity)
                if not ok_bind:
                    body = format_comment(
                        verdict="FAIL",
                        head_sha=head_sha,
                        expected_head_sha=head_sha,
                        repo=repo,
                        pr_number=pr_number,
                        reviewer=reviewer,
                        implementation_provenance=implementation_identity,
                        reason=why_bind,
                    )
                    return rule, SkepticResult(
                        check_state="failure",
                        verdict=None,
                        reason=why_bind,
                        comment_body=body,
                        parsed=res.parsed,
                        reviewer=reviewer,
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
                        reviewer=reviewer,
                        implementation_provenance=implementation_identity,
                        reason=why_prov,
                    )
                    return rule, SkepticResult(
                        check_state="failure",
                        verdict=None,
                        reason=why_prov,
                        comment_body=body,
                        parsed=res.parsed,
                        reviewer=reviewer,
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
