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
