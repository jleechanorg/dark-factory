# systemd drop-in directory — operator-managed

This directory holds **example / template** drop-ins for
`ai.dark-factory.daemon.service`. The runtime drop-ins (containing real
secrets) live at:

```
~/.config/systemd/user/ai.dark-factory.daemon.service.d/
```

and are **never** committed to this repo. They are operator-managed and
machine-specific — same pattern as `~/.config/gh/hosts.yml`, never tracked.

## Why drop-ins (instead of editing the unit template)

- The unit template (`ai.dark-factory.daemon.service.template`) is the
  shared base. Every machine that runs the daemon renders the same template.
- Drop-ins are machine-specific overrides. They survive template re-renders.
- Drop-ins are the only place secrets (API tokens) belong — never inline in
  the unit file, never in a `.env` (see `~/.claude/CLAUDE.md` "Credentials
  and environment").

## Existing drop-ins in production (as of 2026-08-17)

| Drop-in (runtime) | Purpose | Source |
|---|---|---|
| `gemini.conf` | `GEMINI_API_KEY` (kept for explicit Gemini CLI override; Gemini CLI is **not** a default reviewer — `agy` is the Google lane) | operator directive 2026-07-09; superseded as default 2026-08-18 |
| `github.conf` | `UnsetEnvironment=GITHUB_TOKEN` (strips the legacy var) | factory host placement doc |
| `minimax.conf` | `MINIMAX_API_KEY` for the `claudem` skeptic/`/er` lane (`claude --print` + MiniMax env) | operator directive 2026-07-08; reviewer default 2026-08-18 |
| `zz-runtime-recovery.conf` | Runtime-recovery overrides. Required live values (2026-08-18): `DARK_FACTORY_CODER_DEFAULT=agy`, `DARK_FACTORY_REVIEWER_DEFAULT=agy`, `DARK_FACTORY_REVIEWER_FALLBACK_CHAIN=agy->claudem` | factory author |
| **`github-token.conf`** | **`Environment=GH_TOKEN=...` for explicit `gh` auth** | **operator directive 2026-08-17 (this PR)** |

## When to add a new drop-in here

- Adding a new machine-specific `Environment=` or `UnsetEnvironment=`.
- Adding a new secret-bearing key (`*_API_KEY`, `*_TOKEN`).
- The runtime config deviates from what the shared template renders.

When you do, copy the corresponding `.example` file in this directory into
the runtime path with the real value:

```bash
# example: enabling the GH_TOKEN drop-in
cp daemon/systemd/drop-in/github-token.conf.example \
   ~/.config/systemd/user/ai.dark-factory.daemon.service.d/github-token.conf

# edit the token into place (do NOT echo it in shared logs)
${EDITOR:-vi} ~/.config/systemd/user/ai.dark-factory.daemon.service.d/github-token.conf

# activate
systemctl --user daemon-reload
systemctl --user restart ai.dark-factory.daemon.service
```

## Why `github-token.conf` was added (2026-08-17)

The daemon's INTAKE probe polls ~70 PRs at `fast_tick_secs` cadence.
Combined with worker-side `gh` calls (`max_workers=40`), the daemon burned
through GitHub's 5000/hr core and 5000/hr GraphQL rate-limit pools and
started emitting `gh: API rate limit exceeded (HTTP 403)` for the
`jleechanorg/worldarchitect.ai#8958` (and 69 other) PR probes. Two-part fix:

1. **Slow the polls** — `config/daemon.toml` now ships
   `fast_tick_secs = 60` and `slow_tick_secs = 300` (was 10 / 30). Sustained
   INTAKE drops from ~25,200/hr projected to ~1,860/hr, well under the
   5,000/hr budget.
2. **Make `gh` auth explicit** — drop in `GH_TOKEN=<value>` so `gh` reads
   the env var instead of falling back to `~/.config/gh/hosts.yml`. Same
   `jleechan2015` user → same rate-limit pool, but explicit auth makes
   failures deterministic instead of depending on a keyring entry that can
   silently break.

If you ever want a **separate** rate-limit pool for the factory, mint a PAT
under a different GitHub account (or a GitHub App installation) and put it
in this drop-in instead. Until then, all factory auth shares one
`jleechan2015` budget.