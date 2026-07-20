import unittest
from unittest.mock import MagicMock
from runner.rule_loader import Rule
from runner.consensus import ConsensusAggregator

class TestConsensus(unittest.TestCase):
    def test_aggregation_fail_closed(self):
        agg = ConsensusAggregator()
        rule1 = Rule("rule1", "Rule 1", ["*"], "cheap", "Desc", "Prompt")
        res1 = MagicMock(check_state="success", verdict="PASS", parsed=None, reason="OK")
        
        rule2 = Rule("rule2", "Rule 2", ["*"], "cheap", "Desc", "Prompt")
        res2 = MagicMock(check_state="success", verdict="FAIL", parsed=None, reason="Failed")

        verdict = agg.aggregate([(rule1, res1), (rule2, res2)])
        self.assertEqual(verdict, "FAIL")

    def test_aggregation_all_pass(self):
        agg = ConsensusAggregator()
        rule1 = Rule("rule1", "Rule 1", ["*"], "cheap", "Desc", "Prompt")
        res1 = MagicMock(check_state="success", verdict="PASS", parsed=None, reason="OK")

        verdict = agg.aggregate([(rule1, res1)])
        self.assertEqual(verdict, "PASS")

    def test_compile_report(self):
        agg = ConsensusAggregator()
        rule1 = Rule("rule1", "Rule 1", ["*"], "cheap", "Desc", "Prompt")
        res1 = MagicMock(check_state="success", verdict="PASS", parsed=None, reason="OK")

        verdict, body = agg.compile_report(
            [(rule1, res1)],
            "jleechanorg/dark-factory",
            123,
            "head_sha_value",
            "head_sha_value",
            "claude"
        )
        self.assertEqual(verdict, "PASS")
        self.assertIn("All 1 matching skeptic rules passed successfully.", body)
        self.assertIn("Rule 1 (rule1)", body)
