# P0 prerequisite assessment — first READY canary

target_repo: jleechanorg/dark-factory
Target bead: dark-factory-c4zhq
Role: read-only administrative assessment; no factory label and no coding authority.
Date: 2026-09-13

## Decision

STATUS: BLOCKED. This report collects facts; it does not certify P0 or READY.
Downstream TEST and IMPL admission remains blocked until an independent reviewer
reproduces both (a) compliant scope validation at every AI child launch edge and
(b) a supported Go AO dispatch path. Do not turn source inspection, service
presence, `ao status`, or a worker's claim into a positive P0 boolean.

## Exact read-only collection commands

Run from `/Users/jleechan/projects/worktree_factory_review` for source checks.
Use SSH for Linux; the Mac is only the operator client. These commands do not
read `.env` files, process environments, credential stores, or secret values.

```bash
git status --short
nl -ba daemon/src/adapters.rs | sed -n '3131,3141p;3272,3330p;3950,4071p;4428,4455p;4789,4795p;10649,10765p'
nl -ba daemon/src/tools.rs | sed -n '760,780p;1189,1212p;1239,1285p'
nl -ba daemon/scripts/ao-spawn-v013-bridge.mjs | sed -n '8,100p'
rg -n 'ao-go|agent-orchestrator-ts|Command::new\("ao"\)|NODE_OPTIONS|run_tool_in_dir|Command::new\("codex"\)|Command::new\("agy"\)' daemon/src daemon/scripts
```

The exact live Linux probes are:

```bash
ssh jeff-ubuntu 'command -v ao; command -v ao-go; ao --version; ao-go --version'
ssh jeff-ubuntu 'systemctl --user status ai.dark-factory.ao.service --no-pager'
ssh jeff-ubuntu 'systemctl --user status ao-daemon.service --no-pager'
ssh jeff-ubuntu 'ao --help; ao status --help; ao status -p dark-factory --json'
ssh jeff-ubuntu 'ao-go --help; ao-go status --help; ao-go spawn --help'
ssh jeff-ubuntu 'ao-go project get dark-factory --json'
ssh jeff-ubuntu 'ao-go session ls -p dark-factory --json'
ssh jeff-ubuntu 'systemctl --user show ai.dark-factory.daemon.service -p ExecStart --value'
```

Retain command output with credential values and unrelated notifier text
redacted. Do not run an unscoped `ao status`, `ao session ls`, or process-env
dump. Do not invoke `spawn`, `start`, `stop`, `kill`, `restore`, `project add`,
or any service mutation.

## Facts established by this assessment

The Rust spawn path calls `Command::new("ao")` in `ao_spawn_command_with_mode`
(`daemon/src/adapters.rs:3932-3964`). It installs `NODE_OPTIONS` with the
repository's `scripts/ao-spawn-v013-bridge.mjs` and sends the v0.1.3 argv shape
`ao spawn --project <project> --agent <agent> -- <prompt>`
(`adapters.rs:4026-4063`). The bridge rejects a CLI whose package is not
`@jleechanorg/ao-cli` version `0.1.3` (`daemon/scripts/ao-spawn-v013-bridge.mjs:25-41`)
and checks the Node/AO public APIs before dispatch (`:83-98`). Therefore the
current factory source invokes the Node/TS bridge boundary.

The Linux host has `/home/jleechan/.local/bin/ao` reporting `0.1.3` and
`/home/jleechan/.local/bin/ao-go` reporting `dev`. Both
`ai.dark-factory.ao.service` and `ao-daemon.service` are active with an
`ao-go daemon` main process. `ao-go project get dark-factory --json` returns a
registered `dark-factory` project, while the project-scoped
`ao-go session ls -p dark-factory --json` returned an empty session list.
`ao status -p dark-factory --json` returned an empty TS-side session list.
These are independent service/CLI facts; they do not prove that the Rust
factory dispatches through the Go service.

The live daemon `ExecStart` is the release path
`/home/jleechan/.local/share/dark-factory/releases/8316b204b9871c70b8e9ecbc9647b8d9579c6d62/daemon/target/release/daemon`
(PID `3091590` at collection). The source search found no `ao-go` command
invocation in `daemon/src`. The supported `Sessions` interface in
`daemon/src/tools.rs:764-780` is abstract, but the production implementation
in `CliSessions::run_spawn_process` (`adapters.rs:4559-4660`) reaches the `ao`
command above. The Go CLI advertises
`ao spawn --project ... --harness ... --prompt ...` and project-scoped
`ao session ls -p ...`, but no adapter mapping from `SpawnSpec` to that Go
interface was established here. Do not invent one.

The account-scope source is also not sufficient as proof. `ao_controller_env`
overlays `HOME`, `AO_CONFIG_PATH`, and `CLAUDE_CONFIG_DIR`
(`adapters.rs:3272-3313`), while `ChainLlm::judge` directly launches `codex`,
Claude, MiniMax, and `agy` (`adapters.rs:10666-10755`). MiniMax sets its endpoint
and API key for that child, but a pre-launch validation result and invalid-scope
zero-child proof are not present in these call sites. `run_tool_in_dir` and
`run_tool_with_env` (`tools.rs:1189-1211`) provide subprocess plumbing, not an
account-boundary attestation. Presence of an environment variable is not scope
validation.

## Stop reasons and owners

1. The current Rust path is demonstrably TS-bridge based; a Go service being
   active is not Go adapter proof. The bounded Go interface mapping and a real,
   project-scoped dispatch reproduction are missing.
2. The four required launch edges (router, coder, reviewer, fallback) lack an
   independent transcript proving intended provider/account validation happened
   before each child was created and that invalid scope created zero children.
3. The AGY attempt timed out without a report, and Spark quota plus `claudem`
   failure do not establish a compliant bootstrap lane. Preserve existing repair
   ownership: `i92jy` owns account scope and `btlc0` owns the harness.

### Zero-op rule

If a compliant bootstrap cannot be reproduced for both the Go adapter and every
launch edge, perform zero product/factory coding, service changes, branch changes or canary intake.
Continue read-only diagnosis and record sourced blockers through br and roadmap. Record the exact failed edge, command, binary identity, exit/error
text with secrets removed, and the missing proof. Do not guess a bootstrap,
silently fall back to TS, or dispatch TEST/IMPL as a coding fallback. If the
active factory cannot read and sequence the full administrative contracts within
its prompt/body cap, stop admission without coding fallback.

## Next exact bounded information required

An independent factory reviewer must, in one fresh transcript:

1. Pin the live daemon `ExecStart`, release SHA, and target project, then show
   the source symbol or committed adapter that invokes the supported Go CLI/API.
2. For router, coder, reviewer, and fallback, run the existing `i92jy` account
   repair path and capture redacted argv, source symbol/SHA, child identity,
   `validated_before_spawn=true`, and a separate invalid-scope test showing
   `children_created=0`; booleans without raw execution are invalid.
3. Preserve `btlc0` harness ownership and report the exact Go command/API,
   project identifier, and successful project-scoped session/worktree receipt.
4. If any item fails, write `bootstrap_supported=false` plus the exact blocker;
   P0 remains FAIL and TEST/IMPL remains inadmissible. Only after all items are
   independently reproduced may the P0 contract and later canary steps run.
