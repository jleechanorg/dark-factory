# Operator trust manifest

Pipelines with an `operator_verify` node (default: `pipelines/slim/two_node.dot`)
run privileged commands **outside** the worker sandbox. Policy must not be
worker-writable, so dark-factory pins it from **git history** at run start.

## Where to commit policy

| Path | Role |
|------|------|
| `.github/dark-factory-operator.yaml` | **Preferred.** Dedicated tracked operator policy. Never gitignore this path. |
| `.dark-factory/evidence.yaml` | **Legacy fallback.** Still honored when the canonical file is absent. Often gitignored for scratch artifacts — do not rely on this path in new repos. |

Resolution order is canonical first, then legacy. Both files use the same YAML
shape:

```yaml
operator_verification:
  schema_version: 1
  commands:
    - id: example-check
      argv: ["@runner-python", "-m", "pytest", "-q", "tests/test_syntax.py"]
      lane: worker_safe
      timeout_seconds: 120
      classification: required
  exclusions: []
```

Optional gate-audit filename aliases remain in `.dark-factory/evidence.yaml`
(working tree, not git-pinned) under the top-level `aliases:` key.

## Trust head

At run start the controller resolves `trust_head` (default: upstream `@{u}`),
loads `git show <trust_head>:<manifest-path>`, hashes the policy, and stores the
hash in a private registry the worker cannot mutate. `operator_verify` re-reads
from the same git object and fails closed on drift.

Override for local experiments:

```bash
export DARK_FACTORY_OPERATOR_TRUST_HEAD=<40-char-sha>
```

## Preflight

Before spawning a worker, run:

```bash
dark-factory --pipeline two_node --preflight --workdir /path/to/target/repo
```

When the pipeline includes `operator_verify`, preflight now fails early with
actionable codes:

| Code | Meaning |
|------|---------|
| `DF_OPERATOR_MANIFEST_MISSING_IN_HISTORY` | Neither manifest path exists at `trust_head` — commit `.github/dark-factory-operator.yaml`. |
| `DF_OPERATOR_TRUST_HEAD_UNRESOLVED` | No upstream / invalid `DARK_FACTORY_OPERATOR_TRUST_HEAD`. |
| `DF_OPERATOR_MANIFEST_INVALID` | Manifest present but schema/argv validation failed. |
