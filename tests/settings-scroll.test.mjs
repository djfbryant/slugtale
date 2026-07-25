import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");

function cssRule(selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = settingsHtml.match(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`));
  assert.ok(match, `Missing CSS rule for ${selector}`);
  return match[1];
}

test("the settings content column gives its inner scroller a bounded height", () => {
  assert.match(cssRule(".content"), /min-height:\s*0\s*;/);
  assert.match(cssRule(".content-scroll"), /min-height:\s*0\s*;/);
  assert.match(cssRule(".content-scroll"), /overflow(?:-y)?:\s*auto\s*;/);
});

test("overflowing settings expose a visible scrollbar", () => {
  assert.match(cssRule(".content-scroll"), /scrollbar-gutter:\s*stable\s*;/);
  assert.match(cssRule(".content-scroll"), /scrollbar-color:\s*var\(--border-strong\)\s+transparent\s*;/);
  assert.match(cssRule(".content-scroll::-webkit-scrollbar"), /width:\s*10px\s*;/);
  assert.match(cssRule(".content-scroll::-webkit-scrollbar-thumb"), /background:\s*var\(--border-strong\)\s*;/);
});
