#!/usr/bin/env node

const { existsSync } = require("node:fs");
const { join } = require("node:path");
const { spawnSync } = require("node:child_process");
const {
  createTauriEnvironment,
  resolveRuntimeFeatures,
} = require("./run-tauri.js");

const root = join(__dirname, "..");
const tauri = process.platform === "win32" ? "tauri.cmd" : "tauri";
const macosBundleIdentifier = "com.slugtale.desktop";
const defaultMacosSignIdentity = "Slugtale Dev";
const macosSignIdentity =
  process.env.SLUGTALE_SIGN_IDENTITY || defaultMacosSignIdentity;
const installRoot = process.env.SLUGTALE_INSTALL_DIR || "/Applications";
const buildOnly = process.argv.includes("--build-only");
const env = createTauriEnvironment({ projectRoot: root });
const runtimeFeatures = resolveRuntimeFeatures();

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    env,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    console.error(result.error.message);
    process.exit(1);
  }

  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function runForOutput(command, args) {
  return spawnSync(command, args, {
    cwd: root,
    env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    shell: false,
  });
}

function macosCodeSigningIdentityExists(identity) {
  const result = runForOutput("security", [
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

function requireMacosCodeSigningIdentity(identity) {
  if (identity === "-") {
    console.error(
      "SLUGTALE_SIGN_IDENTITY must name a stable code-signing identity, not '-'.",
    );
    console.error(
      "Ad-hoc signatures change on every build, so macOS drops the Microphone and Accessibility grants each time.",
    );
    process.exit(1);
  }

  if (macosCodeSigningIdentityExists(identity)) {
    return;
  }

  console.error(`Missing macOS code-signing identity: ${identity}`);
  console.error("");
  console.error("Create it once in Keychain Access:");
  console.error("1. Open Keychain Access.");
  console.error(
    "2. Choose Certificate Assistant > Create a Certificate from the Keychain Access menu.",
  );
  console.error(`3. Name it '${identity}'.`);
  console.error("4. Set Identity Type to 'Self Signed Root'.");
  console.error("5. Set Certificate Type to 'Code Signing'.");
  console.error(
    "6. Create it in the login keychain, then run npm run macos:install again.",
  );
  console.error("");
  console.error(
    "Set SLUGTALE_SIGN_IDENTITY to use a different existing identity.",
  );
  process.exit(1);
}

// The bundle is Slugtale.app but the executable inside it is `slugtale`, so
// process-name matching has to use the lowercase name.
const macosExecutableName = "slugtale";

function quitRunningSlugtale() {
  runForOutput("osascript", [
    "-e",
    `tell application id "${macosBundleIdentifier}" to quit`,
  ]);
  runForOutput("pkill", ["-x", macosExecutableName]);
}

if (process.platform !== "darwin") {
  console.error("This installer only supports macOS.");
  process.exit(1);
}

requireMacosCodeSigningIdentity(macosSignIdentity);

// Printed because a Transcription Engine that was not compiled in shows up in
// Settings as an unexplained "Unavailable" row, and this line is the only place
// the answer is visible before the build starts.
console.log(`Building with Cargo features: ${runtimeFeatures}`);

run(tauri, [
  "build",
  "--features",
  runtimeFeatures,
  "--bundles",
  "app",
]);

const builtAppPath = join(
  root,
  "src-tauri",
  "target",
  "release",
  "bundle",
  "macos",
  "Slugtale.app",
);

if (!existsSync(builtAppPath)) {
  console.error(`Expected macOS app bundle was not created: ${builtAppPath}`);
  process.exit(1);
}

run("codesign", [
  "--force",
  "--deep",
  "--sign",
  macosSignIdentity,
  "--identifier",
  macosBundleIdentifier,
  builtAppPath,
]);
run("codesign", ["--verify", "--deep", "--strict", builtAppPath]);

if (buildOnly) {
  console.log("");
  console.log(`Signed release bundle: ${builtAppPath}`);
  console.log("Run without --build-only to install it.");
  process.exit(0);
}

const installedAppPath = join(installRoot, "Slugtale.app");

quitRunningSlugtale();

if (existsSync(installedAppPath)) {
  // ditto merges into an existing bundle, so remove the old install first and
  // leave no stale files behind from a previous version.
  run("rm", ["-rf", installedAppPath]);
}

run("ditto", [builtAppPath, installedAppPath]);
run("codesign", ["--verify", "--deep", "--strict", installedAppPath]);
run("open", [installedAppPath]);

console.log("");
console.log(`Slugtale is installed at ${installedAppPath} and running.`);
console.log("Look for the Slugtale icon in the menu bar, not the Dock.");
console.log("");
console.log(
  "macOS treats this installed app and the developer-run build from npm run dev as separate privacy subjects.",
);
console.log(
  "Even if you granted the developer build access, open System Settings > Privacy & Security and grant Slugtale:",
);
console.log("1. Microphone access.");
console.log("2. Accessibility access for text insertion.");
console.log("");
console.log("Then open Slugtale settings and complete setup:");
console.log("1. Download the local base.en model.");
console.log("2. Choose a hotkey and activation mode.");
