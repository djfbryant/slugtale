import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function loadSettingsScript({ invoke }) {
  const html = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
  const [, script] = html.match(/<script>\s*([\s\S]*?)\s*<\/script>/);
  const elements = new Map();
  const timers = [];

  function createElement(tagName, id = "") {
    return {
      id,
      tagName,
      children: [],
      className: "",
      classList: {
        add() {},
        remove() {},
        toggle() {}
      },
      dataset: {},
      disabled: false,
      hidden: false,
      innerHTML: "",
      style: {
        removeProperty() {}
      },
      textContent: "",
      value: "",
      addEventListener() {},
      // The settings window drives segmented controls through aria-pressed and
      // focuses the hotkey field when capture starts; both are no-ops here.
      setAttribute() {},
      focus() {},
      append(...children) {
        this.children.push(...children);
      },
      querySelector() {
        return createElement("div");
      },
      replaceChildren(...children) {
        this.children = children;
      }
    };
  }

  function element(id) {
    if (!elements.has(id)) elements.set(id, createElement("div", id));
    return elements.get(id);
  }

  const context = {
    console,
    navigator: { userAgent: "Mozilla/5.0 (X11; Linux x86_64)" },
    document: {
      createElement,
      getElementById: element
    },
    setTimeout(callback) {
      timers.push(callback);
      return timers.length;
    },
    window: {
      __TAURI__: {
        core: { invoke }
      }
    }
  };
  context.globalThis = context;

  const testableScript = script.replace(
    /\s*init\(\);\s*$/,
    "\nwindow.__slugtaleTest = { loadReadiness, openReadinessAction, saveDictationBarSettings, saveEngineSettings };\n"
  );
  vm.runInNewContext(testableScript, context);

  return {
    elements,
    saveDictationBarSettings: context.window.__slugtaleTest.saveDictationBarSettings,
    saveEngineSettings: context.window.__slugtaleTest.saveEngineSettings,
    async flushNextTimer() {
      for (let spin = 0; timers.length === 0 && spin < 10; spin += 1) {
        await Promise.resolve();
      }
      const callback = timers.shift();
      if (!callback) throw new Error("No pending timer to flush");
      callback();
    },
    loadReadiness: context.window.__slugtaleTest.loadReadiness,
    openReadinessAction: context.window.__slugtaleTest.openReadinessAction
  };
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

  // The harness runs with a Linux (X11) navigator, so the copy is the
  // platform-aware Linux variant (guidance rather than an OS permission grant).
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

  assert.equal(elements.get("overall-status").textContent, "Ready");

  if (resolveBackgroundReadiness) {
    resolveBackgroundReadiness(staleReport);
  }
  await backgroundRefresh;

  assert.equal(elements.get("overall-status").textContent, "Ready");
  await action;
});

test("changing one dictation bar setting still sends the pair the backend expects", async () => {
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

  // The accent moved; the position came along at its current value rather than
  // being sent as undefined and cleared.
  assert.deepEqual(calls.at(-1), {
    command: "save_dictation_bar_settings",
    args: { barPosition: "bottom-center", accentColor: "violet" }
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

  // The primary engine moved; Second Opinion (Off by default) came along at
  // its current value rather than being sent as undefined.
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
