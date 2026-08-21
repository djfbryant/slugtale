import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const iconsDir = new URL("../src-tauri/icons/", import.meta.url);

test("tauri bundle lists the generated app icons", () => {
  const conf = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );

  assert.ok(conf.bundle.active, "bundling must stay enabled");
  assert.deepEqual([...conf.bundle.icon].sort(), [
    "icons/128x128.png",
    "icons/128x128@2x.png",
    "icons/32x32.png",
    "icons/icon.icns",
    "icons/icon.ico",
  ]);
});

test("every bundled icon file exists on disk", () => {
  const conf = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );

  for (const rel of conf.bundle.icon) {
    assert.ok(existsSync(new URL(`../src-tauri/${rel}`, import.meta.url)), `${rel} is missing`);
  }
});

test("PNG icon dimensions match their filenames", () => {
  const expected = { "32x32.png": 32, "128x128.png": 128, "128x128@2x.png": 256 };

  for (const [name, size] of Object.entries(expected)) {
    const buf = readFileSync(new URL(name, iconsDir));
    assert.equal(buf.readUInt32BE(16), size, `${name} width`);
    assert.equal(buf.readUInt32BE(20), size, `${name} height`);
  }
});
