# Factory mission admission

The optional `[mission_admission]` table in `config/daemon.toml` selects the union
of exact bead IDs and canonical GitHub issue/PR references. Omit the table for
legacy unrestricted operation. An explicit empty table admits nothing. Invalid
IDs, references, types, or unknown keys inside this table reject configuration.
GitHub HTTPS pull/issues URLs normalize to owner/repo#number; matching is exact.

Example: four existing PRs plus explicitly named repair beads:

```toml
[mission_admission]
bead_ids = ["dark-factory-fsy7", "dark-factory-9mrs", "dark-factory-pr683-af-ready-2dus", "dark-factory-pr642-af-ready-v0ft"]
external_refs = ["jleechanorg/dark-factory#676", "jleechanorg/dark-factory#685", "jleechanorg/dark-factory#683", "jleechanorg/dark-factory#642"]
```

Append actual repair bead IDs to `bead_ids`. Select unlabeled upstream intake by
external reference: the tracker candidate snapshot only contains labeled beads,
so an unlabeled bead's ID is not available before its external reference is
admitted. No GitHub labels need changing. Excluded source items cannot reach the
adapter's create/duplicate/relabel operation. Existing queued beads are also
checked before dispatch. Existing active-worker monitoring continues unchanged.
Configuration is loaded at startup; changing this file requires the normal
Linux systemd deployment/restart procedure. This is an admission policy, not a
worker cancellation operation.
