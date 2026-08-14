import { dirname, join, parse, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { readFile } from "node:fs/promises";
import { realpathSync, mkdirSync, rmSync, existsSync, appendFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { homedir } from "node:os";

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
    const expectedRevision = process.env.DARK_FACTORY_AO_EXPECTED_REVISION;

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
    let projectConfig = config.projects[project];
    if (!projectConfig) fail(`unknown AO project ${project}`);
    if (!diagnosticMode && expectedRevision) {
      const targetCheckout = process.env.DARK_FACTORY_AO_TARGET_CHECKOUT;
      if (!targetCheckout || !targetCheckout.startsWith("/")) {
        fail("DARK_FACTORY_AO_TARGET_CHECKOUT is required for worker spawns");
      }
      const configuredSource = projectConfig.path;
      if (typeof configuredSource !== "string" || !configuredSource.startsWith("/")) {
        fail(`AO project ${project} has no absolute source path`);
      }
      let configuredRealpath;
      let targetRealpath;
      try {
        configuredRealpath = realpathSync(configuredSource);
        targetRealpath = realpathSync(targetCheckout);
      } catch (error) {
        fail(`cannot resolve AO project source and target checkout: ${error}`);
      }
      const managedTargetCheckout =
        process.env.DARK_FACTORY_AO_MANAGED_CHECKOUT === "1";
      if (configuredRealpath !== targetRealpath && !managedTargetCheckout) {
        fail(
          `AO project ${project} source ${configuredRealpath} does not match validated target checkout ${targetRealpath}`,
        );
      }
      let actualRevision;
      try {
        actualRevision = execFileSync(
          "git",
          ["-C", targetRealpath, "rev-parse", "HEAD"],
          { encoding: "utf8" },
        ).trim();
      } catch (error) {
        fail(`cannot resolve validated target checkout revision: ${error}`);
      }
      if (actualRevision !== expectedRevision) {
        fail(
          `AO project ${project} source is at ${actualRevision}, expected validated revision ${expectedRevision}`,
        );
      }
      if (managedTargetCheckout) {
        // The Rust adapter already validated this daemon-owned checkout and
        // revision.  AO's project registry is still allowed to retain a
        // user-facing source path; for this spawn the validated target is the
        // authoritative project root so AO creates the worker in that repo.
        projectConfig = { ...projectConfig, path: targetRealpath };
        config.projects = { ...config.projects, [project]: projectConfig };
      }
    }
    const registry = core.createPluginRegistry();
    await registry.loadFromConfig(config, async (packageName) =>
      import(resolveAoPackage(packageName)),
    );

    const wrapWorkspacePlugin = (originalWorkspace, expectedRev, aoConfig) => ({
      ...originalWorkspace,

      async findManagedWorkspace(workspaceConfig) {
        if (typeof originalWorkspace?.findManagedWorkspace === "function") {
          const found = await originalWorkspace.findManagedWorkspace(workspaceConfig);
          if (found && found.path) {
            try {
              const headSha = execFileSync(
                "git",
                ["-C", found.path, "rev-parse", "HEAD"],
                { encoding: "utf8" }
              ).trim();
              if (headSha === expectedRev) {
                return found;
              }
            } catch {
              // Treat unresolvable HEAD as stale
            }
            try {
              if (typeof originalWorkspace.destroy === "function") {
                await originalWorkspace.destroy(
                  found.path,
                  found.repoPath ?? workspaceConfig.project?.path
                );
              }
            } catch {
              // Best-effort cleanup
            }
          }
        }
        return null;
      },

      async create(workspaceConfig) {
        const { projectId, project: pConfig, sessionId, branch: targetBranch } = workspaceConfig;
        const repoPath = pConfig?.path;
        if (!repoPath) {
          throw new Error(`Project ${projectId} is missing path in workspace config`);
        }

        const worktreeBaseDir =
          workspaceConfig.worktreeDir ??
          aoConfig?.plugins?.["workspace-worktree"]?.worktreeDir ??
          aoConfig?.defaults?.worktreeDir ??
          join(homedir(), ".worktrees");

        const expandedBaseDir = worktreeBaseDir.startsWith("~/")
          ? join(homedir(), worktreeBaseDir.slice(2))
          : worktreeBaseDir;

        const projectWorktreeDir = join(expandedBaseDir, projectId);
        const worktreePath = join(projectWorktreeDir, sessionId);

        mkdirSync(projectWorktreeDir, { recursive: true });

        // Clean up stale worktree / directory at worktreePath if present
        try {
          execFileSync(
            "git",
            ["-C", repoPath, "worktree", "remove", "--force", "--force", worktreePath],
            { stdio: "ignore" }
          );
        } catch {}
        try {
          execFileSync(
            "git",
            ["-C", repoPath, "worktree", "unlock", worktreePath],
            { stdio: "ignore" }
          );
        } catch {}
        if (existsSync(worktreePath)) {
          try {
            rmSync(worktreePath, { recursive: true, force: true });
          } catch {}
        }
        try {
          execFileSync("git", ["-C", repoPath, "worktree", "prune"], {
            stdio: "ignore",
          });
        } catch {}

        // Clean up stale worktree if the branch is checked out elsewhere
        try {
          const listOutput = execFileSync(
            "git",
            ["-C", repoPath, "worktree", "list", "--porcelain"],
            { encoding: "utf8" }
          );
          const entries = listOutput.split("\n\n").map((block) => {
            let p = "";
            let b = "";
            for (const line of block.trim().split("\n")) {
              if (line.startsWith("worktree ")) p = resolve(line.slice("worktree ".length).trim());
              if (line.startsWith("branch ")) b = line.slice("branch ".length).replace("refs/heads/", "").trim();
            }
            return { path: p, branch: b };
          });
          const normalizedWorktreePath = resolve(worktreePath);
          for (const entry of entries) {
            if (entry.branch === targetBranch && entry.path && entry.path !== normalizedWorktreePath) {
              try {
                execFileSync(
                  "git",
                  ["-C", repoPath, "worktree", "remove", "--force", "--force", entry.path],
                  { stdio: "ignore" }
                );
              } catch {}
            }
          }
        } catch {}

        // Create worktree on expectedRevision using -B (force-sets/resets branch to expectedRevision)
        execFileSync(
          "git",
          ["-C", repoPath, "worktree", "add", "-B", targetBranch, worktreePath, expectedRev],
          { encoding: "utf8" }
        );

        // Set up AO-managed exclude patterns in worktree
        try {
          let gitCommonDir;
          try {
            gitCommonDir = execFileSync(
              "git",
              ["-C", worktreePath, "rev-parse", "--path-format=absolute", "--git-common-dir"],
              { encoding: "utf8" }
            ).trim();
          } catch {
            const dotGit = join(worktreePath, ".git");
            gitCommonDir = dotGit;
          }
          const excludeDir = join(gitCommonDir, "info");
          const excludeFile = join(excludeDir, "exclude");
          mkdirSync(excludeDir, { recursive: true });
          let existingContent = "";
          try {
            existingContent = await readFile(excludeFile, "utf8");
          } catch {}
          if (!existingContent.includes("# AO-managed files")) {
            const patterns = `# AO-managed files - do not track in worktree\n.claude/settings.json\n.claude/metadata-updater.sh\n.cursor/settings.json\n.cursor/metadata-updater.sh\n.gemini/settings.json\n.gemini/metadata-updater.sh\n`;
            appendFileSync(excludeFile, (existingContent ? "\n\n" : "") + patterns);
          }
        } catch {}

        // Lock worktree
        try {
          execFileSync(
            "git",
            ["-C", repoPath, "worktree", "lock", "--reason", "AO session active", worktreePath],
            { stdio: "ignore" }
          );
        } catch {}

        return {
          path: worktreePath,
          branch: targetBranch,
          sessionId,
          projectId,
          repoPath,
        };
      },
    });

    if (expectedRevision && !diagnosticMode && typeof registry.get === "function") {
      const originalGet = registry.get.bind(registry);
      registry.get = (slot, name) => {
        const plugin = originalGet(slot, name);
        if (slot === "workspace" && plugin) {
          return wrapWorkspacePlugin(plugin, expectedRevision, config);
        }
        return plugin;
      };
    }

    // Enumerate installed agent plugins for the daemon's fail-closed vendor
    // preflight. Three mutually-exclusive outcomes — the daemon rejects all
    // three, but with distinct messages so triage can tell "registry
    // reachable but empty" from "registry reachable, list threw" from
    // "registry reachable, list was empty even though loadFromConfig just
    // succeeded" (which is a separate state we surface as the empty-list
    // key rather than the error key).
    let agentPluginsPayload;
    try {
      const listResult = registry.list("agent");
      if (Array.isArray(listResult)) {
        const names = listResult
          .map((entry) => (typeof entry === "string" ? entry : entry?.name))
          .filter((name) => typeof name === "string" && name.length > 0);
        agentPluginsPayload = { agentPlugins: [...new Set(names)].sort() };
      } else {
        agentPluginsPayload = {
          agentPluginsError:
            "AO registry.list returned a non-array value for kind 'agent'",
        };
      }
    } catch (registryError) {
      agentPluginsPayload = {
        agentPluginsError: `AO registry.list threw: ${
          registryError instanceof Error ? registryError.message : String(registryError)
        }`,
      };
    }

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
          ...agentPluginsPayload,
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
      delete process.env.DARK_FACTORY_AO_TARGET_CHECKOUT;
      delete process.env.DARK_FACTORY_AO_EXPECTED_REVISION;
      delete process.env.DARK_FACTORY_AO_MANAGED_CHECKOUT;
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
