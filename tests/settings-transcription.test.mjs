import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

function settingsFixture(overrides = {}) {
  return {
    hotkey: null,
    activation_mode: "toggle",
    launch_at_login: false,
    diagnostic_logging: false,
    model: null,
    speed_profile: "balanced",
    segment_pause_secs: 5,
    bar_position: "bottom-center",
    accent_color: "red",
    bar_display: "primary",
    primary_engine: "whisper",
    second_opinion: "off",
    transcript_cleanup: "basic",
    voice_activation_enabled: false,
    prefer_built_in_microphone: true,
    ...overrides
  };
}

function loadSettingsPage({ settings = settingsFixture(), answer = () => new Promise(() => {}) } = {}) {
  const invocations = [];
  const page = runPage("index.html", {
    runBootstrap: true,
    invoke(command, args) {
      invocations.push({ command, args });
      if (command === "get_settings") return Promise.resolve(settings);
      return answer(command, args);
    }
  });
  return { ...page.api, elements: page.elements, invocations };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function pressed(elements, id) {
  return elements.get(id).getAttribute("aria-pressed");
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

test("choosing a speed saves it and shows the saved length", async () => {
  const page = loadSettingsPage({
    answer: async (command) => command === "save_transcription_settings"
      ? settingsFixture({ speed_profile: "fast" })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("speed-fast").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_transcription_settings");
  assert.deepEqual({ ...save.args }, { speedProfile: "fast" });
  assert.equal(pressed(page.elements, "speed-fast"), "true");
  assert.equal(pressed(page.elements, "speed-balanced"), "false");
  assert.equal(
    page.elements.get("transcription-message").textContent,
    "Saved. Applies to your next dictation."
  );
});

test("a speed save the app refuses puts the previous speed back and says why", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_transcription_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("speed-accurate").click();
  await settle();

  assert.equal(pressed(page.elements, "speed-balanced"), "true");
  assert.equal(pressed(page.elements, "speed-accurate"), "false");
  const message = page.elements.get("transcription-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("choosing the speed already in use sends nothing", async () => {
  const page = loadSettingsPage();
  await settle();

  page.elements.get("speed-balanced").click();
  await settle();

  assert.equal(page.invocations.some(({ command }) => command === "save_transcription_settings"), false);
});

test("while a speed is being saved the control shows it and holds every speed choice", async () => {
  const pending = deferred();
  const page = loadSettingsPage({
    answer: (command) => command === "save_transcription_settings" ? pending.promise : new Promise(() => {})
  });
  await settle();

  page.elements.get("speed-fast").click();

  assert.equal(pressed(page.elements, "speed-fast"), "true");
  assert.equal(page.elements.get("speed-fast").disabled, true);
  assert.equal(page.elements.get("speed-balanced").disabled, true);

  pending.resolve(settingsFixture({ speed_profile: "fast" }));
  await settle();
  assert.equal(page.elements.get("speed-balanced").disabled, false);
});

test("saving an 8s pause that fails shows the old choice plus the error", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_segment_pause_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("pause-8").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_segment_pause_settings");
  assert.deepEqual({ ...save.args }, { segmentPauseSecs: 8 });
  assert.equal(pressed(page.elements, "pause-5"), "true");
  assert.equal(pressed(page.elements, "pause-8"), "false");
  const message = page.elements.get("transcription-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("saving an 8s pause that succeeds shows the saved choice", async () => {
  const page = loadSettingsPage({
    answer: async (command) => command === "save_segment_pause_settings"
      ? settingsFixture({ segment_pause_secs: 8 })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("pause-8").click();
  await settle();

  assert.equal(pressed(page.elements, "pause-8"), "true");
  assert.equal(pressed(page.elements, "pause-5"), "false");
  assert.equal(
    page.elements.get("transcription-message").textContent,
    "Saved. Applies to your next dictation."
  );
});
