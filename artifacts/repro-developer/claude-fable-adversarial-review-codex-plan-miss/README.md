# Repro Developer Artifact: claude-fable-adversarial-review-codex-plan-miss

This directory is a git-native repro handoff for an agent-review failure.

## Artifacts

- `claude-fable-adversarial-review-codex-plan-miss-sanitized.tar.zst`: gitleaks-clean sanitized repro for normal repo access.
- `claude-fable-adversarial-review-codex-plan-miss.tar.zst.gpg`: exact raw repro archive encrypted with GPG symmetric encryption.
- Do not commit the passphrase file. Share the passphrase only out of band with the intended engineer.

## Reproduce

```bash
git clone https://github.com/jleechanorg/dark-factory.git
cd dark-factory
git checkout main  # or the specific commit containing this artifact directory
git lfs pull
mkdir -p /tmp/df-repro
tar --use-compress-program=zstd -xf artifacts/repro-developer/claude-fable-adversarial-review-codex-plan-miss/claude-fable-adversarial-review-codex-plan-miss-sanitized.tar.zst -C /tmp/df-repro
sed -n '1,220p' /tmp/df-repro/repro-developer-*/REPLAY.md
```

For exact raw state, get the GPG passphrase out of band and decrypt the `.gpg` archive.
