import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

// Tauri embeds everything under src/ into the shipped app (frontendDist), so
// that directory must hold only runtime pages. Design artifacts live in
// prototypes/ instead (slugtale-g1o.9).

const RUNTIME_ENTRY_PAGES = [
  "dictation-bar.html",
  "index.html",
  "typing-challenge.html",
];

function srcEntries() {
  return readdirSync(new URL("../src", import.meta.url));
}

test("the runtime frontend keeps its three HTML entry pages", () => {
  const entries = srcEntries();
  for (const page of RUNTIME_ENTRY_PAGES) {
    assert(entries.includes(page), `runtime page missing from src/: ${page}`);
  }
});

test("no design prototypes or notes remain in the runtime frontend", () => {
  const entries = srcEntries().join("\n");
  assert(
    !/prototype/i.test(entries),
    "prototype artifacts belong in prototypes/, not src/",
  );
});
