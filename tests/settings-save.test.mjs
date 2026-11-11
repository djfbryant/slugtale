import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

// Page tests for the four local setting saves (speed, Segment Pause,
// transcript cleanup, Dictation Bar). Each save owns its pending flag,
// its save call, its saved message, and the restore of the previous
// choice when the app refuses. The loop refactors these four into one
// private save function; these tests pin the user-visible behaviour so
// the refactor cannot change a message, a restore, or a control state.

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
  if (answer === null) {
    const page = runPage("index.html", { runBootstrap: true, invoke: null });
    return { ...page.api, elements: page.elements, invocations: [] };
  }
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

// --- Speed: save_transcription_settings ---

test("choosing a speed saves it and the control shows the saved speed", async () => {
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

test("a save the app refuses puts the previous speed back and says why", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_transcription_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("speed-fast").click();
  await settle();

  assert.equal(pressed(page.elements, "speed-balanced"), "true");
  assert.equal(pressed(page.elements, "speed-fast"), "false");
  const message = page.elements.get("transcription-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("while a speed is being saved the speed choices hold", async () => {
  const pending = deferred();
  const page = loadSettingsPage({
    answer: (command) => command === "save_transcription_settings" ? pending.promise : new Promise(() => {})
  });
  await settle();

  page.elements.get("speed-accurate").click();

  assert.equal(pressed(page.elements, "speed-accurate"), "true");
  assert.equal(page.elements.get("speed-accurate").disabled, true);
  assert.equal(page.elements.get("speed-balanced").disabled, true);
  assert.equal(page.elements.get("speed-fast").disabled, true);

  pending.resolve(settingsFixture({ speed_profile: "accurate" }));
  await settle();
  assert.equal(page.elements.get("speed-balanced").disabled, false);
});

test("without the app bridge a speed choice updates the page and saves nothing", async () => {
  const page = loadSettingsPage({ answer: null });
  await settle();

  page.elements.get("speed-fast").click();
  await settle();

  assert.equal(pressed(page.elements, "speed-fast"), "true");
  assert.equal(pressed(page.elements, "speed-balanced"), "false");
  assert.equal(page.invocations.some(({ command }) => command === "save_transcription_settings"), false);
});

// --- Segment Pause: save_segment_pause_settings (success/failure/pending
// live in settings-segment-pause.test.mjs; the bridge-less path is
// pinned here alongside the other three saves). ---

test("without the app bridge a pause choice updates the page and saves nothing", async () => {
  const page = loadSettingsPage({ answer: null });
  await settle();

  page.elements.get("pause-8").click();
  await settle();

  assert.equal(pressed(page.elements, "pause-8"), "true");
  assert.equal(pressed(page.elements, "pause-5"), "false");
  assert.equal(page.invocations.some(({ command }) => command === "save_segment_pause_settings"), false);
});

// --- Transcript cleanup: save_transcript_cleanup_settings ---

test("choosing a cleanup mode saves it and the control shows the saved mode", async () => {
  const page = loadSettingsPage({
    answer: async (command) => command === "save_transcript_cleanup_settings"
      ? settingsFixture({ transcript_cleanup: "clean-dictation" })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("cleanup-clean-dictation").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_transcript_cleanup_settings");
  assert.deepEqual({ ...save.args }, { cleanupMode: "clean-dictation" });
  assert.equal(pressed(page.elements, "cleanup-clean-dictation"), "true");
  assert.equal(pressed(page.elements, "cleanup-basic"), "false");
  assert.equal(
    page.elements.get("cleanup-message").textContent,
    "Saved. Applies to your next dictation."
  );
  assert.equal(
    page.elements.get("cleanup-example-typed").textContent,
    "So I think we should ship the beta on Friday. Then collect feedback."
  );
});

test("a save the app refuses puts the previous cleanup mode back and says why", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_transcript_cleanup_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("cleanup-clean-dictation").click();
  await settle();

  assert.equal(pressed(page.elements, "cleanup-basic"), "true");
  assert.equal(pressed(page.elements, "cleanup-clean-dictation"), "false");
  const message = page.elements.get("cleanup-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("while a cleanup mode is being saved the cleanup choices hold", async () => {
  const pending = deferred();
  const page = loadSettingsPage({
    answer: (command) => command === "save_transcript_cleanup_settings" ? pending.promise : new Promise(() => {})
  });
  await settle();

  page.elements.get("cleanup-clean-dictation-with-pause-breaks").click();

  assert.equal(pressed(page.elements, "cleanup-clean-dictation-with-pause-breaks"), "true");
  assert.equal(page.elements.get("cleanup-clean-dictation-with-pause-breaks").disabled, true);
  assert.equal(page.elements.get("cleanup-basic").disabled, true);

  pending.resolve(settingsFixture({ transcript_cleanup: "clean-dictation-with-pause-breaks" }));
  await settle();
  assert.equal(page.elements.get("cleanup-basic").disabled, false);
});

test("without the app bridge a cleanup choice updates the page and saves nothing", async () => {
  const page = loadSettingsPage({ answer: null });
  await settle();

  page.elements.get("cleanup-clean-dictation").click();
  await settle();

  assert.equal(pressed(page.elements, "cleanup-clean-dictation"), "true");
  assert.equal(pressed(page.elements, "cleanup-basic"), "false");
  assert.equal(
    page.elements.get("cleanup-example-typed").textContent,
    "So I think we should ship the beta on Friday. Then collect feedback."
  );
  assert.equal(page.invocations.some(({ command }) => command === "save_transcript_cleanup_settings"), false);
});

// --- Dictation Bar: save_dictation_bar_settings (position, accent,
// display share one command and one pending flag). ---

test("choosing a bar position saves it and the control shows the saved position", async () => {
  const page = loadSettingsPage({
    answer: async (command) => command === "save_dictation_bar_settings"
      ? settingsFixture({ bar_position: "bottom-left" })
      : new Promise(() => {})
  });
  await settle();

  page.elements.get("position-bottom-left").click();
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_dictation_bar_settings");
  assert.deepEqual({ ...save.args }, { barPosition: "bottom-left", accentColor: "red", barDisplay: "primary" });
  assert.equal(pressed(page.elements, "position-bottom-left"), "true");
  assert.equal(pressed(page.elements, "position-bottom-center"), "false");
  assert.equal(page.elements.get("dictation-bar-message").textContent, "Saved.");
});

test("a save the app refuses puts the previous bar position back and says why", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "save_dictation_bar_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  page.elements.get("position-bottom-right").click();
  await settle();

  assert.equal(pressed(page.elements, "position-bottom-center"), "true");
  assert.equal(pressed(page.elements, "position-bottom-right"), "false");
  const message = page.elements.get("dictation-bar-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});

test("while the bar is being saved the bar controls hold", async () => {
  const pending = deferred();
  const page = loadSettingsPage({
    answer: (command) => command === "save_dictation_bar_settings" ? pending.promise : new Promise(() => {})
  });
  await settle();

  page.elements.get("accent-blue").click();

  assert.equal(pressed(page.elements, "accent-blue"), "true");
  assert.equal(page.elements.get("accent-blue").disabled, true);
  assert.equal(page.elements.get("accent-red").disabled, true);
  assert.equal(page.elements.get("position-bottom-center").disabled, true);
  assert.equal(page.elements.get("bar-display").disabled, true);

  pending.resolve(settingsFixture({ accent_color: "blue" }));
  await settle();
  assert.equal(page.elements.get("accent-red").disabled, false);
  assert.equal(page.elements.get("bar-display").disabled, false);
});

test("without the app bridge a bar choice updates the page and saves nothing", async () => {
  const page = loadSettingsPage({ answer: null });
  await settle();

  page.elements.get("position-bottom-left").click();
  await settle();

  assert.equal(pressed(page.elements, "position-bottom-left"), "true");
  assert.equal(pressed(page.elements, "position-bottom-center"), "false");
  assert.equal(page.invocations.some(({ command }) => command === "save_dictation_bar_settings"), false);
});

test("choosing a bar display saves it and the select shows the saved display", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "dictation_bar_displays") {
        return [
          { value: "primary", label: "Main display" },
          { value: { monitor: "Studio Display" }, label: "Studio Display" }
        ];
      }
      if (command === "save_dictation_bar_settings") {
        return settingsFixture({ bar_display: { monitor: "Studio Display" } });
      }
      return new Promise(() => {});
    }
  });
  await settle();

  const select = page.elements.get("bar-display");
  select.value = "monitor:Studio Display";
  select.dispatch("change");
  await settle();

  const save = page.invocations.find(({ command }) => command === "save_dictation_bar_settings");
  assert.deepEqual({ ...save.args.barDisplay }, { monitor: "Studio Display" });
  assert.equal(page.elements.get("bar-display").value, "monitor:Studio Display");
  assert.equal(page.elements.get("dictation-bar-message").textContent, "Saved.");
});

test("a refused bar display save puts the previous display back", async () => {
  const page = loadSettingsPage({
    answer: async (command) => {
      if (command === "dictation_bar_displays") {
        return [
          { value: "primary", label: "Main display" },
          { value: { monitor: "Studio Display" }, label: "Studio Display" }
        ];
      }
      if (command === "save_dictation_bar_settings") throw "Could not write settings: disk full.";
      return new Promise(() => {});
    }
  });
  await settle();

  const select = page.elements.get("bar-display");
  select.value = "monitor:Studio Display";
  select.dispatch("change");
  await settle();

  assert.equal(page.elements.get("bar-display").value, "primary");
  const message = page.elements.get("dictation-bar-message");
  assert.equal(message.textContent, "Could not write settings: disk full.");
  assert.equal(message.classList.contains("error"), true);
});
