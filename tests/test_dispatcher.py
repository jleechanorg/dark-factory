import unittest
from unittest.mock import patch, MagicMock
from runner.rule_loader import Rule
from runner.dispatcher import VerifierDispatcher

class TestDispatcher(unittest.TestCase):
    def test_match_glob(self):
        dispatcher = VerifierDispatcher()
        self.assertTrue(dispatcher.match_glob("src/main.rs", "**/*.rs"))
        self.assertTrue(dispatcher.match_glob("tests/test_foo.py", "**/*.py"))
        self.assertTrue(dispatcher.match_glob("README.md", "*"))
        self.assertFalse(dispatcher.match_glob("src/main.rs", "**/*.py"))

    def test_rule_matches(self):
        dispatcher = VerifierDispatcher()
        rule = Rule("rule1", "Rule 1", ["**/*.py"], "cheap", "Desc", "Prompt")
        self.assertTrue(dispatcher.rule_matches(rule, ["src/main.py", "README.md"]))
        self.assertFalse(dispatcher.rule_matches(rule, ["src/main.rs", "README.md"]))

    @patch("runner.dispatcher.invoke_reviewer")
    @patch("runner.dispatcher.evaluate")
    def test_dispatch(self, mock_evaluate, mock_invoke):
        mock_invoke.return_value = ("stdout_content", None)
        mock_evaluate.return_value = MagicMock(check_state="success")

        rule_cheap = Rule("rule_cheap", "Cheap Rule", ["**/*.py"], "cheap", "Desc", "Prompt")
        rule_premium = Rule("rule_premium", "Premium Rule", ["**/*.rs"], "premium", "Desc", "Prompt")

        dispatcher = VerifierDispatcher(
            cheap_reviewer="gemini", cheap_model="gemini-2.5-flash",
            premium_reviewer="codex", premium_model="codex-smart"
        )
        results = dispatcher.dispatch(
            [rule_cheap, rule_premium],
            ["src/main.py", "src/main.rs"],
            "diff_content",
            "jleechanorg/dark-factory",
            123,
            "head_sha_value",
            "base_sha_value",
            "claude"
        )

        self.assertEqual(len(results), 2)
        mock_invoke.assert_any_call("gemini", "gemini-2.5-flash", unittest.mock.ANY)
        mock_invoke.assert_any_call("codex", "codex-smart", unittest.mock.ANY)
