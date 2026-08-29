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

test("webviews have no direct updater or opener permissions", () => {
  const capability = JSON.parse(readRepo("src-tauri/capabilities/default.json"));
  assert.equal(
    capability.permissions.some(
      (permission) => permission.startsWith("updater:") || permission.startsWith("opener:"),
    ),
    false,
  );
});

test("the binary registers manual check and fixed release-page commands", () => {
  const main = readRepo("src-tauri/src/main.rs");
  assert.match(main, /tauri_plugin_updater::Builder::new\(\)\.build\(\)/);
  assert.doesNotMatch(main, /tauri_plugin_opener/);
  for (const command of ["check_for_app_update", "open_app_update_release"]) {
    assert.match(main, new RegExp(`fn ${command}`), `${command} must be defined`);
    assert.match(
      main,
      new RegExp(`^\\s+${command},`, "m"),
      `${command} must be registered in the invoke handler`,
    );
  }
});

test("the release-page command opens only the compiled-in GitHub URL", () => {
  const main = readRepo("src-tauri/src/main.rs");
  assert.match(
    main,
    /const APP_UPDATE_RELEASE_URL: &str =\s*"https:\/\/github\.com\/djfbryant\/slugtale\/releases\/latest";/,
  );
  assert.match(
    main,
    /fn open_app_update_release\(\) -> Result<\(\), String>\s*{\s*open::that\(APP_UPDATE_RELEASE_URL\)/,
  );
  assert.doesNotMatch(main, /fn open_app_update_release\([^)]*url/i);
});

test("app update results use a tagged status enum", () => {
  const main = readRepo("src-tauri/src/main.rs");
  assert.match(main, /#\[serde\(tag = "status", rename_all = "snake_case"\)\]/);
  assert.match(main, /enum AppUpdateView\s*{\s*Current,\s*Available\s*{\s*version: String,?\s*},?\s*}/);
});

test("the app has no install command or frontend install call", () => {
  const main = readRepo("src-tauri/src/main.rs");
  const settings = readRepo("src/index.html");

  assert.doesNotMatch(main, /install_app_update|download_and_install/);
  assert.doesNotMatch(settings, /install_app_update|installAppUpdate|app-update-install-button/);
  assert.match(settings, /invoke\("check_for_app_update"\)/);
  for (const id of [
    "app-update-state",
    "app-update-check-button",
    "app-update-release-button",
    "app-update-message",
  ]) {
    assert.ok(settings.includes(`id="${id}"`), `Settings must render #${id}`);
  }
});

test("the private signing key never enters the repository", () => {
  const gitignore = readRepo(".gitignore");
  assert.match(gitignore, /\.key$/m);
});
