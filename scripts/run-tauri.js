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
// macOS also ships the opt-in Voice Activation listener. Its Settings toggle
// remains off until the user turns it on.
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
      ? [
          "local-whisper-runtime",
          "local-whisper-runtime-metal",
          "voice-activation",
        ]
      : ["local-whisper-runtime"];
  const requested = (environment.SLUGTALE_ENGINE_FEATURES || "")
    .split(",")
    .map((feature) => feature.trim())
    .filter(Boolean);

  // Cargo rejects an unknown feature name with a clear message of its own, so
  // this deliberately does not second-guess the list in Cargo.toml.
  return [...new Set([...baseline, ...requested])].join(",");
}

// Build and dev invocations must never pin their own Cargo feature list:
// scripts/run-dev.js and install-macos-app.js resolve the platform baseline
// (Metal on macOS) through resolveRuntimeFeatures, and an `npm run build`
// that hardcodes `local-whisper-runtime` would silently ship a CPU-only
// binary where the developer-run build has GPU acceleration. When the caller
// names --features explicitly, their list wins untouched.
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
