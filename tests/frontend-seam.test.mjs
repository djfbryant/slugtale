import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";


const rustDir = new URL("../src-tauri/src/", import.meta.url);
const rustSources = readdirSync(rustDir, { recursive: true })
  .filter((name) => String(name).endsWith(".rs"))
  .map((name) => readFileSync(new URL(String(name), rustDir), "utf8"))
  .join("\n");

const srcDir = new URL("../src/", import.meta.url);
const frontendSources = readdirSync(srcDir, { recursive: true })
  .filter((name) => /\.(html|css|js)$/.test(String(name)))
  .map((name) => readFileSync(new URL(String(name), srcDir), "utf8"))
  .join("\n");

function stylesheetFor(page) {
  const html = readFileSync(new URL(page, srcDir), "utf8");
  const linked = [...html.matchAll(/<link[^>]+rel="stylesheet"[^>]+href="([^"]+)"/g)]
    .map(([, href]) => href)
    .filter((href) => !/^(https?:)?\/\//.test(href));
  return linked
    .map((href) => readFileSync(new URL(href, srcDir), "utf8"))
    .join("\n");
}

function emittedEventNames(source) {
  const names = new Set();
  for (const match of source.matchAll(/\.emit\(\s*"([^"]+)"/g)) {
    names.add(match[1]);
  }
  return names;
}

function listenedEventNames(source) {
  const names = new Set();
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
  for (const match of source.matchAll(
    /#\[tauri::command\]\s*(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+([a-z_]+)/g,
  )) {
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

test("the voice level threshold is one number in Rust and in the bar", () => {
  const rust = rustSources.match(
    /pub const VOICE_LEVEL: f32 = ([0-9.]+);/,
  );
  assert.ok(rust, "expected the Rust VOICE_LEVEL constant in audio_capture.rs");

  const bar = frontendSources.match(/const VOICE_LEVEL = ([0-9.]+);/);
  assert.ok(bar, "expected the bar's own VOICE_LEVEL constant");

  assert.equal(
    Number(bar[1]),
    Number(rust[1]),
    "the bar and the Segment Pause threshold must be the same level",
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

  const nowReachable = [...notYetReachable].filter((command) => invoked.has(command));
  assert.deepEqual(
    nowReachable,
    [],
    "these are reachable now, so the not-yet-reachable list is stale",
  );
});

test("a command name a frontend sends as data is still a real command", () => {
  const asData = new Set([
    ...frontendSources.matchAll(/\bcommand:\s*"([a-z_]+)"/g),
    ...frontendSources.matchAll(/\bsaveUsage\(\s*"([a-z_]+)"/g),
  ].map((match) => match[1]));
  assert.ok(asData.size >= 4, `expected the data-carrying command sites, found ${asData.size}`);

  const declared = declaredCommandNames(rustSources);
  const missing = [...asData].filter((command) => !declared.has(command));
  assert.deepEqual(missing, [], "a command sent as data has no matching backend command");
});

test("the settings window knows every readiness item the backend can report", () => {
  const source = readFileSync(new URL("../src-tauri/src/readiness.rs", import.meta.url), "utf8");
  const enumBlock = source.match(/pub enum ReadinessItemId \{([\s\S]*?)\n\}/);
  assert.ok(enumBlock, "expected a ReadinessItemId enum in readiness.rs");
  const backend = new Set(
    [...enumBlock[1].matchAll(/^\s{4}(\w+),$/gm)].map((match) => match[1]),
  );
  assert.ok(backend.size >= 6, `expected the readiness ids, found ${backend.size}`);

  // The backend serialises these names with `rename_all = "snake_case"`, so the
  // wire form of each variant is its name lowercased with underscores.
  const wireNames = new Set([...backend].map((name) => snakeCase(name)));
  assert.deepEqual(
    [...backend].map(snakeCase).sort(),
    [...wireNames].sort(),
    "two variants collapse onto one wire name",
  );

  const copyBlock = frontendSources.match(/const READINESS_COPY = \{([\s\S]*?)\n    \};/);
  assert.ok(copyBlock, "expected one READINESS_COPY record per readiness item");
  const inCopy = new Set(
    [...copyBlock[1].matchAll(/^ {6}(\w+): \{$/gm)].map((match) => match[1]),
  );

  const fallbackBlock = frontendSources.match(/const fallbackReport = \{([\s\S]*?)\n    \};/);
  assert.ok(fallbackBlock, "expected a fallbackReport for the browser");
  const inFallback = new Set(
    [...fallbackBlock[1].matchAll(/\{ id: "([a-z_]+)"/g)].map((match) => match[1]),
  );

  for (const [name, found] of [
    ["READINESS_COPY", inCopy],
    ["fallbackReport", inFallback],
  ]) {
    assert.deepEqual(
      [...wireNames].filter((id) => !found.has(id)),
      [],
      `${name} is missing a readiness item the backend can report`,
    );
    assert.deepEqual(
      [...found].filter((id) => !wireNames.has(id)),
      [],
      `${name} describes a readiness item the backend never reports`,
    );
  }
});

test("the settings window calls every readiness item the same name the backend does", () => {
  const source = readFileSync(new URL("../src-tauri/src/readiness.rs", import.meta.url), "utf8");
  const labelBlock = source.match(/pub fn label\(self\) -> &'static str \{([\s\S]*?)\n    \}/);
  assert.ok(labelBlock, "expected ReadinessItemId::label to state every name");
  const backend = new Map(
    [...labelBlock[1].matchAll(/ReadinessItemId::(\w+) => "([^"]+)"/g)].map((match) => [
      snakeCase(match[1]),
      match[2],
    ]),
  );
  assert.ok(backend.size >= 6, `expected the readiness labels, found ${backend.size}`);

  const fallbackBlock = frontendSources.match(/const fallbackReport = \{([\s\S]*?)\n    \};/);
  assert.ok(fallbackBlock, "expected a fallbackReport for the browser");
  const inFallback = new Map(
    [...fallbackBlock[1].matchAll(/\{ id: "([a-z_]+)", label: "([^"]+)"/g)].map((match) => [
      match[1],
      match[2],
    ]),
  );

  for (const [id, label] of backend) {
    assert.equal(inFallback.get(id), label, `the fallback report renames ${id}`);
  }
});

function snakeCase(name) {
  return name.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}

test("every configured window label is a label the WindowLabel type names", () => {
  const source = readFileSync(
    new URL("../src-tauri/src/window_label.rs", import.meta.url),
    "utf8",
  );
  const constants = new Map(
    [...source.matchAll(/pub const (\w+): &'static str = "([^"]+)";/g)].map((match) => [
      match[1],
      match[2],
    ]),
  );
  assert.ok(constants.size > 0, "expected WindowLabel string constants");

  const asStr = source.match(/pub fn as_str\([^)]*\) -> &'static str \{([\s\S]*?)\n    \}/);
  assert.ok(asStr, "expected a WindowLabel::as_str match over every variant");
  const named = [...asStr[1].matchAll(/(?:Self|WindowLabel)::(\w+) => (?:Self|WindowLabel)::(\w+),/g)].map(
    (match) => [match[1], constants.get(match[2])],
  );
  assert.ok(
    named.length === constants.size,
    `as_str names ${named.length} of the ${constants.size} constants`,
  );

  const fromLabel = source.match(/pub fn from_label\([^)]*\) -> Option<WindowLabel> \{([\s\S]*?)\n    \}/);
  assert.ok(fromLabel, "expected a WindowLabel::from_label match over every label");
  const parsed = new Set(
    [
      ...fromLabel[1].matchAll(
        /(?:Self|WindowLabel)::(\w+) => Some\((?:Self|WindowLabel)::(\w+)\)/g,
      ),
    ].map((match) => match[2]),
  );
  for (const [variant] of named) {
    assert.ok(
      parsed.has(variant),
      `${variant} is named by as_str but not parsed by from_label`,
    );
  }

  const labels = named.map(([, label]) => label);
  assert.equal(new Set(labels).size, labels.length, `two windows share a label: ${labels}`);

  const config = JSON.parse(
    readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );
  const configured = config.app.windows.map((window) => window.label);
  assert.ok(configured.length > 0, "expected configured windows in tauri.conf.json");
  for (const label of configured) {
    assert.ok(
      labels.includes(label),
      `tauri.conf.json configures "${label}" and no WindowLabel names it`,
    );
  }
});
