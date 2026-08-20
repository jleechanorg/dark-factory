# Generic `/af` Host Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make tracked `/auto-factory` behavior host-neutral while routing this user's `/af` through a sub-second local probe and `/linux` fallback.

**Architecture:** The Dark Factory repository remains the single source for factory semantics. User-scoped command files contain only machine selection and a pointer to the tracked command. Intake becomes two-phase so only a Bead already readable by the selected factory receives the `factory` label.

**Tech Stack:** Markdown slash commands and skills, Bash probe commands, Python pytest contract tests, Beads `br` CLI.

## Global Constraints

- Do not hardcode a personal hostname, SSH alias, operating system, or preferred machine in the tracked factory skill.
- Keep the user-scoped router bounded to a one-second local probe.
- Create and read back a Bead before adding the `factory` label.
- Report `QUEUED` only after overlay adoption, not merely after label verification.
- User-scoped commands must point to `~/projects/dark-factory/.claude/commands/auto-factory.md`; they must not copy factory workflow prose.
- Preserve unrelated `.factory-beads/` and other local changes.

---

### Task 1: Pin the generic tracked contract with failing tests

**Files:**
- Modify: `tests/test_af_gate_contract.py`

**Interfaces:**
- Consumes: tracked `.claude/skills/auto-factory/SKILL.md` and `.claude/commands/auto-factory.md` text.
- Produces: regression assertions for host neutrality and ordered two-phase intake.

- [ ] **Step 1: Add failing host-neutrality assertions**

Add assertions that the tracked skill contains none of `jeff-ubuntu`, `Every /af run operates through SSH`, or `Production /af execution is Linux-only`, and that it states the invocation host is the candidate factory host.

- [ ] **Step 2: Add failing two-phase intake assertions**

Require ordered markers for `create without the factory label`, same-store `br show`, `br update --add-label factory`, and overlay adoption. Assert their text offsets occur in that order.

- [ ] **Step 3: Run the focused test and verify RED**

Run:

```bash
/Users/jleechan/projects/dark-factory/.venv/bin/python -m pytest \
  tests/test_af_gate_contract.py -q
```

Expected: the new generic-host and two-phase-intake assertions fail against the Linux-hardcoded skill.

### Task 2: Make the tracked workflow host-local and two-phase

**Files:**
- Modify: `.claude/skills/auto-factory/SKILL.md`
- Modify: `.claude/commands/auto-factory.md`
- Modify: `.claude/commands/af.md`

**Interfaces:**
- Consumes: `target_repo`, a local factory service/configuration, its exact Beads DB, and `br`.
- Produces: an unlabelled verified Bead followed by a separately verified `factory` label and overlay adoption.

- [ ] **Step 1: Replace personal host routing with a current-host capability contract**

State that the invocation host is the only candidate inside the tracked workflow. Require a live local factory, `target_repo` support in its configuration, exact DB resolution, and healthy `br sync --status --json` plus `br doctor --quick`. Tell callers to route to another host before invoking the tracked command when the local probe fails.

- [ ] **Step 2: Replace direct labelled creation with two-phase intake**

Document these exact operations against one explicit `BR_DB`:

```bash
bead_id="$(br --db "$BR_DB" create "<title>" --body "<body>" --json \
  | jq -r '.id')"
br --db "$BR_DB" show "$bead_id" --json
br --db "$BR_DB" update "$bead_id" --add-label factory --json
br --db "$BR_DB" show "$bead_id" --json
```

Existing Beads follow the same rule: verify first, then add `factory`. GitHub fallback creation must also omit `factory` until read-back succeeds.

- [ ] **Step 3: Separate label verification from queue verification**

Require the overlay harness to show the Bead in `QUEUED` before using that word. Otherwise report `intake verified; adoption pending`.

- [ ] **Step 4: Keep tracked command files as thin aliases**

Remove personal Linux wording from the tracked commands. Both tracked aliases should invoke the canonical skill on the current host and contain no user-specific fallback.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
/Users/jleechan/projects/dark-factory/.venv/bin/python -m pytest \
  tests/test_af_gate_contract.py tests/test_slash_command_skill_dispatch.py -q
git diff --check
```

Expected: all tests pass and no whitespace errors are reported.

- [ ] **Step 6: Commit the tracked workflow unit**

```bash
git add .claude/skills/auto-factory/SKILL.md \
  .claude/commands/auto-factory.md .claude/commands/af.md \
  tests/test_af_gate_contract.py
git commit -m "codex/gpt-5.6-sol: fix(af): make host routing generic"
```

### Task 3: Install thin user-scoped routers

**Files:**
- Replace symlink with regular file: `/Users/jleechan/.claude/commands/af.md`
- Replace symlink with regular file: `/Users/jleechan/.claude/commands/auto-factory.md`

**Interfaces:**
- Consumes: local service-manager state, the existing `/linux` command, and the tracked command at `/Users/jleechan/projects/dark-factory/.claude/commands/auto-factory.md`.
- Produces: local execution when a capable factory exists; `/linux` execution otherwise.

- [ ] **Step 1: Capture and validate the exact symlink targets**

Run `readlink` for both files and require both targets to be inside `/Users/jleechan/projects/dark-factory/.claude/commands/` before replacing them.

- [ ] **Step 2: Replace `auto-factory.md` with the pointer router**

The regular file must instruct the agent to spend no more than one second checking for a live local factory service and configured DB. On success, read and execute the tracked `auto-factory.md`. On failure, follow `/Users/jleechan/.claude/commands/linux.md` and execute that same tracked command on Linux. It must not reproduce intake, routing, dispatch, or gate instructions.

- [ ] **Step 3: Replace `af.md` with an alias pointer**

The regular file must contain only command metadata and an instruction to execute `/Users/jleechan/.claude/commands/auto-factory.md` with the original arguments.

- [ ] **Step 4: Verify pointer shape and fast fallback text**

Run:

```bash
test ! -L /Users/jleechan/.claude/commands/af.md
test ! -L /Users/jleechan/.claude/commands/auto-factory.md
rg -n "projects/dark-factory/.claude/commands/auto-factory.md|one second|commands/linux.md" \
  /Users/jleechan/.claude/commands/af.md \
  /Users/jleechan/.claude/commands/auto-factory.md
```

Expected: both are regular files, the router points to the tracked command and `/linux`, and the alias points only to the router.

### Task 4: Review, publish, and verify the routing boundary

**Files:**
- Verify all files from Tasks 1-3.

**Interfaces:**
- Consumes: committed tracked change and installed user-scoped pointer files.
- Produces: reviewed PR plus live evidence that Mac fallback reaches a Linux-local factory without creating a Mac-local Bead.

- [ ] **Step 1: Run independent semantic review**

Review host neutrality, one-second routing, exact-store ownership, two-phase labeling, and the distinction between label presence and overlay adoption. Fix every blocker and rerun the focused tests.

- [ ] **Step 2: Push and open the Dark Factory PR**

Push `fix/af-generic-host-router`, open a PR against `main`, and record the exact head SHA.

- [ ] **Step 3: Wait for exact-head CI and merge**

Require all Dark Factory checks to complete successfully before merging. Verify the resulting `origin/main` merge SHA.

- [ ] **Step 4: Refresh the tracked local checkout**

Fast-forward `/Users/jleechan/projects/dark-factory` without touching its unrelated `.factory-beads/` directory. Verify both user-scoped pointer files still resolve the merged tracked command.

- [ ] **Step 5: Exercise the fast fallback read-only**

From the Mac, run the user router's local probe without creating a Bead. Confirm it selects `/linux`. On Linux, confirm the local factory service, supported repository map, and exact DB are visible. Do not add a label or run a tick during this routing-only proof.
