import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { shippedSource } from "./harness.mjs";

// The toggle lives in the page's markup; the code that keeps it off is in the
// script it ships. Both are read, because the guarantee is about the page the user
// gets rather than about either file alone.
const settings = shippedSource("index.html");
const settingsHtml = settings.all;
const workerSource = readFileSync(
  new URL("../src-tauri/src/voice_activation.rs", import.meta.url),
  "utf8",
);
const listenLoopSource = readFileSync(
  new URL("../src-tauri/src/listen_loop.rs", import.meta.url),
  "utf8",
);

test("settings marks voice activation as coming soon and keeps it off", () => {
  assert.match(settingsHtml, /Coming soon\. Start dictation with your hotkey for now\./);
  assert.match(
    settingsHtml,
    /id="voice-activation-toggle"[^>]*aria-label="Voice activation, coming soon"[^>]*disabled/,
  );
  assert.match(settingsHtml, /voiceToggle\.checked = false/);
  assert.match(settingsHtml, /voiceToggle\.disabled = true/);
  assert.doesNotMatch(settingsHtml, /voiceRow\.hidden/);
});

test("voice activation reports a blocked or silent microphone", () => {
  assert.match(workerSource, /CurrentPlatform::new\(\)\.microphone_granted\(\)/);
  assert.match(listenLoopSource, /NewAudioState::DigitalSilence/);
  assert.match(workerSource, /report_voice_activation_microphone_problem/);
});

test("the listen loop lives in lib and the adapter answers its ports", () => {
  // The decision loop is platform-independent and unit-tested there; the
  // macOS tier only implements WakeListener against the app handle.
  assert.match(listenLoopSource, /fn run_listen_loop/);
  assert.match(listenLoopSource, /trait WakeListener/);
  assert.match(workerSource, /impl slugtale_lib::WakeListener for AppWakeListener/);
  assert.match(workerSource, /run_listen_loop\(&mut AppWakeListener::new/);
  const rebuilds = workerSource.match(/rebuild\(slugtale_lib::CpalAudioRecorder::new\(\)\)/g) ?? [];
  assert.ok(
    rebuilds.length >= 1,
    "the adapter must expose capture rebuilds to the loop",
  );
});
