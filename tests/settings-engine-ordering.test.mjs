import assert from "node:assert/strict";
import test from "node:test";
import { runPage } from "./harness.mjs";

function engine(id, name, { selected = false, unavailable = false } = {}) {
  return {
    id,
    display_name: name,
    is_primary: selected,
    unavailable_reason: unavailable ? "Not installed" : null,
    metadata: { license: "test", supported_platforms: "test" },
    assets: { present: false },
    installable: unavailable,
  };
}

test("Transcription lists the selection, available engines A–Z, then unavailable engines", () => {
  const { api, elements } = runPage("index.html", { exports: ["renderEngines"] });
  const engines = [
    engine("whisper", "Whisper"),
    engine("uninstalled", "A missing model", { unavailable: true }),
    engine("phonon", "Phonon-2", { selected: true }),
    engine("parakeet", "Parakeet"),
    engine("apple", "Apple SpeechTranscriber"),
  ];
  api.renderEngines(engines);
  assert.deepEqual(elements.get("engine-list").children.map(row => row.dataset.engine),
    ["phonon", "apple", "parakeet", "whisper", "uninstalled"]);
  assert.deepEqual(engines.map(item => item.id),
    ["whisper", "uninstalled", "phonon", "parakeet", "apple"]);

  api.renderEngines(engines.map(item => ({ ...item, is_primary: item.id === "uninstalled" })));
  assert.deepEqual(elements.get("engine-list").children.map(row => row.dataset.engine),
    ["uninstalled", "apple", "parakeet", "phonon", "whisper"]);
});
