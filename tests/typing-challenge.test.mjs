import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const challengeHtml = readFileSync(new URL("../src/typing-challenge.html", import.meta.url), "utf8");

// Let every pending promise settle. The window loads its state asynchronously on
// open and again after each score, and counting microtasks by hand is brittle.
function flush() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// The window is its own page with its own script, so the harness stands up a
// small DOM and a fake clock: the whole point of the challenge is that thirty
// seconds elapse, and no test should actually wait for them.
function loadChallengeScript({ invoke, now = { value: 1_000_000 } }) {
  const [, script] = challengeHtml.match(/<script>\s*([\s\S]*?)\s*<\/script>/);
  const elements = new Map();
  const intervals = [];

  function createElement(tagName, id = "") {
    const node = {
      id,
      tagName,
      classList: { add() {}, remove() {}, toggle() {} },
      dataset: {},
      disabled: false,
      hidden: false,
      innerHTML: "",
      textContent: "",
      value: "",
      listeners: {},
      addEventListener(type, handler) {
        this.listeners[type] = handler;
      },
      focus() {},
      // The passage is rendered as one span per word, keyed by index, so the
      // typed-word marking can be read back the way the user would see it.
      querySelector(selector) {
        const match = selector.match(/\[data-index="(\d+)"\]/);
        if (!match) return null;
        const index = Number(match[1]);
        if (!node.words) node.words = new Map();
        if (!node.words.has(index)) node.words.set(index, createElement("span"));
        return node.words.get(index);
      }
    };
    return node;
  }

  function element(id) {
    if (!elements.has(id)) elements.set(id, createElement("div", id));
    return elements.get(id);
  }

  const context = {
    console,
    Date: { now: () => now.value },
    setInterval(callback) {
      intervals.push(callback);
      return intervals.length;
    },
    clearInterval() {},
    document: { createElement, getElementById: element },
    window: { __TAURI__: { core: { invoke } }, close() {} }
  };
  context.globalThis = context;

  vm.runInNewContext(script, context);

  return {
    elements,
    typing: element("typing"),
    // Type into the box the way a person would: set the value, then fire input.
    type(text) {
      const box = element("typing");
      box.value = text;
      box.listeners.input({ target: box });
    },
    tick() {
      intervals.forEach((callback) => callback());
    },
    click(id) {
      element(id).listeners.click();
    }
  };
}

const state = {
  passage: "the quick brown fox jumps over the lazy dog",
  passage_index: 0,
  completed: 0,
  total: 3,
  seconds: 30,
  measured_wpm: null
};

test("the clock does not start until the user starts typing", async () => {
  // Reading the passage must not cost the user any of their thirty seconds.
  const now = { value: 1_000_000 };
  const { elements, tick } = loadChallengeScript({
    async invoke() {
      return state;
    },
    now
  });
  await flush();

  assert.equal(elements.get("clock").textContent, "30");
  assert.equal(elements.get("clock").dataset.running, "false");

  // Thirty seconds of reading pass. The clock still says thirty.
  now.value += 30_000;
  tick();
  assert.equal(elements.get("clock").textContent, "30");
});

test("typing starts the clock and the deadline is wall-clock, not a tick count", async () => {
  // A throttled timer or a sleeping machine must not hand out extra seconds.
  const now = { value: 1_000_000 };
  const submitted = [];
  const { elements, type, tick } = loadChallengeScript({
    async invoke(command, args) {
      if (command === "submit_typing_challenge") {
        submitted.push(args);
        return { ...state, passage_index: 1, completed: 1 };
      }
      return state;
    },
    now
  });
  await flush();

  type("the quick");
  assert.equal(elements.get("clock").dataset.running, "true");

  now.value += 10_000;
  tick();
  assert.equal(elements.get("clock").textContent, "20");

  // One tick after a long stall still ends the run rather than counting down.
  now.value += 100_000;
  tick();
  await flush();
  await Promise.resolve();

  assert.equal(elements.get("clock").textContent, "0");
  assert.equal(submitted.length, 1);
  assert.equal(submitted[0].passageIndex, 0);
  assert.equal(submitted[0].typed, "the quick");
});

test("the passage marks words right and wrong as they are typed, in order", async () => {
  const { elements, type } = loadChallengeScript({
    async invoke() {
      return state;
    }
  });
  await flush();

  const passage = elements.get("passage");
  type("the qiuck brown ");

  assert.equal(passage.querySelector('[data-index="0"]').dataset.state, "correct");
  assert.equal(passage.querySelector('[data-index="1"]').dataset.state, "wrong");
  assert.equal(passage.querySelector('[data-index="2"]').dataset.state, "correct");
  // The word being typed is not yet judged — otherwise every word flashes wrong
  // on its first letter.
  assert.equal(passage.querySelector('[data-index="3"]').dataset.state, "current");
});

test("a half-typed word is not called wrong before it is finished", async () => {
  const { elements, type } = loadChallengeScript({
    async invoke() {
      return state;
    }
  });
  await flush();

  type("the qu");

  const passage = elements.get("passage");
  assert.equal(passage.querySelector('[data-index="0"]').dataset.state, "correct");
  assert.equal(passage.querySelector('[data-index="1"]').dataset.state, "current");
});

test("finishing the third challenge shows the measured speed instead of another passage", async () => {
  const now = { value: 1_000_000 };
  const { elements, type, tick } = loadChallengeScript({
    async invoke(command) {
      if (command === "submit_typing_challenge") {
        return {
          passage: null,
          passage_index: null,
          completed: 3,
          total: 3,
          seconds: 30,
          measured_wpm: 58
        };
      }
      return { ...state, passage_index: 2, completed: 2 };
    },
    now
  });
  await flush();

  type("the quick brown fox");
  now.value += 31_000;
  tick();
  await flush();

  assert.equal(elements.get("done").hidden, false);
  assert.equal(elements.get("done-value").textContent, "58 WPM");
  assert.equal(elements.get("run").hidden, true);
  assert.equal(elements.get("typing").hidden, true);
  assert.equal(elements.get("progress").textContent, "3 of 3 challenges done");
});

test("the next challenge waits for a deliberate click rather than starting itself", async () => {
  const now = { value: 1_000_000 };
  const { elements, type, tick, click } = loadChallengeScript({
    async invoke(command) {
      if (command === "submit_typing_challenge") {
        return { ...state, passage_index: 1, completed: 1 };
      }
      return state;
    },
    now
  });
  await flush();

  type("the quick brown");
  now.value += 31_000;
  tick();
  await flush();

  // Scored, and waiting. Three runs back to back with no pause is not a
  // measurement of typing.
  assert.equal(elements.get("next").hidden, false);
  assert.equal(elements.get("typing").disabled, true);
  assert.match(elements.get("message").textContent, /1 of 3 done/);

  click("next");
  assert.equal(elements.get("typing").disabled, false);
  assert.equal(elements.get("typing").value, "");
  assert.equal(elements.get("clock").textContent, "30");
  assert.equal(elements.get("progress").textContent, "Challenge 2 of 3");
});

test("closing part-way keeps the challenges already finished", async () => {
  const commands = [];
  const { click } = loadChallengeScript({
    async invoke(command) {
      commands.push(command);
      return state;
    }
  });
  await flush();

  click("close");

  // Nothing resets or discards: the backend keeps whatever was stored, and the
  // outstanding slot is served again next time the window opens.
  assert.deepEqual(
    commands.filter((command) => command !== "get_typing_challenge"),
    ["close_typing_challenge"]
  );
});

test("a failed score leaves the run retryable instead of eating it", async () => {
  const now = { value: 1_000_000 };
  const { elements, type, tick } = loadChallengeScript({
    async invoke(command) {
      if (command === "submit_typing_challenge") throw new Error("could not write settings");
      return state;
    },
    now
  });
  await flush();

  type("the quick brown");
  now.value += 31_000;
  tick();
  await flush();

  assert.match(elements.get("message").textContent, /could not write settings/);
  assert.equal(elements.get("typing").disabled, false);
});

test("the window ships its passages rather than fetching them", () => {
  // Local-Only Processing (CONTEXT.md): nothing here reaches the network.
  assert.doesNotMatch(challengeHtml, /\bfetch\s*\(/);
  assert.doesNotMatch(challengeHtml, /XMLHttpRequest/);
  assert.doesNotMatch(challengeHtml, /https?:\/\//);
});
