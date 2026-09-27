import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

// The Rust↔JS seam is stringly typed by necessity: Tauri events and command
// arguments cross it as bare names. A typo on either side compiles fine and
// silently does nothing. These tests pin both sides of the seam to the same
// vocabulary so drift fails here instead of at a user's desk.
//
// Both directions matter. A frontend name the backend does not have fails at the
// user's desk; a backend name no frontend reaches is a command nothing can call,
// which reads in the source as a feature and behaves as none.

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
  // A literal argument is the obvious case. A command name that travels as data —
  // through a readiness action table, or as a parameter — is just as much a
  // crossing, so those sites are read too.
  for (const match of source.matchAll(/invoke\(\s*"([a-z_]+)"/g)) {
    names.add(match[1]);
  }
  for (const match of source.matchAll(/\bcommand:\s*"([a-z_]+)"/g)) {
    names.add(match[1]);
  }
  for (const match of source.matchAll(/\bsaveUsage\(\s*"([a-z_]+)"/g)) {
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

function registeredCommandNames(source) {
  const block = source.match(/generate_handler!\[([\s\S]*?)\]\)/);
  assert.ok(block, "expected a generate_handler! list in main.rs");
  return new Set(
    block[1]
      .split(",")
      .map((name) => name.trim())
      .filter(Boolean),
  );
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

test("every command a frontend reaches exists as a Tauri command", () => {
  const declared = declaredCommandNames(rustSources);
  assert.ok(declared.size > 0, "expected #[tauri::command] fns in main.rs");

  const invoked = invokedCommandNames(frontendSources);
  assert.ok(invoked.size > 0, "expected the frontends to reach at least one command");
  const missing = [...invoked].filter((command) => !declared.has(command));
  assert.deepEqual(missing, [], "frontend reaches commands that do not exist");
});

test("every Tauri command is registered, or the app cannot call it", () => {
  const declared = declaredCommandNames(rustSources);
  const registered = registeredCommandNames(rustSources);
  const missing = [...declared].filter((command) => !registered.has(command));
  assert.deepEqual(
    missing,
    [],
    "commands declared but absent from generate_handler! — Tauri cannot route them",
  );
});

test("a command no frontend can reach is either gone or listed as not yet wired", () => {
  // `voice_activation_supported` backs a toggle the settings window marks Coming
  // Soon, so nothing can call it yet. Listing it keeps the check honest instead of
  // switched off, and the allowlist empties as the feature lands.
  const notYetReachable = new Set(["voice_activation_supported"]);

  const declared = declaredCommandNames(rustSources);
  const invoked = invokedCommandNames(frontendSources);
  const unreachable = [...declared].filter(
    (command) => !invoked.has(command) && !notYetReachable.has(command),
  );
  assert.deepEqual(
    unreachable,
    [],
    "commands no frontend reaches and no allowlist entry explains — dead surface",
  );

  // A command that has become reachable must leave the list, so the exception
  // cannot become a place to hide a live command.
  const nowReachable = [...notYetReachable].filter((command) => invoked.has(command));
  assert.deepEqual(
    nowReachable,
    [],
    "these are reachable now, so the not-yet-reachable list is stale",
  );
});

test("a command name a frontend sends as data is still a real command", () => {
  // `open_microphone_settings`, `open_text_insertion_settings`, `set_usage_storing`
  // and `set_typing_estimate` cross the seam as a value rather than a literal,
  // so a typo in one of those sites is a command that silently does nothing.
  const asData = new Set([
    ...frontendSources.matchAll(/\bcommand:\s*"([a-z_]+)"/g),
    ...frontendSources.matchAll(/\bsaveUsage\(\s*"([a-z_]+)"/g),
  ].map((match) => match[1]));
  assert.ok(asData.size >= 4, `expected the data-carrying command sites, found ${asData.size}`);

  const declared = declaredCommandNames(rustSources);
  const missing = [...asData].filter((command) => !declared.has(command));
  assert.deepEqual(missing, [], "a command sent as data has no matching backend command");
});
