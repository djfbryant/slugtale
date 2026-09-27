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
  // If the strip were removed, `started_()` would run and the page would say so.
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
  // A void element that opens a container nests everything after it inside
  // itself. A descendant query still finds those nodes, so the assertion has to
  // be on the host's own children, which is what mis-nesting changes.
  const { elements } = runPage("void.html", {
    markup: `${shell()}${page(`
      document.getElementById("passage").innerHTML =
        '<span>before</span><input><span>after</span>';
    `)}`
  });

  assert.deepEqual(
    elements.get("passage").children.map((child) => child.tagName || "#text"),
    ["SPAN", "INPUT", "SPAN"]
  );
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
  assert.equal(bar.getAttribute("class"), "progress-bar");

  // And classList writes the same value className and a query read.
  bar.classList.add("done");
  assert.equal(bar.className, "progress-bar done");
  assert.equal(elements.get("passage").querySelector(".done"), bar);
  bar.classList.remove("progress-bar");
  assert.equal(elements.get("passage").querySelector(".progress-bar"), null);
  assert.equal(bar.classList.toggle("done"), false);
  assert.equal(elements.get("passage").querySelector(".done"), null);
  // And a one-argument toggle flips, as a browser's does.
  assert.equal(bar.classList.toggle("fresh"), true);
  assert.equal(bar.classList.contains("fresh"), true);
  assert.equal(bar.classList.toggle("fresh"), false);
  assert.equal(bar.classList.contains("fresh"), false);
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
  const { elements, flushNextTimeout } = runPage("clock.html", {
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
  return flushNextTimeout().then(() => {
    assert.equal(elements.get("passage").textContent, "1030000");
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

test("hidden, disabled and checked in the page's own markup reach the property", () => {
  // `src/index.html` ships `<button hidden>` and `<input disabled>`, and a pane
  // reads `pane.hidden` rather than the attribute.
  const { elements } = runPage("booleans.html", {
    markup: `${shell()}<input id="toggle" checked /><button id="reveal" hidden></button>${page("function reachable() { return 1; }")}`,
    exports: ["reachable"]
  });

  assert.equal(elements.get("toggle").checked, true);
  assert.equal(elements.get("reveal").hidden, true);
  elements.get("reveal").hidden = false;
  assert.equal(elements.get("reveal").hasAttribute("hidden"), false);
  assert.equal(elements.get("reveal").getAttribute("hidden"), null);
});

test("getAttribute answers for class, id and data, as a browser does", () => {
  const { elements } = runPage("attributes.html", {
    markup: `${shell()}<span id="word" class="word" data-index="3"></span>${page("function reachable() { return 1; }")}`,
    exports: ["reachable"]
  });

  const word = elements.get("word");
  assert.equal(word.getAttribute("class"), "word");
  assert.equal(word.getAttribute("id"), "word");
  assert.equal(word.getAttribute("data-index"), "3");
  assert.equal(word.hasAttribute("data-index"), true);
});

test("an inline style set by property is the one removeProperty clears", () => {
  // `src/index.html` sets `bar.style.width` and then removes it on the same node.
  const { elements } = runPage("styles.html", {
    markup: `${shell()}<div id="bar" class="progress-bar"></div>${page(`
      const bar = document.getElementById("bar");
      bar.style.width = "42%";
    `)}`
  });


  const bar = elements.get("bar");
  assert.equal(bar.style.width, "42%");
  assert.equal(bar.style.getPropertyValue("width"), "42%");
  bar.style.removeProperty("width");
  assert.equal(bar.style.width, "");
});

test("a cleared interval stops running, and the survivors keep their handles", () => {
  const { api, elements, intervals, tickIntervals } = runPage("intervals.html", {
    markup: `${shell()}${page(`
      function count(id) { document.getElementById("passage").textContent += id; }
      function start() {
        globalThis.first = setInterval(function tickOne() { count("a"); }, 10);
        globalThis.second = setInterval(function tickTwo() { count("b"); }, 10);
      }
      function stopSecond() { clearInterval(second); }
    `)}`,
    exports: ["start", "stopSecond"]
  });

  api.start();
  assert.equal(intervals.length, 2);
  api.stopSecond();
  assert.equal(intervals.length, 1);

  // The first interval is the one that survives, and the second must not run.
  tickIntervals();
  assert.equal(elements.get("passage").textContent, "a");
});

test("the page's own html and body are the document's", () => {
  // `dictation-bar.html` puts its bar position on its own body, and 18
  // assertions read it from there.
  const { document } = runPage("shell.html", {
    markup: `<!doctype html><html lang="en"><head><title>bar</title></head><body data-position="bottom-center"></body></html>${page("function reachable() { return 1; }")}`,
    exports: ["reachable"]
  });

  assert.equal(document.documentElement.getAttribute("lang"), "en");
  assert.equal(document.querySelector("title").textContent, "bar");
  assert.equal(document.body.dataset.position, "bottom-center");
});

test("a replaced element's own text is gone, as in a browser", () => {
  const { elements } = runPage("replaced.html", {
    markup: `${shell()}${page(`
      const host = document.getElementById("passage");
      host.textContent = "stale";
      const span = document.createElement("span");
      span.textContent = "fresh";
      host.replaceChildren(span);
    `)}`
  });

  assert.equal(elements.get("passage").textContent, "fresh");
});

test("a pinned Date is still the realm's own Date", () => {
  const { api } = runPage("date-identity.html", {
    markup: `${shell()}${page("function isOwn() { return new Date(0) instanceof Date; }\nfunction sharesPrototype() { return Object.getPrototypeOf(new Date(0)) === Date.prototype; }")}`,
    exports: ["isOwn", "sharesPrototype"],
    now: () => 1_000_000
  });

  assert.equal(api.isOwn(), true);
  assert.equal(api.sharesPrototype(), true);
});

test("a page's window-level listener can be reached, as a focus event is", () => {
  // `src/index.html` refreshes readiness and usage when the window regains focus,
  // and a harness that dropped the listener could not test that path at all.
  const { elements, windowEvent } = runPage("window-focus.html", {
    markup: `${shell()}${page(`
      window.addEventListener("focus", function onFocus() {
        document.getElementById("passage").textContent = "refreshed";
      });
    `)}`
  });

  assert.equal(elements.get("passage").textContent, "");
  windowEvent("focus");
  assert.equal(elements.get("passage").textContent, "refreshed");
});
