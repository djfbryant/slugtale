import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

function loadSettingsScript({ invoke }) {
  const { api, elements, flushNextTimeout } = runPage("index.html", {
    exports: ["loadReadiness", "openReadinessAction", "saveDictationBarSettings", "saveEngineSettings"],
    invoke,
    userAgent: "Mozilla/5.0 (X11; Linux x86_64)"
  });

  return { elements, ...api, flushNextTimer: flushNextTimeout };
}

test("readiness permission action ignores repeated requests while polling", async () => {
  const commands = [];
  const { openReadinessAction } = loadSettingsScript({
    async invoke(command) {
      commands.push(command);
      return {};
    }
  });

  openReadinessAction("microphone");
  await Promise.resolve();
  await Promise.resolve();

  openReadinessAction("microphone");
  await Promise.resolve();

  assert.deepEqual(commands, ["open_microphone_settings"]);
});

test("readiness permission action shows not-yet-granted message after polling timeout", async () => {
  const report = {
    dictation_available: false,
    items: [
      {
        id: "microphone",
        label: "Microphone",
        ready: false,
        required: true
      }
    ]
  };
  const { elements, flushNextTimer, openReadinessAction } = loadSettingsScript({
    async invoke(command) {
      if (command === "open_microphone_settings") return {};
      return report;
    }
  });

  const action = openReadinessAction("microphone");
  await Promise.resolve();
  await Promise.resolve();

  for (let attempt = 0; attempt < 12; attempt += 1) {
    await flushNextTimer();
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  }
  await action;

  assert.equal(
    elements.get("settings-message").textContent,
    "Still not ready. Connect a microphone or switch to an X11 session, then reopen this window."
  );
});

test("background readiness refresh does not overwrite active permission polling render", async () => {
  const staleReport = {
    dictation_available: false,
    items: [
      {
        id: "microphone",
        label: "Microphone",
        ready: false,
        required: true
      }
    ]
  };
  const readyReport = {
    dictation_available: true,
    items: [
      {
        id: "microphone",
        label: "Microphone",
        ready: true,
        required: true
      }
    ]
  };
  let captureNextReadinessAsBackground = false;
  let resolveBackgroundReadiness;

  const { elements, flushNextTimer, loadReadiness, openReadinessAction } = loadSettingsScript({
    async invoke(command) {
      if (command === "open_microphone_settings") return {};
      if (captureNextReadinessAsBackground) {
        return new Promise((resolve) => {
          resolveBackgroundReadiness = resolve;
        });
      }
      return readyReport;
    }
  });

  const action = openReadinessAction("microphone");
  await Promise.resolve();
  await Promise.resolve();

  captureNextReadinessAsBackground = true;
  const backgroundRefresh = loadReadiness({ background: true });
  await Promise.resolve();
  captureNextReadinessAsBackground = false;

  await flushNextTimer();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();

  assert.equal(elements.get("overall-status").textContent, "Ready to dictate");

  if (resolveBackgroundReadiness) {
    resolveBackgroundReadiness(staleReport);
  }
  await backgroundRefresh;

  assert.equal(elements.get("overall-status").textContent, "Ready to dictate");
  await action;
});

test("a blocked transcription engine shows the reason the backend reported", async () => {
  const report = {
    dictation_available: false,
    items: [
      { id: "local_model", label: "Local model", ready: true, required: true, pane: "transcription" },
      {
        id: "transcription_engine",
        label: "Transcription engine",
        ready: false,
        required: true,
        pane: "transcription",
        detail: "Whisper base.en cannot run: this build was compiled without support for this engine"
      }
    ]
  };
  const { elements, loadReadiness } = loadSettingsScript({
    async invoke(command) {
      if (command === "get_settings_readiness") return report;
      return {};
    }
  });

  await loadReadiness();

  const row = elements.get("readiness-list").children.at(-1);
  const guidanceText = row.children
    .flatMap((child) => child.children || [])
    .find((child) => child.tagName === "SMALL");

  assert.equal(elements.get("overall-status").textContent, "Not ready to dictate");
  assert.equal(
    guidanceText.textContent,
    "Whisper base.en cannot run: this build was compiled without support for this engine"
  );
});

test("changing one dictation bar setting still sends the complete appearance the backend expects", async () => {
  const calls = [];
  const { saveDictationBarSettings } = loadSettingsScript({
    async invoke(command, args) {
      calls.push({ command, args: { ...args } });
      if (command === "save_dictation_bar_settings") {
        return { ...args, bar_position: args.barPosition, accent_color: args.accentColor };
      }
      return {};
    }
  });

  await saveDictationBarSettings({ accentColor: "violet" });

  assert.deepEqual(calls.at(-1), {
    command: "save_dictation_bar_settings",
    args: {
      barPosition: "bottom-center",
      accentColor: "violet",
      barDisplay: "primary"
    }
  });
});

test("choosing a display sends it with the current bar appearance", async () => {
  const calls = [];
  const { saveDictationBarSettings } = loadSettingsScript({
    async invoke(command, args) {
      calls.push({ command, args: { ...args } });
      if (command === "save_dictation_bar_settings") {
        return {
          ...args,
          bar_position: args.barPosition,
          accent_color: args.accentColor,
          bar_display: args.barDisplay
        };
      }
      return {};
    }
  });

  await saveDictationBarSettings({ barDisplay: { monitor: "Studio Display" } });

  assert.deepEqual(calls.at(-1), {
    command: "save_dictation_bar_settings",
    args: {
      barPosition: "bottom-center",
      accentColor: "red",
      barDisplay: { monitor: "Studio Display" }
    }
  });
});

test("a failed dictation bar save reports the error instead of pretending it stuck", async () => {
  const { elements, saveDictationBarSettings } = loadSettingsScript({
    async invoke(command) {
      if (command === "save_dictation_bar_settings") throw new Error("settings file is read-only");
      return {};
    }
  });

  await saveDictationBarSettings({ barPosition: "bottom-right" });

  assert.equal(
    elements.get("dictation-bar-message").textContent,
    "Error: settings file is read-only"
  );
});

test("changing the primary engine still sends the current second opinion mode", async () => {
  const calls = [];
  const { saveEngineSettings } = loadSettingsScript({
    async invoke(command, args) {
      calls.push({ command, args: { ...args } });
      if (command === "set_transcription_engines") {
        return {
          primary_engine: args.primaryEngine,
          second_opinion: args.secondOpinion
        };
      }
      if (command === "transcription_engines") return [];
      return {};
    }
  });

  await saveEngineSettings({ primaryEngine: "parakeet" });

  const call = calls.find((entry) => entry.command === "set_transcription_engines");
  assert.deepEqual(call.args, { primaryEngine: "parakeet", secondOpinion: "off" });
});

test("choosing Automatic second opinion still sends the current primary engine", async () => {
  const calls = [];
  const { saveEngineSettings } = loadSettingsScript({
    async invoke(command, args) {
      calls.push({ command, args: { ...args } });
      if (command === "set_transcription_engines") {
        return {
          primary_engine: args.primaryEngine,
          second_opinion: args.secondOpinion
        };
      }
      if (command === "transcription_engines") return [];
      return {};
    }
  });

  await saveEngineSettings({ secondOpinion: "automatic" });

  const call = calls.find((entry) => entry.command === "set_transcription_engines");
  assert.deepEqual(call.args, { primaryEngine: "whisper", secondOpinion: "automatic" });
});

test("a failed engine settings save reports the error instead of pretending it stuck", async () => {
  const { elements, saveEngineSettings } = loadSettingsScript({
    async invoke(command) {
      if (command === "set_transcription_engines") {
        throw new Error("settings file is read-only");
      }
      return [];
    }
  });

  await saveEngineSettings({ primaryEngine: "parakeet" });

  assert.equal(
    elements.get("engine-message").textContent,
    "Error: settings file is read-only"
  );
});
