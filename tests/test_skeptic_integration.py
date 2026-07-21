import unittest
from unittest.mock import patch, MagicMock
from runner.handlers import _gate_skeptic
from runner.handler_core import Context, Result

class TestSkepticIntegration(unittest.TestCase):
    @patch("runner.rule_loader.RuleLoader")
    @patch("runner.scm_provider.LocalGitScm")
    @patch("runner.dispatcher.VerifierDispatcher")
    @patch("runner.consensus.ConsensusAggregator")
    def test_gate_skeptic_runs_consensus(self, mock_aggregator, mock_dispatcher, mock_scm, mock_loader):
        # Configure mocks
        mock_loader_instance = MagicMock()
        mock_loader.return_value = mock_loader_instance
        mock_loader_instance.load_rules.return_value = [MagicMock(id="rule1")]

        mock_scm_instance = MagicMock()
        mock_scm.return_value = mock_scm_instance
        mock_scm_instance.get_changed_files.return_value = ["src/main.rs"]
        mock_scm_instance.get_diff.return_value = "diff content"

        mock_dispatcher_instance = MagicMock()
        mock_dispatcher.return_value = mock_dispatcher_instance
        mock_dispatcher_instance.dispatch.return_value = [("rule1", MagicMock())]

        mock_aggregator_instance = MagicMock()
        mock_aggregator.return_value = mock_aggregator_instance
        mock_aggregator_instance.compile_report.return_value = ("PASS", "Consolidated Report")

        # Set up Context and Node
        ctx = Context(goal="test", workdir=MagicMock(), backend="ao")
        # Mock _handlers_shim._worktree_head_sha
        with patch("runner.handler_universal_prompts._handlers_shim._worktree_head_sha", return_value="head_sha_value"):
            node = MagicMock()
            node.name = "gate_skeptic"
            node.attrs = {}
            
            res = _gate_skeptic(node, ctx)
            
            # Assertions
            self.assertEqual(res.outcome, "success")
            self.assertEqual(res.output, "Consolidated Report")
            self.assertEqual(res.metadata["verdict"], "PASS")
