#!/usr/bin/env node

const { spawnSync } = require("node:child_process");

const macosBundleIdentifier = "com.slugtale.desktop";
const resetAllAccessibility = process.argv.includes("--all-accessibility");

function run(command, args) {
  const result = spawnSync(command, args, {
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

if (process.platform !== "darwin") {
  console.log("macOS Accessibility reset is not needed on this platform.");
  process.exit(0);
}

run("tccutil", ["reset", "Accessibility", macosBundleIdentifier]);

if (resetAllAccessibility) {
  run("tccutil", ["reset", "Accessibility"]);
}

console.log("");
console.log("Slugtale macOS Accessibility state was reset.");
console.log("");
console.log("Next steps:");
console.log("1. Quit any running Slugtale instances.");
console.log("2. Run npm run dev so the app is signed with the stable Slugtale Dev identity.");
console.log("3. Grant Slugtale in System Settings > Privacy & Security > Accessibility.");
console.log("4. Run npm run dev again and confirm Text Insertion remains ready.");
console.log("");
console.log(
  "If stale Slugtale rows remain, run npm run macos:reset-permissions -- --all-accessibility, then grant again.",
);
