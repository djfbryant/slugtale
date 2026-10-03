import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

function loadSettingsScript({ invoke }) {
  const { api, elements } = runPage("index.html", {
    exports: ["saveMicrophonePreference"],
    invoke,
    userAgent: "Mozilla/5.0 (X11; Linux x86_64)"
  });
  return { elements, ...api };
}

test("turning the built-in microphone off saves it and shows the saved value", async () => {
  const calls = [];
  const { elements, saveMicrophonePreference } = loadSettingsScript({
    async invoke(command, args) {
      calls.push({ command, args: { ...args } });
      if (command === "save_microphone_settings") {
        return { prefer_built_in_microphone: args.preferBuiltInMicrophone };
      }
      return {};
    }
  });

  await saveMicrophonePreference(false);

  const call = calls.find((entry) => entry.command === "save_microphone_settings");
  assert.deepEqual(call.args, { preferBuiltInMicrophone: false });
  const toggle = elements.get("prefer-built-in-microphone-toggle");
  assert.equal(toggle.checked, false);
  assert.equal(toggle.disabled, false);
});

test("a failed microphone save puts the switch back and says why", async () => {
  const { elements, saveMicrophonePreference } = loadSettingsScript({
    async invoke(command) {
      if (command === "save_microphone_settings") {
        throw new Error("settings file is read-only");
      }
      return {};
    }
  });

  await saveMicrophonePreference(false);

  assert.equal(elements.get("prefer-built-in-microphone-toggle").checked, true);
  assert.equal(
    elements.get("microphone-message").textContent,
    "Error: settings file is read-only"
  );
});
