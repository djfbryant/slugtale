import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function loadSettingsScript({ invoke }) {
  const html = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
  const [, script] = html.match(/<script>\s*([\s\S]*?)\s*<\/script>/);
  const elements = new Map();

  function element(id) {
    if (!elements.has(id)) {
      elements.set(id, {
        id,
        classList: { toggle() {} },
        disabled: false,
        textContent: "",
        value: ""
      });
    }
    return elements.get(id);
  }

  const context = {
    console,
    document: {
      getElementById: element
    },
    setTimeout() {},
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
