import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const {
  runTauri,
  withResolvedRuntimeFeatures,
  withoutUnsignedUpdaterArtifacts,
} = require("../scripts/run-tauri.js");
const { runDev, defaultMacosSignIdentity } = require("../scripts/run-dev.js");

// The developer-run build is the only path that produces a macOS bundle the OS
// will keep permission grants for, so its identity and its signature are worth
// pinning. The script is driven directly rather than read as text: the assertions
// are about the commands it runs, and a rename inside the script should not be
// able to pass by keeping the words it used to use.
function runDeveloperBuild({ environment = {}, identityOutput = "1) abc \"Slugtale Dev\"" } = {}) {
  const commands = [];
  const logs = [];
  const appPath = new URL(
    "../src-tauri/target/debug/bundle/macos/Slugtale.app",
    import.meta.url,
  ).pathname;

  runDev({
    platform: "darwin",
    environment: { HOME: "/Users/tester", PATH: "/usr/bin", ...environment },
    existsSync: () => true,
    spawnSync(command, args) {
      commands.push([command, ...args]);
      if (command === "security") {
        return { status: 0, stdout: identityOutput, stderr: "" };
      }
      return { status: 0 };
    },
    log(message) {
      logs.push(message);
    },
  });

  return { commands, logs, appPath };
}

test("package exposes the developer run through the macOS bundling script", () => {
  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  assert.equal(packageJson.scripts.dev, "node scripts/run-dev.js");
});

test("the developer run is a signed macOS bundle under Slugtale's own identifier", () => {
  const config = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );

  assert.equal(config.identifier, "com.slugtale.desktop");
  assert.equal(config.bundle.active, true);
  assert.equal(config.bundle.macOS.infoPlist, "Info.plist");
});

test("the developer run says why it needs the microphone", () => {
  const plist = readFileSync(new URL("../src-tauri/Info.plist", import.meta.url), "utf8");

  assert.match(plist, /<key>NSMicrophoneUsageDescription<\/key>/);
  assert.match(plist, /dictation/);
});

test("the developer run builds a bundle, signs it with the stable identity, and opens it", () => {
  const { commands, appPath } = runDeveloperBuild();

  assert.ok(
    commands.some(
      ([command, ...args]) => command === "tauri" && args.includes("--bundles") && args.includes("app"),
    ),
    `no tauri --bundles app invocation in ${JSON.stringify(commands)}`,
  );
  assert.deepEqual(commands.filter(([command]) => command === "codesign"), [
    [
      "codesign",
      "--force",
      "--deep",
      "--sign",
      defaultMacosSignIdentity,
      "--identifier",
      "com.slugtale.desktop",
      appPath,
    ],
    ["codesign", "--verify", "--deep", "--strict", appPath],
  ]);
  assert.deepEqual(commands.filter(([command]) => command === "open"), [["open", appPath]]);
});

test("the developer run refuses an ad-hoc signing identity", () => {
  // "-" is what `codesign` calls a signature with no identity behind it. macOS
  // drops every permission grant on an app signed that way, so a developer run
  // that used it would ask for the microphone and Accessibility again on every
  // launch and look like a permissions bug.
  assert.throws(
    () => runDeveloperBuild({ environment: { SLUGTALE_SIGN_IDENTITY: "-" } }),
    /stable code-signing identity/,
  );
});

test("the developer run stops when the named signing identity is not in the keychain", () => {
  assert.throws(
    () => runDeveloperBuild({ identityOutput: "0 valid identities found" }),
    /Missing macOS code-signing identity/,
  );
});

test("the developer run does not force a second Slugtale instance", () => {
  // `open -n` launches a new instance even when one is already running, so the
  // tray and its single hotkey registration would end up duplicated.
  const { commands } = runDeveloperBuild();

  assert.ok(
    !commands.some(([command, ...args]) => command === "open" && args.includes("-n")),
    `developer runs must not force a second Slugtale instance: ${JSON.stringify(commands)}`,
  );
});

test("package build uses the Tauri launcher", () => {
  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  assert.equal(packageJson.scripts.build, "node scripts/run-tauri.js build --ci");
});

test("the release build resolves Cargo features through the shared helper", () => {
  assert.deepEqual(
    withResolvedRuntimeFeatures(["build", "--ci"], {
      platform: "darwin",
      environment: {},
    }),
    [
      "build",
      "--features",
      "local-whisper-runtime,local-whisper-runtime-metal,voice-activation",
      "--ci",
    ],
  );
});

test("the release build keeps plain Whisper on other platforms", () => {
  assert.deepEqual(
    withResolvedRuntimeFeatures(["build", "--ci"], {
      platform: "win32",
      environment: {},
    }),
    ["build", "--features", "local-whisper-runtime", "--ci"],
  );
});

test("an explicit --features list is passed through untouched", () => {
  assert.deepEqual(
    withResolvedRuntimeFeatures(
      ["build", "--features", "local-whisper-runtime"],
      { platform: "darwin", environment: {} },
    ),
    ["build", "--features", "local-whisper-runtime"],
  );
});

test("a local build without the signing key drops only the updater signature step", () => {
  const features = "local-whisper-runtime,local-whisper-runtime-metal";
  assert.deepEqual(
    withoutUnsignedUpdaterArtifacts(["build", "--features", features, "--ci"], {
      environment: {},
    }),
    [
      "build",
      "--features",
      features,
      "--ci",
      "--config",
      JSON.stringify({ bundle: { createUpdaterArtifacts: false } }),
    ],
  );
});

test("a build with the signing key keeps the signed updater artifacts", () => {
  for (const key of [
    "TAURI_SIGNING_PRIVATE_KEY",
    "TAURI_SIGNING_PRIVATE_KEY_PATH",
  ]) {
    assert.deepEqual(
      withoutUnsignedUpdaterArtifacts(["build", "--ci"], {
        environment: { [key]: "secret" },
      }),
      ["build", "--ci"],
    );
  }
});

test("an explicit --config is never overridden", () => {
  assert.deepEqual(
    withoutUnsignedUpdaterArtifacts(["build", "--config", "custom.json"], {
      environment: {},
    }),
    ["build", "--config", "custom.json"],
  );
});

test("macOS release builds compile native dependencies for the bundled minimum system version", () => {
  const invocations = [];

  runTauri({
    args: ["build", "--features", "local-whisper-runtime"],
    platform: "darwin",
    environment: {
      HOME: "/Users/tester",
      PATH: "/usr/bin",
    },
    spawnSync(command, args, options) {
      invocations.push({ command, args, options });
      return { status: 0 };
    },
  });

  assert.equal(invocations.length, 1);
  assert.equal(invocations[0].options.env.CMAKE_OSX_DEPLOYMENT_TARGET, "10.15");
});
