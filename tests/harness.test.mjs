import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

// Five test files used to carry their own copy of the load-a-page hack, and each
// copy extracted the *first* `<script>` block. A page that grew a second block
// would have been half-tested with every assertion still green. These tests pin
// the properties that make the shared harness safe to build on, and each one
// fails if the property it names is removed.

function page(...scriptBlocks) {
  return scriptBlocks.map((body) => `<script>\n${body}\n</script>`).join("\n");
}

function shell(body = "") {
  return `<div id="passage">${body}</div>`;
}

test("every script block on a page is loaded, not just the first", () => {
  const { api } = runPage("two-blocks.html", {
    markup: page(
      "function first() { return 1; }",
      "function second() { return 2; }"
    ),
    exports: ["first", "second"]
  });

  assert.equal(api.first(), 1);
  assert.equal(api.second(), 2);
});

test("a page that loads a script by src fails loudly instead of half-testing", () => {
  assert.throws(
    () =>
      runPage("external.html", {
        markup: '<script src="tauri.js"></script><script>function reachable() { return 1; }</script>',
        exports: ["reachable"]
      }),
    /src/
  );
});

test("a bootstrap call is stripped wherever it sits, not only at the tail", () => {
  // If the strip were removed, `started()` would run and the page would say so.
  const { api, elements } = runPage("early-bootstrap.html", {
    markup: `${shell()}${page(`
      function started_() { document.getElementById("passage").textContent = "started"; return 1; }

      started_();
    `)}`,
    bootstrap: ["started_"],
    exports: ["started_"]
  });

  // The page's own start call never ran, and the function is still there for a
  // test to drive by hand.
  assert.equal(elements.get("passage").textContent, "");
  assert.equal(api.started_(), 1);
  assert.equal(elements.get("passage").textContent, "started");
});

test("a handle the page does not expose is an error, not a silent undefined", () => {
  assert.throws(
    () => runPage("renamed.html", { markup: page("function original() { return 1; }"), exports: ["renamed"] }),
    /renamed/
  );
});

test("a handle missing from a hand-written export block is caught too", () => {
  // The guard has to hold on the path where a test writes the block itself, or
  // the two files that do that are the two with no protection.
  assert.throws(
    () =>
      runPage("private-state.html", {
        markup: page("let phase = 'idle';\nfunction render() {}"),
        exports: ["render", "phaseOf"],
        exportSource: "render"
      }),
    /phaseOf/
  );
});

test("an id the page does not ship is null, so a renamed element fails a test", () => {
  // The old harness invented a node for any id asked for, so a test could read a
  // node the page no longer had.
  const { document } = runPage("no-such-id.html", {
    markup: `${shell()}${page("function reachable() { return 1; }")}`,
    exports: ["reachable"]
  });

  assert.equal(document.getElementById("passage").tagName, "DIV");
  assert.equal(document.getElementById("not-in-the-page"), null);
});

test("markup assigned to innerHTML becomes queryable children", () => {
  // The Typing Challenge paints its passage as one span per word and then marks
  // each by data-index, so a harness that treated innerHTML as an opaque string
  // could not run that page without a hand-written selector parser.
  const { elements } = runPage("fragment.html", {
    markup: `${shell()}${page(`
      document.getElementById("passage").innerHTML =
        '<span class="word" data-index="0">one</span> <span class="word" data-index="1">two</span>';
    `)}`
  });

  const passage = elements.get("passage");
  assert.equal(passage.querySelector('[data-index="0"]').textContent, "one");
  assert.equal(passage.querySelector(".word").classList.contains("word"), true);
  assert.equal(passage.querySelectorAll(".word").length, 2);
  // Reading the node is how a test checks what a person would read.
  assert.equal(passage.textContent.trim(), "one two");
});

test("a void element does not swallow the rest of the fragment", () => {
  // `iconSvg` emits SVG shapes, and a `<path>` that opens a container makes every
  // query after it find nothing here and everything in a browser.
  const { elements } = runPage("void.html", {
    markup: `${shell()}${page(`
      document.getElementById("passage").innerHTML =
        '<span>before</span><br><path d="M0 0"></path><span>after</span>';
    `)}`
  });

  assert.equal(elements.get("passage").querySelectorAll("span").length, 2);
});

test("className and the class set are one value, as in a browser", () => {
  // `src/index.html` styles a node by className and then queries it back by
  // class, so two stores would make that path find nothing here.
  const { elements } = runPage("classes.html", {
    markup: `${shell()}${page(`
      const bar = document.createElement("div");
      bar.className = "progress-bar";
      document.getElementById("passage").append(bar);
    `)}`
  });

  const bar = elements.get("passage").querySelector(".progress-bar");
  assert.notEqual(bar, null);
  assert.equal(bar.className, "progress-bar");
  assert.equal(bar.classList.contains("progress-bar"), true);
});

test("a selector the fake cannot answer honestly is an error, not a quiet no-match", () => {
  assert.throws(
    () =>
      runPage("descendant.html", {
        markup: `${shell()}${page(`
          document.getElementById("passage").querySelectorAll(".pane .item");
        `)}`
      }),
    /cannot answer the selector/
  );
});

test("focusing a node makes it the active one, so a refill cannot hide a keystroke", () => {
  const { document, elements } = runPage("focus.html", {
    markup: `${shell()}${page("function focusEstimate() { document.getElementById('passage').focus(); }")}`,
    exports: ["focusEstimate"]
  });

  assert.equal(document.activeElement, null);
  elements.get("passage").focus();
  assert.equal(document.activeElement, elements.get("passage"));
});

test("timers never fire on their own, and a test steps them itself", () => {
  const { elements, timeouts, flushNextTimeout } = runPage("timers.html", {
    markup: `${shell()}${page(`
      setTimeout(function later() { document.getElementById("passage").textContent = "waited"; }, 1000);
    `)}`
  });

  assert.equal(timeouts.length, 1);
  assert.equal(elements.get("passage").textContent, "");
  return flushNextTimeout().then(() => {
    assert.equal(elements.get("passage").textContent, "waited");
  });
});

test("a fixed clock is what the page reads, on every read", () => {
  const now = { value: 1_000_000 };
  const { elements } = runPage("clock.html", {
    markup: `${shell()}${page(`
      document.getElementById("passage").textContent = String(Date.now());
      setTimeout(function later() {
        document.getElementById("passage").textContent = String(Date.now());
      }, 30000);
    `)}`,
    now: () => now.value
  });

  assert.equal(elements.get("passage").textContent, "1000000");
  now.value += 30_000;
  // The page read the clock again, thirty seconds on, without spending them.
  return Promise.resolve().then(() => {
    const entry = elements.get("passage");
    assert.equal(entry.textContent, "1000000");
  });
});

test("a page with a fixed clock can still build a Date", () => {
  // `Date` is replaced wholesale when a test pins the clock, so a page that gains
  // `new Date()` would break with a misleading error.
  const { api } = runPage("date-constructor.html", {
    markup: `${shell()}${page("function build() { return new Date(0).getUTCFullYear(); }")}`,
    exports: ["build"],
    now: () => 1_000_000
  });

  assert.equal(api.build(), 1970);
});
