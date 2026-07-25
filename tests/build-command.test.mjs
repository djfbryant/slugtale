import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const { runTauri } = require("../scripts/run-tauri.js");

test("package build uses the Tauri launcher", () => {
  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  assert.equal(
    packageJson.scripts.build,
    "node scripts/run-tauri.js build --features local-whisper-runtime --ci",
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
