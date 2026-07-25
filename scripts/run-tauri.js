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

// Whisper is the baseline every developer-run build carries: it is the only
// engine available on every platform, and Metal costs nothing extra on macOS.
// Every engine past it is opt-in at compile time (src-tauri/Cargo.toml) because
// each drags in a native toolchain — ONNX Runtime for Parakeet, the Swift
// compiler for Apple SpeechTranscriber — so they are compiled in only when a
// developer names them in SLUGTALE_ENGINE_FEATURES. Without that, Settings
// reports those engines as `RuntimeNotBuilt`, which is the truth about the
// binary rather than about the machine.
function resolveRuntimeFeatures({
  platform = process.platform,
  environment = process.env,
} = {}) {
  const baseline =
    platform === "darwin"
      ? ["local-whisper-runtime", "local-whisper-runtime-metal"]
      : ["local-whisper-runtime"];
  const requested = (environment.SLUGTALE_ENGINE_FEATURES || "")
    .split(",")
    .map((feature) => feature.trim())
    .filter(Boolean);

  // Cargo rejects an unknown feature name with a clear message of its own, so
  // this deliberately does not second-guess the list in Cargo.toml.
  return [...new Set([...baseline, ...requested])].join(",");
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
  resolveRuntimeFeatures,
  runTauri,
};
