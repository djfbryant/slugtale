import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { runPage } from "./harness.mjs";

// The settings window is a set of classic scripts that share one global scope;
// `runPage` loads index.html the way a browser does and lets a test name the
// handles it wants to drive directly.
function src(name) {
  return readFileSync(new URL(`../src/${name}`, import.meta.url), "utf8");
}

function load(exports) {
  const { api, elements } = runPage("index.html", { exports });
  return { ...api, elements };
}

// --- Criterion 1: one home for the low-level Tauri invoke ------------------

test("the low-level invoke has one home, and the standalone pages use it", () => {
  const bridge = src("settings/tauri-bridge.js");
  assert.match(
    bridge,
    /\bfunction\s+invoke\s*\(/,
    "tauri-bridge.js owns the shared safe invoke",
  );

  // Neither standalone page may define its own invoke/tauriInvoke any more; both
  // reach Tauri only through the bridge.
  for (const page of ["dictation-bar.js", "typing-challenge.js"]) {
    assert.equal(
      /\bfunction\s+(?:invoke|tauriInvoke)\s*\(/.test(src(page)),
      false,
      `${page} still defines its own invoke/tauriInvoke`,
    );
  }

  // Both pages load the bridge before their own script, so the shared handle is
  // in scope by the time theirs runs.
  assert.match(src("dictation-bar.html"), /settings\/tauri-bridge\.js[\s\S]*dictation-bar\.js/);
  assert.match(src("typing-challenge.html"), /settings\/tauri-bridge\.js[\s\S]*typing-challenge\.js/);
});

// --- Criterion 2: one owner of settings defaults ----------------------------

test("a partial settings answer is filled from one defaults home", () => {
  const { applySettingsDefaults } = load(["applySettingsDefaults"]);

  const merged = applySettingsDefaults({ hotkey: "Cmd+K" });
  assert.equal(merged.hotkey, "Cmd+K");
  // Every omitted key lands on its default from the single home, so no renderer
  // re-applies one inline.
  assert.equal(merged.activation_mode, "toggle");
  assert.equal(merged.speed_profile, "balanced");
  assert.equal(merged.segment_pause_secs, 5);
  assert.equal(merged.bar_position, "bottom-center");
  assert.equal(merged.accent_color, "red");
  assert.equal(merged.primary_engine, "whisper");
  assert.equal(merged.second_opinion, "off");
  assert.equal(merged.transcript_cleanup, "basic");

  assert.equal(applySettingsDefaults(undefined).prefer_built_in_microphone, true);
});

// --- Criterion 3: single-source vocabularies, consumed everywhere -----------

test("panes, positions, accents and second-opinion share one source with the markup", () => {
  const html = src("index.html");
  const { BAR_POSITIONS, ACCENT_COLORS, SECOND_OPINION_MODES, PANES } = load([
    "BAR_POSITIONS", "ACCENT_COLORS", "SECOND_OPINION_MODES", "PANES"
  ]);

  // The pane list drives the rail, the icons, the badges and the sections.
  const railIds = [...html.matchAll(/data-pane="([a-z-]+)"/g)].map((match) => match[1]);
  assert.deepEqual(railIds, [...PANES].map((pane) => pane.id));
  for (const pane of PANES) {
    for (const fragment of [`id="rail-${pane.id}"`, `id="rail-icon-${pane.id}"`, `id="badge-${pane.id}"`, `id="pane-${pane.id}"`]) {
      assert.ok(html.includes(fragment), `index.html is missing ${fragment}`);
    }
  }

  // Each segmented control's buttons are exactly the vocabularies, spelled once.
  const positions = [...html.matchAll(/id="position-(bottom-[\w-]+)"/g)].map((match) => match[1]);
  assert.deepEqual([...positions].sort(), [...BAR_POSITIONS].sort());
  const accents = [...html.matchAll(/id="accent-([\w-]+)"/g)].map((match) => match[1]);
  assert.deepEqual([...accents].sort(), [...ACCENT_COLORS].sort());
  const opinions = [...html.matchAll(/id="second-opinion-([\w-]+)"/g)].map((match) => match[1]);
  assert.deepEqual([...opinions].sort(), [...SECOND_OPINION_MODES].sort());
});

// --- Criterion 5: one progress-percent rule ---------------------------------

test("both progress bars round, clamp and treat no-total the same way", () => {
  const { progressPercent } = load(["progressPercent"]);

  assert.equal(progressPercent(0, null), null, "no total means indeterminate");
  assert.equal(progressPercent(10, null), null);
  assert.equal(progressPercent(0, 200), 0);
  assert.equal(progressPercent(50, 200), 25);
  assert.equal(progressPercent(499, 1000), 50, "rounds to the nearest whole percent");
  assert.equal(progressPercent(200, 100), 100, "never runs past 100");
});

// --- Criterion 5: the download labels the model it is actually fetching ------

test("the download progress names the model it is actually fetching", () => {
  const { elements, renderModel, showProgress } = load(["renderModel", "showProgress"]);

  renderModel({ id: "parakeet", path: "p", present: false, bytes: null });
  showProgress({ downloaded: 50, total: 100 });
  assert.equal(elements.get("model-progress").hidden, false);
  assert.match(elements.get("model-message").textContent, /Downloading parakeet… 50%/);
  assert.equal(elements.get("model-progress").querySelector(".progress-bar").style.width, "50%");

  // With no total it is indeterminate, and it still names the model, not "base.en".
  showProgress({ downloaded: 20 * 1024 * 1024, total: null });
  assert.ok(elements.get("model-progress").classList.contains("indeterminate"));
  assert.match(elements.get("model-message").textContent, /Downloading parakeet… 20 MB/);
  assert.doesNotMatch(elements.get("model-message").textContent, /base\.en/);
});

// --- Criterion 4: derived state lives in the model, not the renderer ---------

test("the engine ordering rule lives in the model", () => {
  const { orderEngines } = load(["orderEngines"]);
  const engines = [
    { id: "whisper", display_name: "Whisper base.en", is_primary: false, unavailable_reason: null },
    { id: "apple-speech", display_name: "Apple", is_primary: true, unavailable_reason: "macOS 26" },
    { id: "parakeet", display_name: "Parakeet", is_primary: false, unavailable_reason: null },
    { id: "phonon", display_name: "Phonon", is_primary: false, unavailable_reason: "no assets" }
  ];
  assert.deepEqual(
    [...orderEngines(engines)].map((engine) => engine.id),
    ["apple-speech", "parakeet", "whisper", "phonon"],
  );
  // Ordering must not mutate the caller's list.
  assert.equal(engines[0].id, "whisper");
});

test("the usage derivations live in the model, not the renderer", () => {
  const { usageModel } = load(["usageModel"]);
  const base = {
    store_usage: true,
    today: { dictations: 2, words: 240, time_saved: "About 4 min" },
    this_week: { dictations: 9, words: 1100, time_saved: "About 18 min" },
    all_time: { dictations: 40, words: 5200, time_saved: "About 1 hr 25 min" },
    measured_wpm: 62,
    typed_estimate: null,
    completed_challenges: 3,
    challenge_count: 3
  };

  const measured = usageModel(base);
  assert.equal(measured.measured, true);
  assert.equal(measured.hasBaseline, true);
  assert.equal(measured.wpm, 62);
  assert.equal(measured.allCounts, "40 dictations · 5200 words");
  assert.equal(measured.todayCounts, "2 dictations · 240 words");

  const none = usageModel({ ...base, measured_wpm: null, typed_estimate: null, completed_challenges: 0 });
  assert.equal(none.hasBaseline, false);
  assert.equal(none.wpm, null);
  assert.equal(none.baselineStateText, "Not measured yet.");
  assert.equal(none.baselineButtonLabel, "Measure my typing speed");
  assert.equal(none.hasEstimate, false);
  assert.equal(none.estimateValue, "");

  const estimated = usageModel({ ...base, measured_wpm: null, typed_estimate: 45, completed_challenges: 0 });
  assert.equal(estimated.measured, false);
  assert.equal(estimated.hasBaseline, true);
  assert.match(estimated.baselineStateText, /45 words per minute, your estimate/);
  assert.equal(estimated.hasEstimate, true);
  assert.equal(estimated.estimateValue, "45");

  const partway = usageModel({ ...base, measured_wpm: null, typed_estimate: null, completed_challenges: 2 });
  assert.equal(partway.baselineButtonLabel, "Continue typing challenge (2 of 3)");
  assert.equal(partway.baselineStateText, "2 of 3 typing challenges done.");

  assert.equal(usageModel({ ...base, store_usage: false }).storing, false);
});
