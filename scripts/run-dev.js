#!/usr/bin/env node

const { existsSync } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync } = require("node:child_process");

const root = join(__dirname, "..");
const tauri = process.platform === "win32" ? "tauri.cmd" : "tauri";
const macosBundleIdentifier = "com.slugtale.desktop";
const defaultMacosSignIdentity = "Slugtale Dev";
const macosSignIdentity =
  process.env.SLUGTALE_SIGN_IDENTITY || defaultMacosSignIdentity;
const pathEntries = [
  join(root, "node_modules", ".bin"),
  process.env.HOME ? join(process.env.HOME, ".cargo", "bin") : null,
  process.env.PATH,
].filter(Boolean);
const env = {
  ...process.env,
  PATH: pathEntries.join(delimiter),
};
const whisperRuntimeFeatures =
  process.platform === "darwin"
    ? "local-whisper-runtime,local-whisper-runtime-metal"
    : "local-whisper-runtime";

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
  console.error("6. Create it in the login keychain, then run npm run dev again.");
  console.error("");
  console.error(
    `Set SLUGTALE_SIGN_IDENTITY to use a different existing identity.`,
  );
  process.exit(1);
}

if (process.platform === "darwin") {
  requireMacosCodeSigningIdentity(macosSignIdentity);

  run(tauri, [
    "build",
    "--debug",
    "--features",
    whisperRuntimeFeatures,
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
    macosSignIdentity,
    "--identifier",
    macosBundleIdentifier,
    appPath,
  ]);
  run("codesign", ["--verify", "--deep", "--strict", appPath]);
  run("open", [appPath]);
} else {
  run(tauri, ["dev", "--features", whisperRuntimeFeatures]);
}
