# Claude/fable 53-Agent Review vs Codex Plan-Miss Repro

## Status

The git-native `/repro_developer` handoff was pushed to `origin/main` in:

https://github.com/jleechanorg/dark-factory/commit/77494977caa4bf7809415dd3dfb0eaca807cbeb3

That commit added the repo-local command/skill/collector, LFS artifact guard, narrowed LFS rules,
and the sanitized plus encrypted raw replay archives.

## Reproduction Claim

Claude/fable ran a 53-agent adversarial review that confirmed current-state findings but did not
adversarially review the future blocker ordering or self-hosting ratchet proposal. Codex later
caught plan-level hazards from the same available context using parallel subagents.

The replay should let an engineer verify that the missed facts were already present in the Claude
context and that the later Codex pass identified the missing cross-finding hazards.

## Findings to Reproduce

1. `jleechan-qdw` must precede `jleechan-1m4`: systemd `Restart=always` before per-tick
   error isolation/backoff would turn one `gh` 403/timeout into a rate-limit-burning crash loop.
2. The original watchdog metric was gameable: raw `state transitions > 0` can be satisfied by
   HUMAN_HELD->QUEUED->HUMAN_HELD churn or canary-only movement.
3. The daily canary is liveness smoke, not E2E, until it proves a full PR lifecycle with the
   evidence class required by the evidence-standards matrix.
4. Self-hosting needs a promotion gate: 3 consecutive canary successes plus one non-canary bead
   autonomously escaping HUMAN_HELD before low-risk blocker handoff.
5. Oversight components must be write-locked against the factory until it has promotion history:
   watchdog, canary definitions, supervisor units, evidence rules, and verifier prompts.
6. The cutover metric needs a zero-touch ledger and daemon-runner correlation IDs: bead id, branch,
   PR, runner run id, head SHA, and evidence bundle hash.

## Artifact Location

```text
artifacts/repro-developer/claude-fable-adversarial-review-codex-plan-miss/
```

Contained LFS artifacts:

- `claude-fable-adversarial-review-codex-plan-miss-sanitized.tar.zst`
- `claude-fable-adversarial-review-codex-plan-miss.tar.zst.gpg`

The sanitized archive is intended for normal repo access. The encrypted raw archive is the exact
local-state capture; its passphrase is intentionally not in git.

## Replay

```bash
git clone https://github.com/jleechanorg/dark-factory.git
cd dark-factory
git checkout 77494977caa4bf7809415dd3dfb0eaca807cbeb3
git lfs pull
mkdir -p /tmp/df-repro
tar --use-compress-program=zstd \
  -xf artifacts/repro-developer/claude-fable-adversarial-review-codex-plan-miss/claude-fable-adversarial-review-codex-plan-miss-sanitized.tar.zst \
  -C /tmp/df-repro
sed -n '1,220p' /tmp/df-repro/repro-developer-*/REPLAY.md
```

For exact raw replay, get the GPG passphrase out of band and decrypt:

```bash
gpg --batch --yes --decrypt \
  --output /tmp/claude-fable-adversarial-review-codex-plan-miss.tar.zst \
  artifacts/repro-developer/claude-fable-adversarial-review-codex-plan-miss/claude-fable-adversarial-review-codex-plan-miss.tar.zst.gpg
```

## Primary Reading Order

1. `REPLAY.md` from the extracted sanitized bundle.
2. `manifest.json` and `checksums.sha256` from the extracted bundle.
3. `docs/factory-goal-gap-review-2026-07-06.md`.
4. `docs/adversarial-review-miss-retrospective-2026-07-06.md`.
5. `roadmap/nextsteps-2026-07-06-gap-review.md`.
6. Claude parent session and subagent/workflow material under `raw/claude/`.
7. Codex parent and subagent sessions under `raw/codex/`.

## Current Follow-Through

The later takeover work reconciled the durable planning artifacts:

- `roadmap/nextsteps-2026-07-06-gap-review.md` now orders `jleechan-qdw` before `jleechan-1m4`.
- `docs/pr-review-sweep-2026-07-06.md` preserves the limit-interrupted four-PR review sweep and
  points at real follow-up beads.
- `.beads/issues.jsonl` has duplicate `external_ref` values removed and the refined `jleechan-niq`
  and `jleechan-qdw` records merged from the Beads DB.
- `scripts/setup-agent-hooks.sh` now fixes the hook-template rotation documented by
  `docs/setup-agent-hooks-review-2026-07-06.md`.
