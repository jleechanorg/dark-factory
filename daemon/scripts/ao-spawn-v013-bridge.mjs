import { dirname, join, parse } from "node:path";
import { pathToFileURL } from "node:url";
import { readFile } from "node:fs/promises";

if (process.env.DARK_FACTORY_AO_V013_BRIDGE === "1") {
  const fail = (message) => {
    console.error(`[dark-factory AO bridge] ${message}`);
    process.exit(1);
  };

  try {
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
    for (let index = 1; index < argv.length; index += 1) {
      const value = argv[index];
      if (value === "--project") {
        project = argv[++index];
      } else if (value === "--agent") {
        agent = argv[++index];
      } else if (value.startsWith("-")) {
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
    if (prompt.length > 4096) fail("prompt must be at most 4096 characters");

    const branch = process.env.DARK_FACTORY_AO_SPAWN_BRANCH;
    if (!branch) fail("DARK_FACTORY_AO_SPAWN_BRANCH is required");

    // Resolve AO core and every configured plugin from the already-running
    // CLI installation. This avoids hardcoded checkout paths and uses each
    // package's declared ESM export rather than a private source path.
    const resolveAoPackage = async (packageName) => {
      const packagePath = join(packageDir, "node_modules", ...packageName.split("/"));
      const manifest = JSON.parse(await readFile(join(packagePath, "package.json"), "utf8"));
      const rootExport = manifest.exports?.["."];
      const entry =
        (typeof rootExport === "string" ? rootExport : rootExport?.import) ?? manifest.module ?? manifest.main;
      if (!entry) fail(`AO dependency ${packageName} has no ESM root export`);
      return pathToFileURL(join(packagePath, entry)).href;
    };
    const coreUrl = await resolveAoPackage("@jleechanorg/ao-core");
    const core = await import(coreUrl);
    for (const api of ["loadConfig", "createPluginRegistry", "createSessionManager", "acquireSpawnLock"]) {
      if (typeof core[api] !== "function") fail(`AO core is missing required public API ${api}`);
    }

    const config = core.loadConfig();
    const projectConfig = config.projects[project];
    if (!projectConfig) fail(`unknown AO project ${project}`);
    const registry = core.createPluginRegistry();
    await registry.loadFromConfig(config, (packageName) =>
      resolveAoPackage(packageName).then((url) => import(url)),
    );

    // Read-only startup/deployment diagnostic: verifies the running Node,
    // AO package version, public core API, config, and plugin resolution
    // without creating a workspace or worker.
    if (process.env.DARK_FACTORY_AO_BRIDGE_DIAGNOSTIC === "1") {
      console.log(
        `AO_BRIDGE_DIAGNOSTIC=${JSON.stringify({
          cliVersion: packageJson.version,
          nodeVersion: process.versions.node,
          project,
          agent,
          branch,
          promptLength: prompt.length,
        })}`,
      );
      process.exit(0);
    }

    const lock = core.acquireSpawnLock(config.configPath, projectConfig.path ?? "");
    if (!lock.acquired) {
      fail(`another AO spawn is in progress for project ${project}`);
    }

    try {
      const sessions = core.createSessionManager({ config, registry });
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
        prompt,
      });
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
