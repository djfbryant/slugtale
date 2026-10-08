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

// The page loads its settings through `get_settings`. Every other command stays
// pending, so nothing but the test's own save answers.
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

// Let the settings load and any queued promise callbacks run.
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

test("the Insert after quiet control shows the length the settings file holds", async () => {
  const page = loadSettingsPage({ settings: settingsFixture({ segment_pause_secs: 8 }) });
  await settle();

  assert.equal(pressed(page.elements, "pause-8"), "true");
  assert.equal(pressed(page.elements, "pause-5"), "false");
});

test("a settings file from before the setting shows the five-second default", async () => {
  const { segment_pause_secs: _omitted, ...legacy } = settingsFixture();
  const page = loadSettingsPage({ settings: legacy });
  await settle();

  assert.equal(pressed(page.elements, "pause-5"), "true");
});

test("choosing a length saves it, and the control shows the saved length", async () => {
  const page = loadSettingsPage({
    answer: async (command) => command === "save_segment_pause_settings"
      ? settingsFixture({ segment_pause_secs: 8 })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("pause-8").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_segment_pause_settings");
  assert.deepEqual({ ...save.args }, { segmentPauseSecs: 8 });
  assert.equal(pressed(page.elements, "pause-8"), "true");
  assert.equal(pressed(page.elements, "pause-5"), "false");
  assert.equal(
    page.elements.get("transcription-message").textContent,
    "Saved. Applies to your next dictation."
  );
});

test("a save the app refuses puts the previous length back and says why", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_segment_pause_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("pause-2").click();
  await settle();

  assert.equal(pressed(page.elements, "pause-5"), "true");
  assert.equal(pressed(page.elements, "pause-2"), "false");
  const message = page.elements.get("transcription-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("while a length is being saved the control shows it and holds every choice", async () => {
  const pending = deferred();
  const page = loadSettingsPage({
    answer: (command) => command === "save_segment_pause_settings" ? pending.promise : new Promise(() => {})
  });
  await settle();

  page.elements.get("pause-10").click();

  assert.equal(pressed(page.elements, "pause-10"), "true");
  assert.equal(page.elements.get("pause-10").disabled, true);
  assert.equal(page.elements.get("pause-5").disabled, true);

  pending.resolve(settingsFixture({ segment_pause_secs: 10 }));
  await settle();
  assert.equal(page.elements.get("pause-5").disabled, false);
});

test("choosing the length already in use sends nothing", async () => {
  const page = loadSettingsPage();
  await settle();

  page.elements.get("pause-5").click();
  await settle();

  assert.equal(page.invocations.some(({ command }) => command === "save_segment_pause_settings"), false);
});

test("a speed save leaves the chosen Segment Pause alone", async () => {
  const page = loadSettingsPage({
    settings: settingsFixture({ segment_pause_secs: 8 }),
    answer: async (command) => command === "save_transcription_settings"
      ? settingsFixture({ segment_pause_secs: 8, speed_profile: "fast" })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("speed-fast").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_transcription_settings");
  assert.deepEqual({ ...save.args }, { speedProfile: "fast" });
  assert.equal(pressed(page.elements, "pause-8"), "true");
});
