#!/usr/bin/env node

const { readFileSync: defaultReadFileSync } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync: defaultSpawnSync } = require("node:child_process");

const root = join(__dirname, "..");

function createTauriEnvironment({
  platform = process.platform,
  environment = process.env,
  readFileSync = defaultReadFileSync,
  projectRoot = root,
} = {}) {
  const pathEntries = [
    join(projectRoot, "node_modules", ".bin"),
    environment.HOME ? join(environment.HOME, ".cargo", "bin") : null,
    environment.PATH,
  ].filter(Boolean);
  const buildEnvironment = {
    ...environment,
    PATH: pathEntries.join(delimiter),
  };

  if (platform === "darwin") {
    const tauriConfig = JSON.parse(
      readFileSync(join(projectRoot, "src-tauri", "tauri.conf.json"), "utf8"),
    );
    const minimumSystemVersion =
      tauriConfig.bundle?.macOS?.minimumSystemVersion;

    if (!minimumSystemVersion) {
      throw new Error(
        "src-tauri/tauri.conf.json must define bundle.macOS.minimumSystemVersion.",
      );
    }

    buildEnvironment.CMAKE_OSX_DEPLOYMENT_TARGET = minimumSystemVersion;
  }

  return buildEnvironment;
}

function runTauri({
  args = process.argv.slice(2),
  platform = process.platform,
  environment = process.env,
  readFileSync = defaultReadFileSync,
  spawnSync = defaultSpawnSync,
  projectRoot = root,
} = {}) {
  const command = platform === "win32" ? "tauri.cmd" : "tauri";
  const result = spawnSync(command, args, {
    cwd: projectRoot,
    env: createTauriEnvironment({
      platform,
      environment,
      readFileSync,
      projectRoot,
    }),
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    throw new Error(`${command} could not start: ${result.error.message}`);
  }

  if (result.status !== 0) {
    throw new Error(
      `${[command, ...args].join(" ")} exited with status ${result.status ?? "unknown"}`,
    );
  }

  return result;
}

if (require.main === module) {
  try {
    runTauri();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}

module.exports = {
  createTauriEnvironment,
  runTauri,
};
