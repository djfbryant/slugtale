import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const config = require("../src-tauri/tauri.conf.json");

const readRepo = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("the updater endpoint points at the GitHub Release latest.json (ADR-0022)", () => {
  const endpoints = config.plugins?.updater?.endpoints ?? [];
  assert.deepEqual(
    endpoints,
    ["https://github.com/djfbryant/slugtale/releases/latest/download/latest.json"],
  );
});

test("a Tauri updater public key is committed in the config", () => {
  const pubkey = config.plugins?.updater?.pubkey;
  assert.match(typeof pubkey === "string" ? pubkey : "", /^dW50cnVzdGVkIGNvbW1lbnQ6/);
});

test("release builds produce the signed updater artifacts", () => {
  assert.equal(config.bundle.createUpdaterArtifacts, true);
});

test("the settings windows are allowed to use the updater", () => {
  const capability = JSON.parse(readRepo("src-tauri/capabilities/default.json"));
  assert.ok(capability.permissions.includes("updater:default"));
});

test("the binary registers the updater plugin and its two commands", () => {
  const main = readRepo("src-tauri/src/main.rs");
  assert.match(main, /tauri_plugin_updater::Builder::new\(\)\.build\(\)/);
  for (const command of ["check_for_app_update", "install_app_update"]) {
    assert.match(main, new RegExp(`fn ${command}`), `${command} must be defined`);
    assert.match(
      main,
      new RegExp(`^\\s+${command},`, "m"),
      `${command} must be registered in the invoke handler`,
    );
  }
});

test("installing restarts through the downloaded update rather than a manual copy", () => {
  const main = readRepo("src-tauri/src/main.rs");
  assert.match(main, /download_and_install/);
  assert.match(main, /app\.restart\(\)/);
});

test("Settings offers the check-on-launch and apply UX", () => {
  const settings = readRepo("src/index.html");
  assert.match(settings, /checkForAppUpdate\(\);\s*\n\s*}\s*\n\s*<\/script>|checkForAppUpdate\(\)/);
  assert.match(settings, /invoke\("check_for_app_update"\)/);
  assert.match(settings, /invoke\("install_app_update"\)/);
  for (const id of [
    "app-update-state",
    "app-update-check-button",
    "app-update-install-button",
    "app-update-message",
  ]) {
    assert.ok(settings.includes(`id="${id}"`), `Settings must render #${id}`);
  }
});

test("the private signing key never enters the repository", () => {
  const gitignore = readRepo(".gitignore");
  assert.match(gitignore, /\.key$/m);
});
