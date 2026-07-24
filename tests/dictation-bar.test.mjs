import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// The Dictation Bar runs as a plain script inside a transparent Tauri window.
// These tests load that script against a hand-rolled DOM so the bar's behaviour —
// what it expands for, what it hands back to the app underneath, what it paints —
// is checked without a running app.
function loadDictationBar({ invoke = async () => false, reduceMotion = false } = {}) {
  const html = readFileSync(new URL("../src/dictation-bar.html", import.meta.url), "utf8");
  const [, script] = html.match(/<script>\s*([\s\S]*?)\s*<\/script>/);

  const elements = new Map();
  const documentListeners = new Map();
  const invocations = [];
  const frames = [];
  const intervals = [];
  const rootStyle = new Map();

  function createElement(id = "") {
    const listeners = new Map();
    return {
      id,
      dataset: {},
      disabled: false,
      hidden: false,
      textContent: "",
      style: {},
      listeners,
      addEventListener(type, handler) {
        listeners.set(type, handler);
      },
      click() {
        const handler = listeners.get("click");
        if (handler) handler({ preventDefault() {} });
      }
    };
  }

  function element(id) {
    if (!elements.has(id)) elements.set(id, createElement(id));
    return elements.get(id);
  }

  const body = createElement("body");

  const context = {
    console,
    Date,
    Math,
    Number,
    String,
    Promise,
    document: {
      body,
      documentElement: {
        style: {
          setProperty(name, value) {
            rootStyle.set(name, value);
          }
        }
      },
      getElementById: element,
      querySelector: element,
      addEventListener(type, handler) {
        documentListeners.set(type, handler);
      }
    },
    requestAnimationFrame(callback) {
      frames.push(callback);
      return frames.length;
    },
    setInterval(callback, delay) {
      intervals.push({ callback, delay });
      return intervals.length;
    },
    clearInterval() {},
    window: {
      matchMedia: () => ({ matches: reduceMotion, addEventListener() {} }),
      __TAURI__: {
        core: {
          async invoke(command, args) {
            // Rebuilt in this realm: objects made inside the vm carry their own
            // prototypes, which deepEqual refuses to match.
            invocations.push({ command, args: { ...args } });
            return invoke(command, args);
          }
        },
        event: { listen() {} }
      }
    }
  };
  context.globalThis = context;
  context.window.document = context.document;

  // The bar ships no test seam of its own, so the handles are appended here —
  // the same trick tests/settings-readiness.test.mjs uses on the settings window.
  const testableScript = `${script}
window.__slugtaleBar = {
  setPhase, setAppearance, setAudioLevel, setVisible, pollPointer, renderFrame,
  isPolling: () => pointerPoll !== null
};`;
  vm.runInNewContext(testableScript, context);

  return {
    body,
    elements,
    invocations,
    intervals,
    rootStyle,
    keydown: (key) => documentListeners.get("keydown")({ key, preventDefault() {} }),
    leaveBar: () => elements.get(".bar").listeners.get("mouseleave")(),
    api: context.window.__slugtaleBar
  };
}

test("the bar rests as an orb and expands for the whole transcribing phase", () => {
  const bar = loadDictationBar();

  bar.api.setPhase("recording");
  assert.equal(bar.body.dataset.expanded, "false");

  bar.api.setPhase("transcribing");
  assert.equal(bar.body.dataset.expanded, "true");
  assert.equal(bar.body.dataset.phase, "transcribing");
});

test("hovering the orb expands it, leaving it collapses it again", async () => {
  let over = true;
  const bar = loadDictationBar({ invoke: async () => over });

  await bar.api.pollPointer();
  assert.equal(bar.body.dataset.expanded, "true");

  over = false;
  await bar.api.pollPointer();
  assert.equal(bar.body.dataset.expanded, "false");
});

test("the pointer poll tells the backend which shape to hit-test against", async () => {
  // The window is always sized for the expanded pill, so the backend cannot know
  // how much of it is painted unless the bar says so.
  const bar = loadDictationBar({ invoke: async () => false });

  await bar.api.pollPointer();
  assert.deepEqual(bar.invocations.at(-1), {
    command: "dictation_bar_pointer_over",
    args: { expanded: false }
  });

  bar.api.setPhase("transcribing");
  await bar.api.pollPointer();
  assert.deepEqual(bar.invocations.at(-1), {
    command: "dictation_bar_pointer_over",
    args: { expanded: true }
  });
});

test("a transcribing bar stays expanded even when the pointer is elsewhere", async () => {
  const bar = loadDictationBar({ invoke: async () => false });

  bar.api.setPhase("transcribing");
  await bar.api.pollPointer();

  assert.equal(bar.body.dataset.expanded, "true");
});

test("the accent arrives as a custom property, never as markup", () => {
  const bar = loadDictationBar();

  bar.api.setAppearance({ position: "bottom-right", accent: "violet" });

  assert.equal(bar.rootStyle.get("--accent"), "#a78bfa");
  assert.equal(bar.body.dataset.position, "bottom-right");
  // Nothing the backend sent is written into the document as text.
  assert.equal(bar.elements.get("label").innerHTML, undefined);
});

test("an unknown accent or position falls back rather than painting nothing", () => {
  const bar = loadDictationBar();

  bar.api.setAppearance({ position: "top-middle", accent: "chartreuse" });

  assert.equal(bar.rootStyle.get("--accent"), "#ff5a52");
  assert.equal(bar.body.dataset.position, "bottom-center");
});

test("escape cancels a recording and is inert once transcription has started", () => {
  const bar = loadDictationBar();

  bar.api.setPhase("recording");
  bar.keydown("Escape");
  assert.deepEqual(bar.invocations.at(-1), {
    command: "dictation_event",
    args: { event: "cancel" }
  });

  const before = bar.invocations.length;
  bar.api.setPhase("transcribing");
  bar.keydown("Escape");
  assert.equal(bar.invocations.length, before);
});

test("stop and cancel still reach the backend", () => {
  const bar = loadDictationBar();

  bar.elements.get("stop").click();
  assert.deepEqual(bar.invocations.at(-1), {
    command: "dictation_event",
    args: { event: "stop" }
  });

  bar.elements.get("cancel").click();
  assert.deepEqual(bar.invocations.at(-1), {
    command: "dictation_event",
    args: { event: "cancel" }
  });
});

test("the halo reads from the voice level alone, with no idle drift", () => {
  const bar = loadDictationBar();

  bar.api.setPhase("recording");
  bar.api.setAudioLevel(0);
  // Settle the smoothing, then confirm the shape stops moving on silence.
  for (let frame = 0; frame < 60; frame += 1) bar.api.renderFrame(frame * 16);
  const silent = bar.elements.get("halo").style.transform;
  bar.api.renderFrame(1000);
  assert.equal(bar.elements.get("halo").style.transform, silent);

  bar.api.setAudioLevel(0.9);
  for (let frame = 0; frame < 60; frame += 1) bar.api.renderFrame(frame * 16);
  assert.notEqual(bar.elements.get("halo").style.transform, silent);
});

test("the elapsed clock counts the current phase", () => {
  const bar = loadDictationBar();

  bar.api.setPhase("recording", 1_000);
  bar.api.renderFrame(0, 1_000 + 67_000);

  assert.equal(bar.elements.get("elapsed").textContent, "1:07");
});

test("reduced motion leaves the halo alone", () => {
  const bar = loadDictationBar({ reduceMotion: true });

  bar.api.setAudioLevel(0.9);
  bar.api.renderFrame(16);

  assert.equal(bar.elements.get("halo").style.transform, undefined);
});

test("the bar only watches the pointer while it is on screen", async () => {
  const bar = loadDictationBar({ invoke: async () => false });

  // The window is hidden between dictations and the webview keeps running, so
  // polling before it is shown would ping the backend all day for nothing.
  assert.equal(bar.api.isPolling(), false);
  assert.equal(bar.intervals.length, 0);

  bar.api.setVisible(true);
  assert.equal(bar.api.isPolling(), true);
  assert.equal(bar.intervals.at(-1).delay, 100);

  bar.api.setVisible(false);
  assert.equal(bar.api.isPolling(), false);
});

test("hiding the bar forgets a hover it can no longer see end", async () => {
  const bar = loadDictationBar({ invoke: async () => true });

  bar.api.setVisible(true);
  await bar.api.pollPointer();
  assert.equal(bar.body.dataset.expanded, "true");

  bar.api.setVisible(false);
  assert.equal(bar.body.dataset.expanded, "false");
});

test("leaving the orb hands clicks back at once, not on the next poll", async () => {
  // Waiting for the tick would leave a window where the pointer has moved off the
  // orb and the transparent surround is still swallowing clicks.
  const bar = loadDictationBar({ invoke: async () => true });

  bar.api.setVisible(true);
  await bar.api.pollPointer();
  assert.equal(bar.body.dataset.expanded, "true");

  const before = bar.invocations.length;
  await bar.leaveBar();

  assert.equal(bar.invocations.length, before + 1);
  assert.equal(bar.invocations.at(-1).command, "dictation_bar_pointer_over");
});
