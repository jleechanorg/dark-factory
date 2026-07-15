import { dirname, join, parse } from "node:path";
import { pathToFileURL } from "node:url";
import { readFile } from "node:fs/promises";

if (process.env.DARK_FACTORY_AO_V013_BRIDGE === "1") {
  const fail = (message) => {
    console.error(`[dark-factory AO bridge] ${message}`);
    process.exit(1);
  };

  try {
    // This bridge is part of the production dispatch boundary. Never allow
    // inherited environment variables to weaken its runtime contract; test
    // harnesses must execute it with a real Node 22 binary instead.
    if (process.versions.node.split(".")[0] !== "22") {
      fail(`AO bridge requires Node 22, got ${process.versions.node}`);
    }

    const cliEntry = process.argv[1];
    if (!cliEntry) fail("cannot resolve the running AO CLI entrypoint");

    let packageDir = dirname(cliEntry);
    let packageJson;
    while (packageDir !== parse(packageDir).root) {
      try {
        const candidate = JSON.parse(await readFile(join(packageDir, "package.json"), "utf8"));
        if (candidate.name === "@jleechanorg/ao-cli") {
          packageJson = candidate;
          break;
        }
      } catch {
        // Keep walking toward the package root.
      }
      packageDir = dirname(packageDir);
    }
    if (!packageJson) fail("running executable is not @jleechanorg/ao-cli");
    if (packageJson.version !== "0.1.3") {
      fail(`expected @jleechanorg/ao-cli v0.1.3, got ${packageJson.version ?? "unknown"}`);
    }

    const argv = process.argv.slice(2);
    if (argv[0] !== "spawn") fail(`expected spawn command, got ${argv[0] ?? "nothing"}`);
    let project;
    let agent;
    let prompt;
    let positionalOnly = false;
    let diagnosticFlag = false;
    for (let index = 1; index < argv.length; index += 1) {
      const value = argv[index];
      if (!positionalOnly && value === "--") {
        positionalOnly = true;
      } else if (!positionalOnly && value === "--project") {
        project = argv[++index];
      } else if (!positionalOnly && value === "--agent") {
        agent = argv[++index];
      } else if (!positionalOnly && value === "--dark-factory-read-only-diagnostic") {
        diagnosticFlag = true;
      } else if (!positionalOnly && value.startsWith("-")) {
        fail(`unsupported AO v0.1.3 spawn option: ${value}`);
      } else if (prompt === undefined) {
        prompt = value;
      } else {
        fail("AO v0.1.3 spawn bridge received more than one positional prompt");
      }
    }
    if (!project || !agent || prompt === undefined) {
      fail("spawn requires --project, --agent, and one positional prompt");
    }
    const diagnosticMode = process.env.DARK_FACTORY_AO_BRIDGE_DIAGNOSTIC === "1";
    if (diagnosticFlag !== diagnosticMode) {
      fail("read-only diagnostic flag and environment marker must be supplied together");
    }
    const sanitizedPrompt = prompt.replace(/[\r\n]/g, " ").trim();
    if (!sanitizedPrompt) fail("prompt must not be empty after sanitization");
    if (sanitizedPrompt.length > 4096) fail("prompt must be at most 4096 characters");

    const branch = process.env.DARK_FACTORY_AO_SPAWN_BRANCH;
    if (!branch) fail("DARK_FACTORY_AO_SPAWN_BRANCH is required");

    // Resolve AO dependencies with Node's ESM resolver anchored at the
    // running CLI entry. This supports workspace symlinks, hoisted package
    // managers, and ordinary nested installs without guessing node_modules.
    const cliEntryUrl = pathToFileURL(cliEntry).href;
    const resolveAoPackage = (packageName) => import.meta.resolve(packageName, cliEntryUrl);
    const coreUrl = resolveAoPackage("@jleechanorg/ao-core");
    const core = await import(coreUrl);
    for (const api of [
      "loadConfig",
      "createPluginRegistry",
      "createSessionManager",
      "acquireSpawnLock",
      "resolveSpawnQueueConfig",
      "isTerminalSession",
    ]) {
      if (typeof core[api] !== "function") fail(`AO core is missing required public API ${api}`);
    }

    // AO v0.1.3 does not export these command-level guards from its package
    // root. The exact-version check above makes their pinned locations a
    // fail-closed compatibility boundary rather than an unversioned guess.
    const { preflight } = await import(pathToFileURL(join(packageDir, "dist/lib/preflight.js")).href);
    const { getRunning } = await import(
      pathToFileURL(join(packageDir, "dist/lib/running-state.js")).href
    );
    const { ensureLifecycleWorker } = await import(
      pathToFileURL(join(packageDir, "dist/lib/lifecycle-service.js")).href
    );
    if (
      typeof preflight?.checkTmux !== "function" ||
      typeof preflight?.checkGhAuth !== "function" ||
      typeof getRunning !== "function" ||
      typeof ensureLifecycleWorker !== "function"
    ) {
      fail("AO v0.1.3 command preflight/lifecycle API is incompatible");
    }

    const config = core.loadConfig();
    const projectConfig = config.projects[project];
    if (!projectConfig) fail(`unknown AO project ${project}`);
    const registry = core.createPluginRegistry();
    await registry.loadFromConfig(config, async (packageName) =>
      import(resolveAoPackage(packageName)),
    );

    // Read-only startup/deployment diagnostic: verifies the running Node,
    // AO package version, public core API, config, and plugin resolution
    // without creating a workspace or worker.
    if (diagnosticMode) {
      console.log(
        `AO_BRIDGE_DIAGNOSTIC=${JSON.stringify({
          cliVersion: packageJson.version,
          nodeVersion: process.versions.node,
          project,
          agent,
          branch,
          promptLength: sanitizedPrompt.length,
        })}`,
      );
      process.exit(0);
    }

    const runtime = projectConfig.runtime ?? config.defaults?.runtime;
    if (runtime === "tmux") await preflight.checkTmux();
    if (projectConfig.tracker?.plugin === "github") await preflight.checkGhAuth();

    const running = await getRunning();
    if (!running) fail("AO is not running; run `ao start` before factory dispatch");
    if (!running.projects.includes(project)) {
      fail(`running AO instance is not polling project ${project}`);
    }
    await ensureLifecycleWorker(config, project);

    const lock = core.acquireSpawnLock(config.configPath, projectConfig.path ?? "");
    if (!lock.acquired) {
      fail(`another AO spawn is in progress for project ${project}`);
    }

    try {
      const sessions = core.createSessionManager({ config, registry });
      const listed = await sessions.list(project);
      const active = listed.filter((session) => !core.isTerminalSession(session));
      const queue = core.resolveSpawnQueueConfig(projectConfig);
      if (active.length >= queue.maxActiveSessions) {
        if (!queue.enabled) {
          lock.release();
          fail(
            `spawn rejected: ${active.length} active sessions >= cap (${queue.maxActiveSessions})`,
          );
        }
        // AO's v0.1.3 persistent SpawnRequest cannot carry an explicit
        // branch. The daemon overlay is already the durable queue, so return
        // its established REQUEST= deferral signal instead of enqueueing a
        // request that would later spawn on the wrong branch.
        console.log(
          `  Reason: ${active.length} active sessions >= cap ${queue.maxActiveSessions}; exact branch retained by daemon queue`,
        );
        console.log(`REQUEST=dark-factory-exact-branch-${project}`);
        lock.release();
        process.exit(0);
      }

      // The preload is only for this AO process. Restore the parent's Node
      // options and remove bridge-only variables before AO launches the
      // worker so a Node-based agent CLI does not preload this adapter and
      // mistake its own argv for `ao spawn`.
      const parentNodeOptions = process.env.DARK_FACTORY_AO_PARENT_NODE_OPTIONS ?? "";
      if (parentNodeOptions) process.env.NODE_OPTIONS = parentNodeOptions;
      else delete process.env.NODE_OPTIONS;
      delete process.env.DARK_FACTORY_AO_PARENT_NODE_OPTIONS;
      delete process.env.DARK_FACTORY_AO_SPAWN_BRANCH;
      delete process.env.DARK_FACTORY_AO_V013_BRIDGE;
      const session = await sessions.spawn({
        projectId: project,
        agent,
        branch,
        prompt: sanitizedPrompt,
      });
      // Always return the session identity to Rust, even when AO supplies an
      // invalid workspace path. `CliSessions::run_spawn_process` owns the
      // compensating `ao session kill` and its fatal cleanup-failure
      // classification; killing here would hide the session id behind an
      // ordinary nonzero exit and let vendor fallback launch a second worker.
      console.log(`  Worktree: ${session.workspacePath ?? "-"}`);
      console.log(`  Branch:   ${session.branch ?? "-"}`);
      console.log(`SESSION=${session.id}`);
    } finally {
      lock.release();
    }
    process.exit(0);
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
}
