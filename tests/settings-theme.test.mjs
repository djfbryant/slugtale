import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { stylesheetOf } from "./harness.mjs";

const settingsHtml = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
const settingsCss = stylesheetOf("index.html");

test("no raw hex colours outside the style token block", () => {
  const tokenBlocks = [...settingsCss.matchAll(/:root\s*\{[\s\S]*?\n\}/g)];
  assert.ok(tokenBlocks.length > 0, ":root token block missing");

  const outsideTokens = settingsCss.replace(/:root\s*\{[\s\S]*?\n\}/g, "");
  const hexOutside = [
    ...`${outsideTokens}\n${settingsHtml}`.matchAll(/#[0-9a-fA-F]{3,8}\b/g),
  ];

  assert.deepEqual(hexOutside.map((match) => match[0]), []);
});

test("the dark scheme overrides every colour token the light theme defines", () => {
  const rootBlock = settingsCss.match(/:root\s*\{([\s\S]*?)\n\}/)?.[1];
  const darkBlock = settingsCss.match(
    /@media \(prefers-color-scheme: dark\)\s*\{\s*:root\s*\{([\s\S]*?)\n\}/
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

test("keycaps keep a real border that tracks the theme", () => {
  const kbdRule = settingsCss.match(/\.keys kbd\s*\{([\s\S]*?)\}/)?.[1];

  assert.ok(kbdRule, ".keys kbd rule missing");
  assert.match(kbdRule, /border:\s*1px solid var\(--border-strong\)/);
});
