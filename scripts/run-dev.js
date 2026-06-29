#!/usr/bin/env node

const { existsSync } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync } = require("node:child_process");

const root = join(__dirname, "..");
const tauri = process.platform === "win32" ? "tauri.cmd" : "tauri";
const macosBundleIdentifier = "com.slugtale.desktop";
const pathEntries = [
  join(root, "node_modules", ".bin"),
  process.env.HOME ? join(process.env.HOME, ".cargo", "bin") : null,
  process.env.PATH,
].filter(Boolean);
const env = {
  ...process.env,
  PATH: pathEntries.join(delimiter),
};

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

if (process.platform === "darwin") {
  run(tauri, [
    "build",
    "--debug",
    "--features",
    "local-whisper-runtime",
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
    console.error(`Expected macOS app bundle was not created: ${appPath}`);
    process.exit(1);
  }

  run("codesign", [
    "--force",
    "--deep",
    "--sign",
    "-",
    "--identifier",
    macosBundleIdentifier,
    appPath,
  ]);
  run("codesign", ["--verify", "--deep", "--strict", appPath]);
  run("open", ["-n", appPath]);
} else {
  run(tauri, ["dev", "--features", "local-whisper-runtime"]);
}
