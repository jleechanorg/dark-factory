from __future__ import annotations
from typing import List, Tuple
from runner.rule_loader import Rule
from runner.skeptic_gate import SkepticResult, format_comment

class ConsensusAggregator:
    def aggregate(self, results: List[Tuple[Rule, SkepticResult]]) -> str:
        if not results:
            return "PASS"
        for rule, res in results:
            if res.check_state == "failure":
                return "FAIL"
            if res.verdict == "FAIL":
                return "FAIL"
        return "PASS"

    def compile_report(self, results: List[Tuple[Rule, SkepticResult]], repo: str, pr_number: int, head_sha: str, expected_head_sha: str, implementation_provenance: str) -> Tuple[str, str]:
        if not results:
            agg_verdict = "PASS"
            agg_reason = "No matching skeptic review rules found for the changed files."
            body = format_comment(
                verdict=agg_verdict,
                head_sha=head_sha,
                expected_head_sha=expected_head_sha,
                repo=repo,
                pr_number=pr_number,
                reviewer="(aggregate)",
                implementation_provenance=implementation_provenance,
                reason=agg_reason,
            )
            return agg_verdict, body

        agg_verdict = self.aggregate(results)
        
        extras: List[str] = []
        failed_rules = []
        for rule, res in results:
            marker = "✅ PASS" if (res.check_state == "success" and res.verdict == "PASS") else "❌ FAIL"
            extras.append(f"- **Rule: {rule.name} ({rule.id})** — {marker} — {res.reason}")
            if marker == "❌ FAIL":
                failed_rules.append(rule.name)

        if agg_verdict == "PASS":
            agg_reason = f"All {len(results)} matching skeptic rules passed successfully."
        else:
            agg_reason = f"Skeptic review failed for rules: {', '.join(failed_rules)}"

        bound = [res for _, res in results if res.parsed is not None]
        primary_sha = bound[0].parsed.head_sha if bound and bound[0].parsed else head_sha

        body = format_comment(
            verdict=agg_verdict,
            head_sha=primary_sha,
            expected_head_sha=expected_head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
            reason=agg_reason,
            extra_reviewer_lines=extras,
        )
        return agg_verdict, body
