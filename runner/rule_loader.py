from __future__ import annotations
import yaml
from pathlib import Path
from typing import List

class Rule:
    def __init__(self, rule_id: str, name: str, target_globs: List[str], model_tier: str, description: str, prompt: str):
        self.id = rule_id
        self.name = name
        self.target_globs = target_globs
        self.model_tier = model_tier
        self.description = description
        self.prompt = prompt

class RuleLoader:
    def __init__(self, global_dir: str | Path, local_dir: str | Path):
        self.global_dir = Path(global_dir)
        self.local_dir = Path(local_dir)

    def load_rules(self) -> List[Rule]:
        rules: dict[str, Rule] = {}
        for d in [self.global_dir, self.local_dir]:
            if not d.exists():
                continue
            for f in d.glob("*.md"):
                try:
                    content = f.read_text()
                    if content.startswith("---"):
                        parts = content.split("---", 2)
                        if len(parts) >= 3:
                            frontmatter = yaml.safe_load(parts[1])
                            prompt = parts[2].strip()
                            rule_id = frontmatter.get("id")
                            if not rule_id:
                                continue
                            rule = Rule(
                                rule_id=rule_id,
                                name=frontmatter.get("name", f.stem),
                                target_globs=frontmatter.get("target_globs", ["*"]),
                                model_tier=frontmatter.get("model_tier", "cheap"),
                                description=frontmatter.get("description", ""),
                                prompt=prompt
                            )
                            rules[rule_id] = rule
                except Exception:
                    continue
        return list(rules.values())
