import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/rust.yml", import.meta.url),
  "utf8",
);

test("Windows CI builds the Whisper runtime through the documented npm script", () => {
  // npm test runs with default features, so whisper.cpp is not compiled by it.
  // Without this step the Windows job goes green while never touching the ASR
  // baseline that ADR-0006 and ADR-0001 depend on — which is exactly what it
  // did until slugtale-5pc.10. Pinned via the npm script rather than a bare
  // cargo call, per the project-checks rule in CLAUDE.md and AGENTS.md.
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
