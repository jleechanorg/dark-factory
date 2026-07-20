from __future__ import annotations
import concurrent.futures
import fnmatch
import re
from typing import List, Tuple, Dict, Any
from runner.rule_loader import Rule
from runner.skeptic_gate_cli import invoke_reviewer
from runner.skeptic_gate import evaluate, SkepticResult, build_prompt

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
        for f in changed_files:
            for glob in rule.target_globs:
                if self.match_glob(f, glob):
                    return True
        return False

    def build_rule_prompt(self, rule: Rule, repo: str, pr_number: int, head_sha: str, base_sha: str, diff: str, implementation_identity: str) -> str:
        base_prompt = build_prompt(
            repo=repo,
            pr_number=pr_number,
            head_sha=head_sha,
            base_sha=base_sha,
            diff=diff,
            implementation_identity=implementation_identity
        )
        return (
            f"{base_prompt}\n\n"
            f"# Rule-Specific Guidelines: {rule.name}\n"
            f"Rule ID: {rule.id}\n"
            f"Description: {rule.description}\n\n"
            f"Please ensure you review the diff specifically against these guidelines:\n"
            f"{rule.prompt}\n"
        )

    def dispatch(self, rules: List[Rule], changed_files: List[str], diff: str, repo: str, pr_number: int, head_sha: str, base_sha: str, implementation_identity: str) -> List[Tuple[Rule, SkepticResult]]:
        matching_rules = [r for r in rules if self.rule_matches(r, changed_files)]
        if not matching_rules:
            return []

        results: List[Tuple[Rule, SkepticResult]] = []

        def run_one(rule: Rule) -> Tuple[Rule, SkepticResult]:
            prompt = self.build_rule_prompt(
                rule, repo, pr_number, head_sha, base_sha, diff, implementation_identity
            )
            if rule.model_tier == "premium":
                reviewer = self.premium_reviewer
                model = self.premium_model
            else:
                reviewer = self.cheap_reviewer
                model = self.cheap_model

            stdout, err = invoke_reviewer(reviewer, model, prompt)
            res = evaluate(
                stdout, err, repo, pr_number, head_sha, reviewer, implementation_identity
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
