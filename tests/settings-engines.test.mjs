import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

// A stand-in for one row of the `transcription_engines` command's output: just
// the fields the engine list reads.
function engineFixture(id, displayName, { primary = false, unavailableReason = null, capability = "" } = {}) {
  return {
    id,
    display_name: displayName,
    is_primary: primary,
    metadata: { engine: id, model_id: `${id}-model`, capability },
    unavailable_reason: unavailableReason,
    installable: false,
    assets: { installed_bytes: null, present: true }
  };
}

function renderEngines(engines) {
  const { api, elements } = runPage("index.html", { exports: ["renderEngines"] });
  api.renderEngines(engines, "");
  return [...elements.get("engine-list").children];
}

test("the selected engine is listed first, then the available ones alphabetically", () => {
  const rows = renderEngines([
    engineFixture("whisper", "Whisper base.en"),
    engineFixture("parakeet", "Parakeet TDT v2"),
    engineFixture("phonon", "Phonon-2"),
    engineFixture("apple-speech", "Apple SpeechTranscriber", {
      primary: true,
      unavailableReason: "Requires macOS 26 or later."
    })
  ]);

  assert.deepEqual(
    rows.map((row) => row.dataset.engine),
    ["apple-speech", "parakeet", "phonon", "whisper"]
  );
});

test("a selection never undoes itself by alphabetical position", () => {
  const rows = renderEngines([
    engineFixture("whisper", "Whisper base.en"),
    engineFixture("parakeet", "Parakeet TDT v2", { primary: true }),
    engineFixture("phonon", "Phonon-2")
  ]);

  assert.deepEqual(
    rows.map((row) => row.dataset.engine),
    ["parakeet", "phonon", "whisper"]
  );
});

test("an engine that cannot run sits below the ones the user can switch to", () => {
  const rows = renderEngines([
    engineFixture("whisper", "Whisper base.en", { primary: true }),
    engineFixture("parakeet", "Parakeet TDT v2", { unavailableReason: "Model not installed." }),
    engineFixture("phonon", "Phonon-2"),
    engineFixture("apple-speech", "Apple SpeechTranscriber", { unavailableReason: "Requires macOS 26 or later." })
  ]);

  assert.deepEqual(
    rows.map((row) => row.dataset.engine),
    ["whisper", "phonon", "apple-speech", "parakeet"]
  );
});

test("each row states what its engine is good at, straight from the engine", () => {
  const parakeetCapabilities = "NVIDIA's fast, accurate English transcriber.";
  const whisperCapabilities = "General-purpose English dictation on a modest model.";
  const rows = renderEngines([
    engineFixture("parakeet", "Parakeet TDT v2", { capability: parakeetCapabilities }),
    engineFixture("whisper", "Whisper base.en", { capability: whisperCapabilities })
  ]);

  const expected = { parakeet: parakeetCapabilities, whisper: whisperCapabilities };
  for (const row of rows) {
    const blurb = row.querySelector(".engine-blurb");
    assert.ok(blurb, `row ${row.dataset.engine} has a capability description`);
    assert.equal(blurb.textContent, expected[row.dataset.engine]);
  }
});
