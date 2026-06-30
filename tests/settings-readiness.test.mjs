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
    "\nwindow.__slugtaleTest = { openReadinessAction };\n"
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

  assert.equal(
    elements.get("settings-message").textContent,
    "Still not granted. Grant access in macOS Privacy & Security, then reopen this window."
  );
});
