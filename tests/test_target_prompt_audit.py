from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner import prompt_substitution_audit as audit  # noqa: E402


class TestFreshReviewGoalAllowlist:
    def test_fresh_review_prompt_is_allowlisted_from_goal_requirement(self):
        assert "prompts/slim/fresh_review.md" in audit.PROMPTS_WITHOUT_GOAL_OK

    def test_check_minimum_content_does_not_flag_missing_goal_for_fresh_review(self):
        violations = audit.check_minimum_content(ROOT / "prompts")
        kind_c_goal = [
            v for v in violations
            if v.location == "<no ${goal}>" and v.prompt == "prompts/slim/fresh_review.md"
        ]
        assert kind_c_goal == []


class TestRenderedReviewerPromptCheck:
    def test_clean_prompt_has_no_violations(self):
        assert audit.check_rendered_reviewer_prompt("Review target: git-range://x@a..b") == []

    def test_unsubstituted_target_flagged(self):
        problems = audit.check_rendered_reviewer_prompt("Review target: ${target}")
        assert any("target" in p for p in problems)

    def test_unsubstituted_intent_flagged(self):
        problems = audit.check_rendered_reviewer_prompt("${intent}")
        assert any("intent" in p for p in problems)

    def test_both_unsubstituted_flagged_together(self):
        problems = audit.check_rendered_reviewer_prompt("${target} ${intent}")
        assert len(problems) == 2
