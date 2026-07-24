#!/usr/bin/env node

const { existsSync: defaultExistsSync } = require("node:fs");
const { join } = require("node:path");
const { spawnSync: defaultSpawnSync } = require("node:child_process");

const macosBundleIdentifier = "com.slugtale.desktop";
const macosExecutableName = "slugtale";
const reauthorizePermissionsArgument = "--reauthorize-permissions";

function runCommand(spawnSync, command, args, allowedStatuses = [0]) {
  const result = spawnSync(command, args, {
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    throw new Error(`${command} could not start: ${result.error.message}`);
  }

  if (!allowedStatuses.includes(result.status)) {
    throw new Error(
      `${[command, ...args].join(" ")} exited with status ${result.status ?? "unknown"}`,
    );
  }
}

function reauthorizeMacosApp({
  platform = process.platform,
  installRoot = process.env.SLUGTALE_INSTALL_DIR || "/Applications",
  existsSync = defaultExistsSync,
  spawnSync = defaultSpawnSync,
  log = console.log,
} = {}) {
  if (platform !== "darwin") {
    throw new Error("Slugtale permission re-authorization only supports macOS.");
  }

  const installedAppPath = join(installRoot, "Slugtale.app");
  if (!existsSync(installedAppPath)) {
    throw new Error(
      `Installed app not found at ${installedAppPath}. Install Slugtale first with npm run macos:install.`,
    );
  }

  // pkill returns 1 when no matching process exists, which is already the
  // desired state. Any other status is a real failure.
  runCommand(spawnSync, "pkill", ["-x", macosExecutableName], [0, 1]);
  runCommand(spawnSync, "tccutil", [
    "reset",
    "Microphone",
    macosBundleIdentifier,
  ]);
  runCommand(spawnSync, "tccutil", [
    "reset",
    "Accessibility",
    macosBundleIdentifier,
  ]);
  runCommand(spawnSync, "open", [
    "-n",
    installedAppPath,
    "--args",
    reauthorizePermissionsArgument,
  ]);

  log("");
  log(`Reopened ${installedAppPath} for permission recovery.`);
  log("1. Accept the fresh Microphone prompt from Slugtale.");
  log(
    "2. In Slugtale settings, choose Open Accessibility and enable the installed app.",
  );

  return installedAppPath;
}

if (require.main === module) {
  try {
    reauthorizeMacosApp();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}

module.exports = {
  reauthorizeMacosApp,
};
