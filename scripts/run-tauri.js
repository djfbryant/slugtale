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

function resolveRuntimeFeatures({
  platform = process.platform,
  environment = process.env,
} = {}) {
  const baseline =
    platform === "darwin"
      ? [
          "local-whisper-runtime",
          "local-whisper-runtime-metal",
          "voice-activation",
          "local-phonon-mlx",
        ]
      : ["local-whisper-runtime"];
  const requested = (environment.SLUGTALE_ENGINE_FEATURES || "")
    .split(",")
    .map((feature) => feature.trim())
    .filter(Boolean);

  return [...new Set([...baseline, ...requested])].join(",");
}

function withResolvedRuntimeFeatures(args, { platform, environment } = {}) {
  if (
    !args.length ||
    args.some((arg) => arg === "--features" || arg.startsWith("--features="))
  ) {
    return args;
  }

  if (args[0] !== "build" && args[0] !== "dev") {
    return args;
  }

  return [
    args[0],
    "--features",
    resolveRuntimeFeatures({ platform, environment }),
    ...args.slice(1),
  ];
}

// The committed config signs updater artifacts (tests/updater-config.test.mjs
// pins that), and Tauri refuses to finish the build without the minisign
// private key — which deliberately lives outside the repository. A release
// build on a machine that has the key (TAURI_SIGNING_PRIVATE_KEY or its _PATH
// variant) keeps signing; a local build without it still gets a complete
// .app/.dmg by dropping only the .tar.gz signature step.
function withoutUnsignedUpdaterArtifacts(args, { environment } = {}) {
  if (
    !args.length ||
    args[0] !== "build" ||
    args.includes("--config") ||
    environment.TAURI_SIGNING_PRIVATE_KEY ||
    environment.TAURI_SIGNING_PRIVATE_KEY_PATH
  ) {
    return args;
  }

  return [
    ...args,
    "--config",
    JSON.stringify({ bundle: { createUpdaterArtifacts: false } }),
  ];
}

function runTauri({
  args: rawArgs = process.argv.slice(2),
  platform = process.platform,
  environment = process.env,
  readFileSync = defaultReadFileSync,
  spawnSync = defaultSpawnSync,
  projectRoot = root,
} = {}) {
  const command = platform === "win32" ? "tauri.cmd" : "tauri";
  const args = withoutUnsignedUpdaterArtifacts(
    withResolvedRuntimeFeatures(rawArgs, {
      platform,
      environment,
    }),
    { environment },
  );
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
  resolveRuntimeFeatures,
  withResolvedRuntimeFeatures,
  withoutUnsignedUpdaterArtifacts,
  runTauri,
};
