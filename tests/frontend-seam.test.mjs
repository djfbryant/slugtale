import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

// The Rust↔JS seam is stringly typed by necessity: Tauri events and command
// arguments cross it as bare names. A typo on either side compiles fine and
// silently does nothing. These tests pin both sides of the seam to the same
// vocabulary so drift fails here instead of at a user's desk.

const rustDir = new URL("../src-tauri/src/", import.meta.url);
const rustSources = readdirSync(rustDir, { recursive: true })
  .filter((name) => String(name).endsWith(".rs"))
  .map((name) => readFileSync(new URL(String(name), rustDir), "utf8"))
  .join("\n");

const frontendSources = readdirSync(new URL("../src/", import.meta.url))
  .filter((name) => name.endsWith(".html"))
  .map((name) => readFileSync(new URL(`../src/${name}`, import.meta.url), "utf8"))
  .join("\n");

function emittedEventNames(source) {
  const names = new Set();
  for (const match of source.matchAll(/\.emit\(\s*"([^"]+)"/g)) {
    names.add(match[1]);
  }
  return names;
}

function listenedEventNames(source) {
  const names = new Set();
  // Matches direct calls and the frontends' `events.listen("name", ...)` wrapper.
  // Word-boundary on the left so `unlisten("name")` teardown calls never count
  // as a listener.
  for (const match of source.matchAll(/\blisten\(\s*"([^"]+)"/g)) {
    names.add(match[1]);
  }
  return names;
}

function invokedCommandNames(source) {
  const names = new Set();
  for (const match of source.matchAll(/invoke\(\s*"([a-z_]+)"/g)) {
    names.add(match[1]);
  }
  return names;
}

function declaredCommandNames(source) {
  const names = new Set();
  for (const match of source.matchAll(/#\[tauri::command\]\s*(?:async\s+)?fn\s+([a-z_]+)/g)) {
    names.add(match[1]);
  }
  return names;
}

test("every event the backend emits is an event some frontend listens for", () => {
  const emitted = emittedEventNames(rustSources);
  assert.ok(emitted.size > 0, "expected to find emitted events in the Rust sources");

  const listened = listenedEventNames(frontendSources);
  const unheard = [...emitted].filter((name) => !listened.has(name));
  assert.deepEqual(
    unheard,
    [],
    "backend emits events no frontend listens for — dead event or missing listener",
  );
});

test("every event a frontend listens for is an event the backend emits", () => {
  const listened = listenedEventNames(frontendSources);
  assert.ok(listened.size > 0, "expected to find listened events in the HTML sources");

  const emitted = emittedEventNames(rustSources);
  const neverSent = [...listened].filter((name) => !emitted.has(name));
  assert.deepEqual(
    neverSent,
    [],
    "frontend listens for events the backend never emits — typo or removed emit",
  );
});

test("the dictation bar only asks dictation_event for known events", () => {
  const known = new Set(["start", "stop", "cancel"]);
  const requested = new Set(
    [...frontendSources.matchAll(/dictation_event",\s*\{\s*event:\s*"([^"]+)"/g)].map(
      (match) => match[1],
    ),
  );
  assert.ok(requested.size > 0, "expected the bar to send dictation_event calls");

  const unknown = [...requested].filter((event) => !known.has(event));
  assert.deepEqual(unknown, [], "frontend sends a dictation event the backend rejects");
});

test("the backend accepts exactly the dictation events the bar can send", () => {
  const command = rustSources.match(
    /fn dictation_event\([\s\S]*?match event\.as_str\(\)\s*\{([\s\S]*?)\n    \}/,
  );
  assert.ok(command, "expected to find the dictation_event match in main.rs");

  const accepted = [...command[1].matchAll(/"([a-z]+)" =>/g)].map((match) => match[1]);
  assert.ok(accepted.includes("start"), `backend accepts: ${accepted.join(", ")}`);
  for (const required of ["stop", "cancel"]) {
    assert.ok(
      accepted.includes(required),
      `backend must accept ${required}; accepts: ${accepted.join(", ")}`,
    );
  }
});

test("every command the settings window invokes exists as a Tauri command", () => {
  const declared = declaredCommandNames(rustSources);
  assert.ok(declared.size > 0, "expected #[tauri::command] fns in main.rs");

  const invoked = invokedCommandNames(frontendSources);
  const missing = [...invoked].filter((command) => !declared.has(command));
  assert.deepEqual(missing, [], "frontend invokes commands that do not exist");
});
