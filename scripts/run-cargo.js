#!/usr/bin/env node

const { accessSync, constants } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync: defaultSpawnSync } = require("node:child_process");
const { runChildWithTimeout } = require("./process-tree.js");

const root = join(__dirname, "..");
const rustCrate = join(root, "src-tauri");

function isExecutable(file) {
  try {
    accessSync(file, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function pathCandidates(command, pathValue) {
  if (!pathValue) {
    return [];
  }

  return pathValue
    .split(delimiter)
    .filter(Boolean)
    .map((entry) => join(entry, command));
}

function resolveCargo(environment = process.env) {
  const explicitCargo = environment.CARGO;
  if (explicitCargo && isExecutable(explicitCargo)) {
    return explicitCargo;
  }

  const pathWithRustup = [
    environment.PATH,
    environment.HOME ? join(environment.HOME, ".cargo", "bin") : null,
  ]
    .filter(Boolean)
    .join(delimiter);

  return pathCandidates(
    process.platform === "win32" ? "cargo.exe" : "cargo",
    pathWithRustup,
  ).find(isExecutable);
}

function missingCargo() {
  return [
    "Cargo was not found.",
    "",
    "Slugtale's Rust crate lives in src-tauri and requires Rust stable.",
    "Install Rust with rustup: https://rustup.rs/",
    "",
    "If Cargo is already installed somewhere else, set CARGO=/path/to/cargo or add it to PATH.",
  ].join("\n");
}

// Wall-clock cap on any cargo invocation. A hung test or build must never be
// able to eat the machine: when the cap fires, the whole cargo process tree
// (cargo plus every rustc it spawned) is killed, not just the parent.
// Override per run with SLUGTALE_CARGO_TIMEOUT=<seconds>, or 0 to disable.
//
// The cap, the process group and the tree kill live in ./process-tree.js,
// shared with run-node-tests.js. This runner used to send the POSIX group
// signal on every platform, which cannot work on Windows.
const DEFAULT_TIMEOUT_SECONDS = 600;

/**
 * @param {object} [options] injection seam for tests.
 * @param {string[]} options.args cargo arguments.
 * @param {object} [options.environment]
 * @returns {Promise<number>} the exit code for the process.
 */
async function runCargo({
  args = process.argv.slice(2),
  environment = process.env,
  log = console.error,
  ...system
} = {}) {
  const cargo = resolveCargo(environment);

  if (!cargo) {
    log(missingCargo());
    return 1;
  }

  const timeoutSeconds = Number(
    environment.SLUGTALE_CARGO_TIMEOUT ?? DEFAULT_TIMEOUT_SECONDS,
  );

  if (timeoutSeconds > 0 && args.length > 0) {
    const { exitCode } = await runChildWithTimeout(
      {
        command: cargo,
        args,
        options: { cwd: rustCrate, stdio: "inherit" },
        timeoutSeconds,
        label: "Cargo",
      },
      { ...system, log },
    );

    return exitCode;
  }

  // No cap requested: nothing to spawn in a group or kill later, so the
  // blocking call is enough and keeps the runner's own timeout out of it.
  const spawnSync = system.spawnSync ?? defaultSpawnSync;
  const result = spawnSync(cargo, args, {
    cwd: rustCrate,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    log(result.error.message);
    return 1;
  }

  return result.status ?? 1;
}

if (require.main === module) {
  runCargo().then(
    (exitCode) => {
      process.exit(exitCode);
    },
    (error) => {
      console.error(error.message);
      process.exit(1);
    },
  );
}

module.exports = { runCargo, resolveCargo, missingCargo, rustCrate };
