import assert from "node:assert/strict";
import test from "node:test";

import { runPage } from "./harness.mjs";

// Five test files used to carry their own copy of the load-a-page hack, and each
// copy extracted the *first* `<script>` block. A page that grew a second block
// would have been half-tested with every assertion still green. These tests pin
// the two properties that make the shared harness safe to build on.

function page(...scriptBlocks) {
  return scriptBlocks.map((body) => `<script>\n${body}\n</script>`).join("\n");
}

test("every script block on a page is loaded, not just the first", () => {
  const { api } = runPage("two-blocks.html", {
    markup: page(
      "function first() { return 1; }\n    first();",
      "function second() { return 2; }\n    second();"
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
  const { api } = runPage("early-bootstrap.html", {
    markup: page(
      "function start() { return started; }\n\n    start();\n\n    var started = 1;",
      "function alsoStarted() { return 1; }"
    ),
    bootstrap: ["start"],
    exports: ["alsoStarted"]
  });

  assert.equal(api.alsoStarted(), 1);
});

test("a handle the page does not define is an error, not a silent undefined", () => {
  // Either the export block or the page itself names the missing handle; what
  // matters is that the test cannot go on asserting against nothing.
  assert.throws(
    () => runPage("renamed.html", { markup: page("function original() { return 1; }"), exports: ["renamed"] }),
    /renamed/
  );
});

test("markup assigned to innerHTML becomes queryable children", () => {
  // The Typing Challenge paints its passage as one span per word and then marks
  // each by data-index, so a harness that treated innerHTML as an opaque string
  // could not run that page without a hand-written selector parser.
  const { elements } = runPage("fragment.html", {
    markup: page(`
      const host = document.getElementById("passage");
      host.innerHTML = '<span class="word" data-index="0">one</span> <span class="word" data-index="1">two</span>';
    `)
  });

  const first = elements.get("passage").querySelector('[data-index="0"]');
  assert.equal(first.textContent, "one");
  assert.equal(first.classList.contains("word"), true);
  assert.equal(elements.get("passage").querySelectorAll(".word").length, 2);
});

test("a fixed clock is what the countdown pages read, so a test spends no real seconds", () => {
  const now = { value: 1_000_000 };
  const { elements } = runPage("clock.html", {
    markup: page(`document.getElementById("clock").textContent = String(Date.now());`),
    now: () => now.value
  });

  assert.equal(elements.get("clock").textContent, "1000000");
  now.value += 30_000;
  assert.equal(Date.now() > 0, true);
});
