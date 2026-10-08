import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { EventEmitter } from "node:events";
import { fileURLToPath } from "node:url";
import test from "node:test";

const require = createRequire(import.meta.url);
const {
  TIMEOUT_EXIT_CODE,
  spawnInOwnGroup,
  killProcessTree,
  runChildWithTimeout,
} = require("../scripts/process-tree.js");
const { runNodeTests } = require("../scripts/run-node-tests.js");
const { runCargo } = require("../scripts/run-cargo.js");

// The runners are the only thing standing between a hung test and a machine
// that never comes back, so these tests drive the real scripts rather than
// reading them as text. Finding 08 was a runner that printed "timed out" while
// the test it claimed to stop was still running: it signalled a process group
// it had not created, so the kill never landed.

const quiet = () => {};
// Normalised so a trailing separator cannot fail an otherwise exact match.
const defaultRepoTestsDir = resolve(fileURLToPath(new URL(".", import.meta.url)));
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ---------------------------------------------------------------------------
// Unit: the child leads a group it owns
// ---------------------------------------------------------------------------

test("a POSIX child is spawned detached, so the timeout owns a process group", () => {
  const calls = [];

  spawnInOwnGroup("node", ["--test"], { cwd: "/tmp" }, {
    spawn(command, args, options) {
      calls.push({ command, args, options });
      return new EventEmitter();
    },
    platform: "linux",
  });

  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.shell, false);
});

test("a Windows child is not detached, because the tree stop is taskkill", () => {
  const calls = [];

  spawnInOwnGroup("cargo.exe", ["test"], {}, {
    spawn(command, args, options) {
      calls.push({ command, args, options });
      return new EventEmitter();
    },
    platform: "win32",
  });

  assert.equal(calls[0].options.detached, false);
});

// ---------------------------------------------------------------------------
// Unit: the tree stop is platform-correct
// ---------------------------------------------------------------------------

test("a POSIX timeout kills the whole process group, not one pid", () => {
  const signals = [];

  const delivered = killProcessTree(4242, {
    platform: "darwin",
    kill(pid, signal) {
      signals.push([pid, signal]);
    },
  });

  assert.equal(delivered, true);
  // The negative pid is the group led by the detached child, which is the only
  // way to reach the grandchildren a direct kill would orphan.
  assert.deepEqual(signals, [[-4242, "SIGKILL"]]);
});

test("a Windows timeout uses taskkill to walk the tree natively", () => {
  const calls = [];

  const delivered = killProcessTree(4242, {
    platform: "win32",
    spawnSync(command, args) {
      calls.push([command, ...args]);
      return { status: 0 };
    },
  });

  assert.equal(delivered, true);
  assert.deepEqual(calls, [["taskkill", "/pid", "4242", "/T", "/F"]]);
});

test("a timeout with no pid to kill reports that nothing was delivered", () => {
  let called = false;

  const delivered = killProcessTree(undefined, {
    platform: "darwin",
    kill() {
      called = true;
    },
  });

  assert.equal(delivered, false);
  assert.equal(called, false);
});

// ---------------------------------------------------------------------------
// Unit: exit codes
// ---------------------------------------------------------------------------

test("a timeout reports 124 even when the OS reports a plain exit code", async () => {
  // This is the Windows shape: taskkill ends the child, so it reports an exit
  // code and never a signal. Deriving the status from the signal alone is what
  // made the old runner report an ordinary failure for a timed-out suite.
  const child = new EventEmitter();
  child.pid = 99;

  const pending = runChildWithTimeout(
    { command: "cargo", args: ["test"], timeoutSeconds: 0.01, label: "Cargo" },
    {
      spawn: () => child,
      platform: "win32",
      spawnSync: () => ({ status: 1 }),
      log: quiet,
    },
  );

  // The cap has to fire first, and only then can the OS report the kill.
  await wait(60);
  child.emit("exit", 1, null);

  assert.equal(TIMEOUT_EXIT_CODE, 124);
  assert.deepEqual(await pending, { exitCode: 124, timedOut: true, signal: null });
});

test("a child killed by a signal without a timeout still reports 124", async () => {
  const child = new EventEmitter();
  child.pid = 99;

  const pending = runChildWithTimeout(
    { command: "cargo", args: ["test"], timeoutSeconds: 60, label: "Cargo" },
    { spawn: () => child, platform: "darwin", log: quiet },
  );

  child.emit("exit", null, "SIGSEGV");

  const result = await pending;
  assert.equal(result.exitCode, 124);
  assert.equal(result.signal, "SIGSEGV");
  assert.equal(result.timedOut, false);
});

test("an ordinary exit code is passed through untouched", async () => {
  for (const code of [0, 1, 101, 42]) {
    const child = new EventEmitter();
    child.pid = 99;

    const pending = runChildWithTimeout(
      { command: "cargo", args: ["test"], timeoutSeconds: 60, label: "Cargo" },
      { spawn: () => child, platform: "darwin", log: quiet },
    );

    child.emit("exit", code, null);

    assert.equal((await pending).exitCode, code, `exit code ${code} was not preserved`);
  }
});

test("a child that cannot be spawned reports a failure, not a timeout", async () => {
  const child = new EventEmitter();
  child.pid = undefined;

  const pending = runChildWithTimeout(
    { command: "cargo", args: ["test"], timeoutSeconds: 60, label: "Cargo" },
    { spawn: () => child, platform: "darwin", log: quiet },
  );

  child.emit("error", new Error("spawn cargo ENOENT"));

  assert.equal((await pending).exitCode, 1);
});

test("a normal exit releases the timeout timer", async () => {
  // A timer left armed keeps a handle open, so a finished run would still be
  // holding the process for the length of the cap.
  const realClearTimeout = globalThis.clearTimeout;
  let cleared = 0;
  globalThis.clearTimeout = (...args) => {
    cleared += 1;
    return realClearTimeout(...args);
  };

  try {
    const child = new EventEmitter();
    child.pid = 99;

    const pending = runChildWithTimeout(
      { command: "cargo", args: ["test"], timeoutSeconds: 600, label: "Cargo" },
      { spawn: () => child, platform: "darwin", log: quiet },
    );

    child.emit("exit", 0, null);
    await pending;
  } finally {
    globalThis.clearTimeout = realClearTimeout;
  }

  assert.ok(cleared >= 1, "the timeout timer was never cleared on a normal exit");
});

test("the timeout still gives up when the kill never reaches the child", async () => {
  // If the tree stop fails there is no exit event coming, and waiting for one
  // is the very hang the cap exists to prevent.
  const child = new EventEmitter();
  child.pid = 99;

  const pending = runChildWithTimeout(
    { command: "cargo", args: ["test"], timeoutSeconds: 0.01, label: "Cargo" },
    {
      spawn: () => child,
      platform: "darwin",
      kill() {
        throw new Error("ESRCH");
      },
      log: quiet,
      graceMs: 25,
    },
  );

  const result = await pending;
  assert.equal(result.exitCode, 124);
  assert.equal(result.timedOut, true);
});

// ---------------------------------------------------------------------------
// Discovery stays inside tests/
// ---------------------------------------------------------------------------

test("frontend discovery is scoped to tests/, not the whole checkout", async () => {
  // A runner that globbed the repository would pick up any stray *.test.mjs
  // anywhere in the tree, so a file left behind by another task could silently
  // join or break the frontend suite.
  const calls = [];

  await runNodeTests({
    environment: { SLUGTALE_NODE_TIMEOUT: "1" },
    spawn(command, args, options) {
      calls.push({ command, args, options });
      const child = new EventEmitter();
      child.pid = 4242;
      setImmediate(() => child.emit("exit", 0, null));
      return child;
    },
    platform: process.platform,
    log: quiet,
  });

  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args, ["--test"]);
  assert.equal(calls[0].command, process.execPath);
  assert.equal(resolve(calls[0].options.cwd), defaultRepoTestsDir);
});

// ---------------------------------------------------------------------------
// End to end: a real hang, stopped for real
// ---------------------------------------------------------------------------

// Both hangs are observed the same portable way: the hanging processes append a
// character to a marker file for as long as they are alive. `p` is the process
// the runner started, `g` is a descendant it spawned. Seeing both characters
// proves a real descendant ran, so the test would have caught a kill that
// reached only the direct child; the marker then having stopped growing proves
// both ended. No `ps`, no `tasklist`, no assumption about a process table.
function makeSandbox(prefix) {
  const root = mkdtempSync(join(tmpdir(), `slugtale-${prefix}-`));
  return {
    root,
    cleanup() {
      rmSync(root, { recursive: true, force: true });
    },
  };
}

function markerText(marker) {
  return existsSync(marker) ? readFileSync(marker, "utf8") : "";
}

async function assertTreeStopped({ marker, exitCode, elapsedMs, label }) {
  assert.equal(exitCode, TIMEOUT_EXIT_CODE, `${label} did not report a timeout`);

  const seen = markerText(marker);
  assert.ok(seen.includes("p"), `${label}: the started process never ran`);
  assert.ok(
    seen.includes("g"),
    `${label}: no descendant ran, so this could not have caught a kill that missed grandchildren`,
  );

  assert.ok(
    elapsedMs < 15_000,
    `${label}: took ${elapsedMs}ms to give up, so the cap did not stop the hang`,
  );

  const afterReturn = markerText(marker).length;
  await wait(1200);
  assert.equal(
    markerText(marker).length,
    afterReturn,
    `${label}: something was still running after the timeout reported success`,
  );
}

test("the frontend runner stops a hung test and the process it spawned", async (t) => {
  // Runs on Windows too, covering the taskkill path where there are no POSIX
  // process groups.
  const sandbox = makeSandbox("node-hang");
  t.after(sandbox.cleanup);

  const marker = join(sandbox.root, "marker");
  const descendant = join(sandbox.root, "descendant.cjs");

  writeFileSync(
    descendant,
    [
      'const { appendFileSync } = require("node:fs");',
      'setInterval(() => appendFileSync(process.env.HANG_MARKER, "g"), 50);',
      "",
    ].join("\n"),
  );

  writeFileSync(
    join(sandbox.root, "hang.test.mjs"),
    [
      'import { spawn } from "node:child_process";',
      'import { appendFileSync } from "node:fs";',
      'import test from "node:test";',
      'test("hangs, and spawns a process that hangs too", () => {',
      `  spawn(process.execPath, [${JSON.stringify(descendant)}], { stdio: "ignore" });`,
      '  setInterval(() => appendFileSync(process.env.HANG_MARKER, "p"), 50);',
      // Never resolves: the shape of a screen test waiting on something that
      // never answers.
      "  return new Promise(() => {});",
      "});",
      "",
    ].join("\n"),
  );

  const started = Date.now();
  process.env.HANG_MARKER = marker;
  // This test spawns a nested `node --test` from inside a `node --test` process.
  // Node marks a test worker with NODE_TEST_CONTEXT and a nested runner that
  // inherits it refuses to run any files ("called recursively"), so the marker
  // is cleared to reproduce a normal `npm test` invocation instead.
  const inheritedTestContext = process.env.NODE_TEST_CONTEXT;
  delete process.env.NODE_TEST_CONTEXT;
  try {
    const exitCode = await runNodeTests({
      environment: { SLUGTALE_NODE_TIMEOUT: "1" },
      testsDir: sandbox.root,
      log: quiet,
    });
    const elapsedMs = Date.now() - started;

    await assertTreeStopped({ marker, exitCode, elapsedMs, label: "run-node-tests" });
  } finally {
    delete process.env.HANG_MARKER;
    if (inheritedTestContext !== undefined) {
      process.env.NODE_TEST_CONTEXT = inheritedTestContext;
    }
  }
});

test("the cargo runner stops a hung cargo and the rustc it spawned", async (t) => {
  const sandbox = makeSandbox("cargo-hang");
  t.after(sandbox.cleanup);

  const marker = join(sandbox.root, "marker");
  // A stand-in for cargo: it hangs and spawns a descendant that hangs too, which
  // is the shape of a real cargo build stuck behind a wedged rustc.
  const script = join(sandbox.root, "hanging-cargo.cjs");
  writeFileSync(
    script,
    [
      'const { spawn } = require("node:child_process");',
      'const { appendFileSync } = require("node:fs");',
      'if (!process.argv.includes("grandchild")) {',
      '  spawn(process.execPath, [__filename, "grandchild"], { stdio: "ignore" });',
      '}',
      'setInterval(',
      '  () => appendFileSync(process.env.HANG_MARKER, process.argv.includes("grandchild") ? "g" : "p"),',
      "  50,",
      ");",
      "",
    ].join("\n"),
  );

  const isWindows = process.platform === "win32";
  const fakeCargo = join(sandbox.root, isWindows ? "hanging-cargo.cmd" : "hanging-cargo");
  writeFileSync(
    fakeCargo,
    isWindows
      ? `@echo off\r\n"${process.execPath}" "${script}" %*\r\n`
      : `#!/usr/bin/env node\nrequire(${JSON.stringify(script)});\n`,
  );
  chmodSync(fakeCargo, 0o755);

  process.env.HANG_MARKER = marker;
  const started = Date.now();
  try {
    const exitCode = await runCargo({
      args: ["test", "--lib"],
      environment: { CARGO: fakeCargo, SLUGTALE_CARGO_TIMEOUT: "1" },
      log: quiet,
    });
    const elapsedMs = Date.now() - started;

    await assertTreeStopped({ marker, exitCode, elapsedMs, label: "run-cargo" });
  } finally {
    delete process.env.HANG_MARKER;
  }
});

test("the cargo runner reports a missing Cargo instead of pretending to test", async () => {
  const logs = [];

  const exitCode = await runCargo({
    args: ["test"],
    environment: { CARGO: join(tmpdir(), "slugtale-no-such-cargo"), PATH: "", HOME: "" },
    log: (message) => logs.push(message),
    spawn: () => {
      throw new Error("cargo must not be spawned when it was not found");
    },
  });

  assert.equal(exitCode, 1);
  assert.ok(logs.some((message) => message.includes("Cargo was not found")));
});
