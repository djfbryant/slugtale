import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const {
  reauthorizeMacosApp,
} = require("../scripts/reauthorize-macos-app.js");
const {
  resetDevPermissions,
} = require("../scripts/reset-dev-permissions.js");

test("package exposes the macOS re-authorization recovery command", () => {
  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  assert.equal(
    packageJson.scripts["macos:reauthorize"],
    "node scripts/reauthorize-macos-app.js",
  );
});

test("macOS recovery resets Slugtale grants and relaunches the installed app in re-authorization mode", () => {
  const commands = [];
  const messages = [];

  const installedAppPath = reauthorizeMacosApp({
    platform: "darwin",
    installRoot: "/Applications",
    existsSync: () => true,
    spawnSync(command, args) {
      commands.push([command, ...args]);
      return { status: command === "pkill" ? 1 : 0 };
    },
    log(message) {
      messages.push(message);
    },
  });

  assert.equal(installedAppPath, "/Applications/Slugtale.app");
  assert.deepEqual(commands, [
    ["pkill", "-x", "slugtale"],
    ["tccutil", "reset", "Microphone", "com.slugtale.desktop"],
    ["tccutil", "reset", "Accessibility", "com.slugtale.desktop"],
    [
      "open",
      "-n",
      "/Applications/Slugtale.app",
      "--args",
      "--reauthorize-permissions",
    ],
  ]);
  assert.match(messages.join("\n"), /fresh Microphone prompt/);
  assert.match(messages.join("\n"), /Accessibility/);
});

test("macOS recovery refuses to reset grants when the installed app is missing", () => {
  const commands = [];

  assert.throws(
    () =>
      reauthorizeMacosApp({
        platform: "darwin",
        installRoot: "/Applications",
        existsSync: () => false,
        spawnSync(command, args) {
          commands.push([command, ...args]);
          return { status: 0 };
        },
        log() {},
      }),
    /Install Slugtale first with npm run macos:install/,
  );

  assert.deepEqual(commands, []);
});

test("macOS recovery reports a TCC reset failure and does not relaunch", () => {
  const commands = [];

  assert.throws(
    () =>
      reauthorizeMacosApp({
        platform: "darwin",
        installRoot: "/Applications",
        existsSync: () => true,
        spawnSync(command, args) {
          commands.push([command, ...args]);
          return {
            status:
              command === "tccutil" && args.includes("Microphone") ? 69 : 0,
          };
        },
        log() {},
      }),
    /tccutil reset Microphone com\.slugtale\.desktop exited with status 69/,
  );

  assert.equal(commands.some(([command]) => command === "open"), false);
});

test("package exposes the Accessibility recovery command for developer runs", () => {
  const packageJson = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8"),
  );

  assert.equal(
    packageJson.scripts["macos:reset-permissions"],
    "node scripts/reset-dev-permissions.js",
  );
});

test("the recovery command clears Slugtale's Accessibility grant and says what to do next", () => {
  const commands = [];
  const messages = [];

  resetDevPermissions({
    platform: "darwin",
    argv: ["node", "reset-dev-permissions.js"],
    spawnSync(command, args) {
      commands.push([command, ...args]);
      return { status: 0 };
    },
    log(message) {
      messages.push(message);
    },
  });

  assert.deepEqual(commands, [
    ["tccutil", "reset", "Accessibility", "com.slugtale.desktop"],
  ]);
  assert.match(messages.join("\n"), /npm run dev/);
});

test("the recovery command only clears every app's Accessibility grants when asked", () => {
  const commands = [];

  resetDevPermissions({
    platform: "darwin",
    argv: ["node", "reset-dev-permissions.js", "--all-accessibility"],
    spawnSync(command, args) {
      commands.push([command, ...args]);
      return { status: 0 };
    },
    log() {},
  });

  assert.deepEqual(commands, [
    ["tccutil", "reset", "Accessibility", "com.slugtale.desktop"],
    ["tccutil", "reset", "Accessibility"],
  ]);
});

test("the recovery command does nothing off macOS", () => {
  const commands = [];

  resetDevPermissions({
    platform: "linux",
    spawnSync(command, args) {
      commands.push([command, ...args]);
      return { status: 0 };
    },
    log() {},
  });

  assert.deepEqual(commands, []);
});
