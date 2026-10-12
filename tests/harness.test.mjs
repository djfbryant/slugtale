import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

const FIXTURES = new URL("./fixtures/", import.meta.url);


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

test("a local script by src is followed, so a page's own file is what runs", () => {
  const { api } = runPage("external.html", {
    baseDir: FIXTURES,
    exports: ["fromExternalFile"]
  });

  assert.equal(api.fromExternalFile(), "loaded from its own file");
});

test("a script src the harness cannot resolve fails loudly instead of half-testing", () => {
  assert.throws(
    () =>
      runPage("remote.html", {
        markup: '<script src="https://cdn.example/tauri.js"></script>',
      }),
    /outside src/
  );
  assert.throws(
    () =>
      runPage("missing.html", {
        baseDir: FIXTURES,
        markup: '<script src="no-such-file.js"></script>',
      }),
    /ENOENT|no-such-file/
  );
});

test("a bootstrap call is stripped wherever it sits, not only at the tail", () => {
  const { api, elements } = runPage("early-bootstrap.html", {
    markup: `${shell()}${page(`
      function started_() { document.getElementById("passage").textContent = "started"; return 1; }

      started_();
    `)}`,
    bootstrap: ["started_"],
    exports: ["started_"]
  });

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
  const { document } = runPage("no-such-id.html", {
    markup: `${shell()}${page("function reachable() { return 1; }")}`,
    exports: ["reachable"]
  });

  assert.equal(document.getElementById("passage").tagName, "DIV");
  assert.equal(document.getElementById("not-in-the-page"), null);
});

test("markup assigned to innerHTML becomes queryable children", () => {
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
  return flushNextTimeout().then(() => {
    assert.equal(elements.get("passage").textContent, "1030000");
  });
});

test("a page with a fixed clock can still build a Date", () => {
  const { api } = runPage("date-constructor.html", {
    markup: `${shell()}${page("function fromArgument() { return new Date(0).getUTCFullYear(); }\nfunction fromNoArgument() { return new Date().getTime(); }")}`,
    exports: ["fromArgument", "fromNoArgument"],
    now: () => 1_000_000
  });

  assert.equal(api.fromArgument(), 1970);
  assert.equal(api.fromNoArgument(), 1_000_000);
});

test("hidden, disabled and checked in the page's own markup reach the property", () => {
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

  tickIntervals();
  assert.equal(elements.get("passage").textContent, "a");
});

test("the page's own html and body are the document's", () => {
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
    markup: `${shell()}${page("function isOwn() { return new Date(0) instanceof Date; }\nfunction sharesPrototype() { return Object.getPrototypeOf(new Date(0)) === Date.prototype; }\nfunction isPinned() { return new Date().getTime(); }")}`,
    exports: ["isOwn", "sharesPrototype", "isPinned"],
    now: () => 1_000_000
  });

  assert.equal(api.isOwn(), true);
  assert.equal(api.sharesPrototype(), true);
  assert.equal(api.isPinned(), 1_000_000);
});

test("a page's window-level listener can be reached, as a focus event is", () => {
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
