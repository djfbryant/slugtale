import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { runPage } from "./harness.mjs";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");

function deferred() {
  let resolve;
  const promise = new Promise((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function loadAppUpdate({ invoke, runInit = false }) {
  const invocations = [];
  const { api, elements } = runPage("index.html", {
    // The phase lives in one private object, so the handle is a closure over it
    // rather than a name the page already exports.
    exports: ["checkForAppUpdate", "getAppUpdateState", "openAppUpdateRelease", "renderAppUpdate"],
    exportSource:
      "checkForAppUpdate, getAppUpdateState: () => ({ ...appUpdateState }), openAppUpdateRelease, renderAppUpdate",
    invoke(command, args) {
      invocations.push({ args, command });
      return invoke(command, args);
    },
    runBootstrap: runInit
  });

  return { elements, invocations, ...api };
}

test("app updates start idle without a release request", () => {
  const app = loadAppUpdate({ invoke: async () => ({ status: "current" }) });

  app.renderAppUpdate();

  assert.equal(app.getAppUpdateState().phase, "idle");
  assert.equal(app.elements.get("app-update-check-button").disabled, false);
  assert.equal(app.elements.get("app-update-release-button").hidden, true);
  assert.match(app.elements.get("app-update-state").textContent, /only when you select Check now/);
  assert.deepEqual(app.invocations, []);
});

test("the real Settings startup path makes no update request", () => {
  const neverAnswers = new Promise(() => {});
  const app = loadAppUpdate({ invoke: () => neverAnswers, runInit: true });

  assert.equal(
    app.invocations.some(({ command }) => command === "check_for_app_update"),
    false,
  );
});

test("a manual check enters checking and blocks a duplicate check", async () => {
  const answer = deferred();
  const app = loadAppUpdate({ invoke: () => answer.promise });

  const firstCheck = app.checkForAppUpdate();
  const duplicateCheck = app.checkForAppUpdate();

  assert.equal(app.getAppUpdateState().phase, "checking");
  assert.equal(app.elements.get("app-update-check-button").disabled, true);
  assert.deepEqual(app.invocations.map(({ command }) => command), ["check_for_app_update"]);

  answer.resolve({ status: "current" });
  await Promise.all([firstCheck, duplicateCheck]);
});

test("a current result shows that Slugtale is up to date", async () => {
  const app = loadAppUpdate({ invoke: async () => ({ status: "current" }) });

  await app.checkForAppUpdate();

  assert.equal(app.getAppUpdateState().phase, "current");
  assert.equal(app.elements.get("app-update-state").textContent, "Slugtale is up to date.");
  assert.equal(app.elements.get("app-update-release-button").hidden, true);
});

test("an available result shows the release-page action", async () => {
  const app = loadAppUpdate({ invoke: async () => ({ status: "available", version: "0.2.0" }) });

  await app.checkForAppUpdate();

  assert.equal(app.getAppUpdateState().phase, "available");
  assert.equal(app.getAppUpdateState().version, "0.2.0");
  assert.equal(app.elements.get("app-update-state").textContent, "Version 0.2.0 is available.");
  assert.equal(app.elements.get("app-update-release-button").hidden, false);

  await app.openAppUpdateRelease();
  assert.deepEqual(
    app.invocations.map(({ command }) => command),
    ["check_for_app_update", "open_app_update_release"],
  );
});

test("a failed check shows an error and keeps the release action hidden", async () => {
  const app = loadAppUpdate({
    invoke: async () => {
      throw new Error("release server is offline");
    },
  });

  await app.checkForAppUpdate();

  assert.equal(app.getAppUpdateState().phase, "error");
  assert.equal(app.elements.get("app-update-state").textContent, "Slugtale could not check for updates.");
  assert.equal(app.elements.get("app-update-message").textContent, "Error: release server is offline");
  assert.equal(app.elements.get("app-update-message").classList.contains("error"), true);
  assert.equal(app.elements.get("app-update-release-button").hidden, true);
});

test("a release-page failure keeps the known update available for retry", async () => {
  const app = loadAppUpdate({
    async invoke(command) {
      if (command === "check_for_app_update") {
        return { status: "available", version: "0.2.0" };
      }
      throw new Error("browser did not open");
    },
  });

  await app.checkForAppUpdate();
  await app.openAppUpdateRelease();

  assert.equal(app.getAppUpdateState().phase, "available");
  assert.equal(app.getAppUpdateState().version, "0.2.0");
  assert.equal(app.elements.get("app-update-release-button").hidden, false);
  assert.equal(app.elements.get("app-update-message").textContent, "Error: browser did not open");
  assert.equal(app.elements.get("app-update-message").classList.contains("error"), true);
});
