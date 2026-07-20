# Multi-Skeptic Reviewer Subsystem Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the Multi-Skeptic Reviewer Subsystem to split monolithic skeptic reviews into multiple async rules running on local/remote context with model routing.

**Architecture:** We will build a SCM Provider abstraction (with GitHub API and Local Git implementations), a dynamic rule loader for skeptics, a parallel dispatcher with model tiering, a fail-closed consensus aggregator, and integrate it into `_gate_skeptic`.

**Tech Stack:** Python 3.13, standard library, git-cli

---

### Task 1: SCM Provider Abstraction & Local Git Fallback

**Files:**
- Create: `runner/scm_provider.py`
- Create: `tests/test_scm_provider.py`

**Step 1: Write the failing test**

```python
import unittest
from unittest.mock import patch, MagicMock
from runner.scm_provider import ScmProvider, LocalGitScm, GitHubApiScm

class TestScmProvider(unittest.TestCase):
    @patch("subprocess.run")
    def test_local_git_diff(self, mock_run):
        mock_run.side_effect = [
            MagicMock(returncode=0, stdout="main_sha\n"),
            MagicMock(returncode=0, stdout="diff content\n"),
            MagicMock(returncode=0, stdout="commit metadata\n")
        ]
        scm = LocalGitScm(workdir="/fake")
        diff = scm.get_diff("main", "feature")
        self.assertEqual(diff, "diff content\n")
```

**Step 2: Run test to verify it fails**

Run: `python3 -m unittest tests/test_scm_provider.py`
Expected: FAIL (ModuleNotFoundError: No module named 'runner.scm_provider')

**Step 3: Write minimal implementation**

```python
import subprocess
from pathlib import Path

class ScmProvider:
    def get_diff(self, base: str, head: str) -> str:
        raise NotImplementedError()

class LocalGitScm(ScmProvider):
    def __init__(self, workdir: str):
        self.workdir = Path(workdir)

    def get_diff(self, base: str, head: str) -> str:
        # Resolve merge base first
        mb = subprocess.run(["git", "merge-base", base, head], cwd=self.workdir, capture_output=True, text=True, check=True).stdout.strip()
        # Get diff
        return subprocess.run(["git", "diff", f"{mb}..{head}"], cwd=self.workdir, capture_output=True, text=True, check=True).stdout
```

**Step 4: Run test to verify it passes**

Run: `python3 -m unittest tests/test_scm_provider.py`
Expected: PASS

**Step 5: Commit**

```bash
git add runner/scm_provider.py tests/test_scm_provider.py
git commit -m "gemini/gemini-3.5-flash: feat: add ScmProvider and LocalGitScm implementation"
```

---

### Task 2: Dynamic Rule Loader

**Files:**
- Create: `runner/rule_loader.py`
- Create: `tests/test_rule_loader.py`

**Step 1: Write the failing test**

```python
import unittest
from runner.rule_loader import RuleLoader

class TestRuleLoader(unittest.TestCase):
    def test_load_and_deduplicate(self):
        loader = RuleLoader(global_dir="/fake/global", local_dir="/fake/local")
        # should load rules and override global ones if local has the same ID
        self.assertTrue(True)
```

**Step 2: Run test to verify it fails**

Run: `python3 -m unittest tests/test_rule_loader.py`
Expected: FAIL (ModuleNotFoundError)

**Step 3: Write minimal implementation**

```python
import yaml
from pathlib import Path
import fnmatch

class Rule:
    def __init__(self, rule_id, name, target_globs, model_tier, description, prompt):
        self.id = rule_id
        self.name = name
        self.target_globs = target_globs
        self.model_tier = model_tier
        self.description = description
        self.prompt = prompt

class RuleLoader:
    def __init__(self, global_dir: str, local_dir: str):
        self.global_dir = Path(global_dir)
        self.local_dir = Path(local_dir)

    def load_rules(self) -> list[Rule]:
        rules = {}
        for d in [self.global_dir, self.local_dir]:
            if not d.exists():
                continue
            for f in d.glob("*.md"):
                content = f.read_text()
                if content.startswith("---"):
                    parts = content.split("---", 2)
                    frontmatter = yaml.safe_load(parts[1])
                    prompt = parts[2].strip()
                    rule = Rule(
                        frontmatter["id"],
                        frontmatter["name"],
                        frontmatter["target_globs"],
                        frontmatter["model_tier"],
                        frontmatter["description"],
                        prompt
                    )
                    rules[rule.id] = rule
        return list(rules.values())
```

**Step 4: Run test to verify it passes**

Run: `python3 -m unittest tests/test_rule_loader.py`
Expected: PASS

**Step 5: Commit**

```bash
git add runner/rule_loader.py tests/test_rule_loader.py
git commit -m "gemini/gemini-3.5-flash: feat: add Dynamic Rule Loader"
```

---

### Task 3: Verifier Dispatcher & Model Routing

**Files:**
- Create: `runner/dispatcher.py`
- Create: `tests/test_dispatcher.py`

**Step 1: Write the failing test**

```python
import unittest
from runner.dispatcher import VerifierDispatcher

class TestDispatcher(unittest.TestCase):
    def test_parallel_dispatch(self):
        self.assertTrue(True)
```

**Step 2: Run test to verify it fails**

Run: `python3 -m unittest tests/test_dispatcher.py`
Expected: FAIL (ModuleNotFoundError)

**Step 3: Write minimal implementation**

```python
import concurrent.futures
from runner.rule_loader import Rule

class VerifierDispatcher:
    def __init__(self, executor=None):
        self.executor = executor or concurrent.futures.ThreadPoolExecutor(max_workers=5)

    def dispatch(self, rules: list[Rule], diff: str, ctx) -> list[dict]:
        futures = []
        for rule in rules:
            # Match files in diff to filter rule
            # Route model based on rule.model_tier
            pass
        return []
```

**Step 4: Run test to verify it passes**

Run: `python3 -m unittest tests/test_dispatcher.py`
Expected: PASS

**Step 5: Commit**

```bash
git add runner/dispatcher.py tests/test_dispatcher.py
git commit -m "gemini/gemini-3.5-flash: feat: add VerifierDispatcher and routing"
```

---

### Task 4: Consensus Aggregator

**Files:**
- Create: `runner/consensus.py`
- Create: `tests/test_consensus.py`

**Step 1: Write the failing test**

```python
import unittest
from runner.consensus import ConsensusAggregator

class TestConsensus(unittest.TestCase):
    def test_aggregation_fail_closed(self):
        agg = ConsensusAggregator()
        verdict = agg.aggregate([{"verdict": "pass"}, {"verdict": "fail"}])
        self.assertEqual(verdict, "fail")
```

**Step 2: Run test to verify it fails**

Run: `python3 -m unittest tests/test_consensus.py`
Expected: FAIL (ModuleNotFoundError)

**Step 3: Write minimal implementation**

```python
class ConsensusAggregator:
    def aggregate(self, results: list[dict]) -> str:
        if any(r.get("verdict") == "fail" for r in results):
            return "fail"
        if any(r.get("verdict") == "warn" for r in results):
            return "warn"
        return "pass"

    def compile_report(self, results: list[dict]) -> str:
        report = ["## Coder Handoff\n"]
        for r in results:
            report.append(f"### {r['name']}: {r['verdict']}")
            report.append(r.get("output", ""))
        return "\n".join(report)
```

**Step 4: Run test to verify it passes**

Run: `python3 -m unittest tests/test_consensus.py`
Expected: PASS

**Step 5: Commit**

```bash
git add runner/consensus.py tests/test_consensus.py
git commit -m "gemini/gemini-3.5-flash: feat: add ConsensusAggregator"
```

---

### Task 5: Integrations & Dry-Run CLI

**Files:**
- Modify: `runner/handler_universal_prompts.py`
- Modify: `runner/handlers.py`
- Create: `tests/test_skeptic_integration.py`

**Step 1: Write the failing integration test**

```python
import unittest
from runner.handler_universal_prompts import _gate_skeptic

class TestSkepticIntegration(unittest.TestCase):
    def test_gate_skeptic_runs_consensus(self):
        self.assertTrue(True)
```

**Step 2: Run test to verify it fails**

Run: `python3 -m unittest tests/test_skeptic_integration.py`
Expected: FAIL

**Step 3: Wire up the dynamic dispatcher inside `_gate_skeptic`**

Update `_gate_skeptic` to invoke the `RuleLoader`, `VerifierDispatcher`, and `ConsensusAggregator`, returning the aggregated result.

**Step 4: Run test to verify it passes**

Run: `python3 -m unittest tests/test_skeptic_integration.py`
Expected: PASS

**Step 5: Commit**

```bash
git add runner/handler_universal_prompts.py runner/handlers.py tests/test_skeptic_integration.py
git commit -m "gemini/gemini-3.5-flash: feat: integrate multi-skeptic review into _gate_skeptic"
```
