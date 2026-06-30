import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/rust.yml", import.meta.url),
  "utf8",
);

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
