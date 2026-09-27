#!/usr/bin/env node

const { existsSync: defaultExistsSync } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync: defaultSpawnSync } = require("node:child_process");
const { resolveRuntimeFeatures } = require("./run-tauri.js");

const root = join(__dirname, "..");
const macosBundleIdentifier = "com.slugtale.desktop";
const defaultMacosSignIdentity = "Slugtale Dev";

function pathEntries(environment) {
  return [
    join(root, "node_modules", ".bin"),
    environment.HOME ? join(environment.HOME, ".cargo", "bin") : null,
    environment.PATH,
  ].filter(Boolean);
}

function run({ cwd, env, spawnSync }, command, args) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    const failure = new Error(result.error.message);
    failure.exitCode = 1;
    throw failure;
  }

  if (result.status !== 0) {
    const failure = new Error(
      `${[command, ...args].join(" ")} exited with status ${result.status ?? 1}`,
    );
    failure.exitCode = result.status ?? 1;
    throw failure;
  }
}

function runForOutput({ cwd, env, spawnSync }, command, args) {
  return spawnSync(command, args, {
    cwd,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    shell: false,
  });
}

function macosCodeSigningIdentityExists(identity, system) {
  const result = runForOutput(system, "security", [
    "find-identity",
    "-v",
    "-p",
    "codesigning",
    "-s",
    identity,
  ]);
  const output = `${result.stdout || ""}\n${result.stderr || ""}`;

  return result.error == null && /^ *\d+\) [A-Fa-f0-9]+ "/m.test(output);
}

function requireMacosCodeSigningIdentity(identity, system) {
  // An ad-hoc identity ("-") produces a bundle macOS refuses to keep permission
  // grants for, so a developer run that cannot sign with a stable identity stops
  // here rather than building something that silently asks for every permission
  // again on the next launch.
  if (identity === "-") {
    const failure = new Error(
      "SLUGTALE_SIGN_IDENTITY must name a stable code-signing identity, not '-'.",
    );
    failure.exitCode = 1;
    throw failure;
  }

  if (macosCodeSigningIdentityExists(identity, system)) {
    return;
  }

  const failure = new Error(`Missing macOS code-signing identity: ${identity}`);
  failure.exitCode = 1;
  failure.remedy = [
    "",
    "Create it once in Keychain Access:",
    "1. Open Keychain Access.",
    "2. Choose Certificate Assistant > Create a Certificate from the Keychain Access menu.",
    "3. Name it 'Slugtale Dev'.",
    "4. Set Identity Type to 'Self Signed Root'.",
    "5. Set Certificate Type to 'Code Signing'.",
    "6. Create it in the login keychain, then run npm run dev again.",
    "",
    "Set SLUGTALE_SIGN_IDENTITY to use a different existing identity.",
  ].join("\n");
  throw failure;
}

function runDev({
  platform = process.platform,
  environment = process.env,
  existsSync = defaultExistsSync,
  spawnSync = defaultSpawnSync,
  log = console.log,
} = {}) {
  // An ad-hoc identity produces a bundle macOS refuses to keep permission grants
  // for. The refusal itself lives in requireMacosCodeSigningIdentity, next to the
  // check for whether the identity exists at all.
  const macosSignIdentity =
    environment.SLUGTALE_SIGN_IDENTITY || defaultMacosSignIdentity;

  const tauriCommand = platform === "win32" ? "tauri.cmd" : "tauri";
  const system = {
    cwd: root,
    env: { ...environment, PATH: pathEntries(environment).join(delimiter) },
    spawnSync,
  };
  const runtimeFeatures = resolveRuntimeFeatures();

  // Printed because a Transcription Engine that was not compiled in shows up in
  // Settings as an unexplained "Unavailable" row, and this line is the only place
  // the answer is visible before the build starts.
  log(`Building with Cargo features: ${runtimeFeatures}`);

  if (platform !== "darwin") {
    run(system, tauriCommand, ["dev", "--features", runtimeFeatures]);
    return;
  }

  requireMacosCodeSigningIdentity(macosSignIdentity, system);

  run(system, tauriCommand, [
    "build",
    "--debug",
    "--features",
    runtimeFeatures,
    "--bundles",
    "app",
  ]);

  const appPath = join(
    root,
    "src-tauri",
    "target",
    "debug",
    "bundle",
    "macos",
    "Slugtale.app",
  );

  if (!existsSync(appPath)) {
    const failure = new Error(
      `Expected macOS app bundle was not created: ${appPath}`,
    );
    failure.exitCode = 1;
    throw failure;
  }

  run(system, "codesign", [
    "--force",
    "--deep",
    "--sign",
    macosSignIdentity,
    "--identifier",
    macosBundleIdentifier,
    appPath,
  ]);
  run(system, "codesign", ["--verify", "--deep", "--strict", appPath]);
  run(system, "open", [appPath]);
}

if (require.main === module) {
  try {
    runDev();
  } catch (error) {
    console.error(error.message);
    if (error.remedy) {
      console.error(error.remedy);
    }
    process.exitCode = error.exitCode ?? 1;
  }
}

module.exports = {
  runDev,
  defaultMacosSignIdentity,
};
