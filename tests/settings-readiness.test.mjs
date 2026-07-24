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
    "\nwindow.__slugtaleTest = { loadReadiness, openReadinessAction };\n"
  );
  vm.runInNewContext(testableScript, context);

  return {
    elements,
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
