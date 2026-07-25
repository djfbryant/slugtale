import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const { resolveRuntimeFeatures } = require("../scripts/run-tauri.js");

test("macOS builds get Whisper with Metal by default", () => {
  assert.equal(
    resolveRuntimeFeatures({ platform: "darwin", environment: {} }),
    "local-whisper-runtime,local-whisper-runtime-metal",
  );
});

test("other platforms get plain Whisper by default", () => {
  assert.equal(
    resolveRuntimeFeatures({ platform: "win32", environment: {} }),
    "local-whisper-runtime",
  );
});

test("requested engine features are added on top of the Whisper baseline", () => {
  assert.equal(
    resolveRuntimeFeatures({
      platform: "darwin",
      environment: {
        SLUGTALE_ENGINE_FEATURES:
          "apple-speech-runtime,local-parakeet-runtime-coreml",
      },
    }),
    "local-whisper-runtime,local-whisper-runtime-metal,apple-speech-runtime,local-parakeet-runtime-coreml",
  );
});

test("whitespace and empty entries in the request are ignored", () => {
  assert.equal(
    resolveRuntimeFeatures({
      platform: "linux",
      environment: {
        SLUGTALE_ENGINE_FEATURES: " local-parakeet-runtime , ,",
      },
    }),
    "local-whisper-runtime,local-parakeet-runtime",
  );
});

test("a request that repeats the baseline does not duplicate it", () => {
  assert.equal(
    resolveRuntimeFeatures({
      platform: "darwin",
      environment: {
        SLUGTALE_ENGINE_FEATURES: "local-whisper-runtime,apple-speech-runtime",
      },
    }),
    "local-whisper-runtime,local-whisper-runtime-metal,apple-speech-runtime",
  );
});

test("the dev and install launchers both resolve features through the shared helper", () => {
  for (const script of ["run-dev.js", "install-macos-app.js"]) {
    const source = readFileSync(
      new URL(`../scripts/${script}`, import.meta.url),
      "utf8",
    );

    assert.match(
      source,
      /resolveRuntimeFeatures\(\)/,
      `${script} must resolve Cargo features through the shared helper`,
    );
    assert.doesNotMatch(
      source,
      /"local-whisper-runtime/,
      `${script} must not hardcode a Cargo feature list of its own`,
    );
  }
});
