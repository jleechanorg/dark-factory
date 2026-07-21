import unittest
import tempfile
import shutil
from pathlib import Path
from runner.rule_loader import RuleLoader

class TestRuleLoader(unittest.TestCase):
    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.global_dir = Path(self.tmpdir) / "global"
        self.local_dir = Path(self.tmpdir) / "local"
        self.global_dir.mkdir()
        self.local_dir.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_load_and_override(self):
        global_rule = (
            "---\n"
            "id: rule1\n"
            "name: Global Rule 1\n"
            "target_globs: ['**/*.py']\n"
            "model_tier: cheap\n"
            "---\n"
            "Global Prompt Content"
        )
        (self.global_dir / "rule1.md").write_text(global_rule)

        global_rule2 = (
            "---\n"
            "id: rule2\n"
            "name: Global Rule 2\n"
            "---\n"
            "Global 2 Content"
        )
        (self.global_dir / "rule2.md").write_text(global_rule2)

        local_rule = (
            "---\n"
            "id: rule1\n"
            "name: Local Rule 1\n"
            "target_globs: ['**/*.rs']\n"
            "model_tier: premium\n"
            "---\n"
            "Local Prompt Content"
        )
        (self.local_dir / "rule1.md").write_text(local_rule)

        loader = RuleLoader(self.global_dir, self.local_dir)
        rules = loader.load_rules()
        self.assertEqual(len(rules), 2)

        rule_dict = {r.id: r for r in rules}
        self.assertIn("rule1", rule_dict)
        self.assertIn("rule2", rule_dict)

        r1 = rule_dict["rule1"]
        self.assertEqual(r1.name, "Local Rule 1")
        self.assertEqual(r1.target_globs, ["**/*.rs"])
        self.assertEqual(r1.model_tier, "premium")
        self.assertEqual(r1.prompt, "Local Prompt Content")

        r2 = rule_dict["rule2"]
        self.assertEqual(r2.name, "Global Rule 2")
        self.assertEqual(r2.prompt, "Global 2 Content")
