import assert from "node:assert/strict";
import test from "node:test";

import { runPage, shippedSource } from "./harness.mjs";

const challenge = shippedSource("typing-challenge.html");

// Let every pending promise settle. The window loads its state asynchronously on
// open and again after each score, and counting microtasks by hand is brittle.
function flush() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// The whole point of the challenge is that thirty seconds elapse, and no test
// should actually wait for them, so the clock and the tick are the test's.
function loadChallengeScript({ invoke, now = { value: 1_000_000 } }) {
  const { api, document, elements, tickIntervals } = runPage("typing-challenge.html", {
    exports: [],
    invoke,
    now: () => now.value,
    // The Typing Challenge starts itself on open, so its tests drive the page
    // through the listeners it registered rather than around them.
    runBootstrap: true
  });

  return {
    api,
    document,
    elements,
    // Type into the box the way a person would: set the value, then fire input.
    type(text) {
      const box = elements.get("typing");
      box.value = text;
      box.dispatch("input", { target: box });
    },
    tick: tickIntervals,
    click(id) {
      elements.get(id).dispatch("click");
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
  // Local-Only Processing (CONTEXT.md): nothing here reaches the network. Read
  // every file the page ships — markup, stylesheet and script — because the
  // fetch would live in the script, not the markup.
  assert.doesNotMatch(challenge.all, /\bfetch\s*\(/);
  assert.doesNotMatch(challenge.all, /XMLHttpRequest/);
  assert.doesNotMatch(challenge.all, /https?:\/\//);
});
