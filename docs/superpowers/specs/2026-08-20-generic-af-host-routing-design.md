# Generic `/af` Host Routing Design

## Goal

Keep Dark Factory's tracked `/auto-factory` workflow host-neutral while giving
this user's `/af` command a fast local-first fallback to `/linux`. A Bead must
not receive the `factory` label until a specific healthy factory can read it.

## Ownership boundaries

The tracked Dark Factory command and skill own factory semantics: capability
validation, Bead-store health, two-phase intake, routing, dispatch, and gate
verification. They must not name a personal hostname, SSH alias, or preferred
machine.

The user-scoped commands `~/.claude/commands/af.md` and
`~/.claude/commands/auto-factory.md` own only machine selection. They are
regular pointer files, not symlinks or copies: after selecting an execution
machine, they read and execute the tracked command in
`~/projects/dark-factory/.claude/commands/auto-factory.md`. This keeps the
personal fallback from duplicating the factory workflow.

## Fast host selection

The user-scoped router uses a bounded local capability probe with a one-second
budget. The probe checks only local service-manager state and the configured
Beads DB; it does not scan the network or run a factory tick.

- If a healthy local factory exists and supports `target_repo`, run the tracked
  command locally.
- Otherwise, execute the request through the existing `/linux` contract. On
  Linux, the tracked command probes and uses that machine's local factory.
- If `/linux` is unreachable or its local factory cannot handle `target_repo`,
  fail closed without creating or labeling a Bead.

The repository workflow treats the invocation machine as the candidate factory
host. Platform-specific service discovery is an adapter detail: systemd and
launchd are allowed examples, but neither an operating system nor a hostname is
the semantic authority. The authoritative values are a live factory instance,
its configured repository map, and its exact Beads DB.

## Two-phase factory intake

After selecting a capable factory:

1. Resolve and health-check that factory's exact Beads DB.
2. Create the Bead with routing metadata but without the `factory` label.
3. Read the new Bead back through the same factory host and DB.
4. Add the `factory` label through that same host and DB.
5. Verify the label, then verify overlay adoption before reporting `QUEUED`.

If any step fails, stop. A Bead created before a labeling failure remains
unlabelled and therefore cannot be dispatched accidentally. Retries use an
external reference when one exists; otherwise they retain and update the
already-created Bead ID rather than creating another item.

## Failure behavior

- No capable local factory and `/linux` unavailable: no Bead is created.
- Factory DB unhealthy or ambiguous: no Bead is created or labeled.
- Read-back fails after creation: leave the Bead unlabelled and report its ID.
- Label verification fails: leave the Bead unlabelled and report the failure.
- Overlay adoption is pending: report `intake verified; adoption pending`, not
  `QUEUED`.

## Verification

Repository contract tests must prove that the tracked skill:

- contains no personal hostname or mandatory Linux-only rule;
- requires capability and exact-DB health before mutation;
- creates before adding the `factory` label;
- verifies same-host readability before labeling;
- distinguishes label presence from overlay `QUEUED` adoption.

User-scope verification must prove that both command files are regular files,
contain only the bounded host-selection logic plus a pointer to the tracked
command, use local execution when a factory probe succeeds, and route through
`/linux` when it fails.
