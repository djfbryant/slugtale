import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");

// slugtale-8oz: every colour is a custom property inside the stylesheet's
// token block so the window can follow the system appearance. A hex literal
// anywhere else would silently opt one element out of dark mode.
test("no raw hex colours outside the style token block", () => {
  const styleStart = settingsHtml.indexOf("<style>");
  const styleEnd = settingsHtml.indexOf("</style>");
  assert.notEqual(styleStart, -1, "stylesheet missing");
  assert.notEqual(styleEnd, -1, "stylesheet not closed");

  const before = settingsHtml.slice(0, styleStart);
  const tokens = settingsHtml.slice(styleStart, styleEnd);
  const after = settingsHtml.slice(styleEnd);
  const hexOutside = [...`${before}${after}`.matchAll(/#[0-9a-fA-F]{3,8}\b/g)];

  assert.deepEqual(hexOutside.map((match) => match[0]), []);
});

// The dark appearance must redefine every colour token rather than inherit
// light values, or a forgotten token renders as a glare-white island in dark
// mode. Layout tokens such as shadows are exempt: they are redefined too, but
// the guarantee this test pins is about colours the user reads against.
test("the dark scheme overrides every colour token the light theme defines", () => {
  const rootBlock = settingsHtml.match(/:root\s*\{([\s\S]*?)\n    \}/)?.[1];
  const darkBlock = settingsHtml.match(
    /@media \(prefers-color-scheme: dark\)\s*\{\s*:root\s*\{([\s\S]*?)\n    \}/
  )?.[1];

  assert.ok(rootBlock, ":root token block missing");
  assert.ok(darkBlock, "dark override block missing");

  const colourTokens = [...rootBlock.matchAll(/(--[a-z-]+):/g)]
    .map((match) => match[1])
    .filter((token) => !token.startsWith("--accent-swatch"));

  const darkTokens = new Set([...darkBlock.matchAll(/(--[a-z-]+):/g)].map((match) => match[1]));
  const missing = colourTokens.filter((token) => !darkTokens.has(token));

  assert.deepEqual(missing, []);
});

// Dark keycaps were flat text until they got a real border; pin it so the
// border survives refactors of the kbd rule.
test("keycaps keep a real border that tracks the theme", () => {
  const kbdRule = settingsHtml.match(/\.keys kbd\s*\{([\s\S]*?)\}/)?.[1];

  assert.ok(kbdRule, ".keys kbd rule missing");
  assert.match(kbdRule, /border:\s*1px solid var\(--border-strong\)/);
});
