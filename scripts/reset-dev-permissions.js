#!/usr/bin/env node

const { spawnSync: defaultSpawnSync } = require("node:child_process");

const macosBundleIdentifier = "com.slugtale.desktop";

function run({ spawnSync }, command, args) {
  const result = spawnSync(command, args, {
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

function resetDevPermissions({
  platform = process.platform,
  argv = process.argv,
  spawnSync = defaultSpawnSync,
  log = console.log,
} = {}) {
  if (platform !== "darwin") {
    log("macOS Accessibility reset is not needed on this platform.");
    return;
  }

  const resetAllAccessibility = argv.includes("--all-accessibility");

  run({ spawnSync }, "tccutil", ["reset", "Accessibility", macosBundleIdentifier]);

  if (resetAllAccessibility) {
    run({ spawnSync }, "tccutil", ["reset", "Accessibility"]);
  }

  log("");
  log("Slugtale macOS Accessibility state was reset.");
  log("");
  log("Next steps:");
  log("1. Quit any running Slugtale instances.");
  log("2. Run npm run dev so the app is signed with the stable Slugtale Dev identity.");
  log("3. Grant Slugtale in System Settings > Privacy & Security > Accessibility.");
  log("4. Run npm run dev again and confirm Text Insertion remains ready.");
  log("");
  log(
    "If stale Slugtale rows remain, run npm run macos:reset-permissions -- --all-accessibility, then grant again.",
  );
}

if (require.main === module) {
  try {
    resetDevPermissions();
  } catch (error) {
    console.error(error.message);
    process.exitCode = error.exitCode ?? 1;
  }
}

module.exports = {
  resetDevPermissions,
};
