import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
const workerSource = readFileSync(
  new URL("../src-tauri/src/voice_activation.rs", import.meta.url),
  "utf8",
);

test("unsupported builds hide voice activation instead of claiming to listen", () => {
  assert.match(settingsHtml, /invoke\("voice_activation_supported"\)/);
  assert.match(settingsHtml, /voiceRow\.hidden = !voiceActivationSupported/);
  assert.match(settingsHtml, /voiceToggle\.disabled = savingVoiceActivation \|\| !voiceActivationSupported/);
});

test("voice activation tells the user to wait for the dictation bar", () => {
  assert.match(settingsHtml, /Wait for the dictation bar, then talk\./);
  assert.doesNotMatch(settingsHtml, /Say it, then keep talking\./);
});

test("voice activation reports a blocked or silent microphone", () => {
  assert.match(workerSource, /PlatformReadiness::microphone_granted/);
  assert.match(workerSource, /NewAudioState::DigitalSilence/);
  assert.match(workerSource, /report_voice_activation_microphone_problem/);
});

test("the listener rebuilds capture after dictation or digital silence", () => {
  assert.match(workerSource, /VoiceActivationCapture::new/);
  assert.match(workerSource, /target_is_dictating/);
  assert.match(workerSource, /whisper_ready/);
  assert.match(workerSource, /wait_or_stop/);
  const rebuilds = workerSource.match(/capture\.rebuild\(CpalAudioRecorder::new\(\)\)/g) ?? [];
  assert.ok(
    rebuilds.length >= 4,
    "dictation, start failure, capture failure, and digital silence must each rebuild",
  );
});
