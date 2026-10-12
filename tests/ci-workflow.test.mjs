import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/rust.yml", import.meta.url),
  "utf8",
);

test("Windows CI builds the Whisper runtime through the documented npm script", () => {
  assert.match(workflow, /npm run test:whisper-build/);
});

test("Linux CI installs native Tauri build dependencies before npm test", () => {
  assert.match(workflow, /apt-get\s+update/);
  assert.match(workflow, /apt-get\s+install/);

  for (const packageName of [
    "pkg-config",
    "libglib2.0-dev",
    "libwebkit2gtk-4.1-dev",
    "libayatana-appindicator3-dev",
    "librsvg2-dev",
  ]) {
    assert.match(workflow, new RegExp(`\\b${packageName}\\b`));
  }
});

test("every CI job compiles the examples, so a broken example cannot go unnoticed", () => {
  // gated on local-whisper-runtime in Cargo.toml, so the check has to enable
  // that feature or it silently skips the very example it was added for.
  const exampleCount = (workflow.match(/npm run check:examples/g) || []).length;
  assert.equal(
    exampleCount,
    3,
    "all three jobs (Linux, Windows, macOS) must run an example compile check",
  );

  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  for (const script of ["check:examples", "check:examples:apple"]) {
    assert.match(packageJson.scripts[script], /run-cargo\.js check --examples/);
    assert.match(
      packageJson.scripts[script],
      /local-whisper-runtime/,
      `${script} must enable local-whisper-runtime or it skips asr_eval`,
    );
  }

  // The Apple acceleration features cannot build off macOS, so only the macOS
  // job may ask for them.
  assert.match(workflow, /npm run check:examples:apple/);
  assert.equal(
    (workflow.match(/local-whisper-runtime-metal/g) || []).length,
    0,
    "the wider feature set belongs in package.json, not inline in the workflow",
  );
});
