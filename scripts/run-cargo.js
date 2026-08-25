#!/usr/bin/env node

const { accessSync, constants } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawn, spawnSync } = require("node:child_process");

const root = join(__dirname, "..");
const rustCrate = join(root, "src-tauri");
const args = process.argv.slice(2);
const cargoName = process.platform === "win32" ? "cargo.exe" : "cargo";

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

function resolveCargo() {
  const explicitCargo = process.env.CARGO;
  if (explicitCargo && isExecutable(explicitCargo)) {
    return explicitCargo;
  }

  const pathWithRustup = [
    process.env.PATH,
    process.env.HOME ? join(process.env.HOME, ".cargo", "bin") : null,
  ]
    .filter(Boolean)
    .join(delimiter);

  return pathCandidates(cargoName, pathWithRustup).find(isExecutable);
}

const cargo = resolveCargo();

if (!cargo) {
  console.error("Cargo was not found.");
  console.error("");
  console.error("Slugtale's Rust crate lives in src-tauri and requires Rust stable.");
  console.error("Install Rust with rustup: https://rustup.rs/");
  console.error("");
  console.error(
    "If Cargo is already installed somewhere else, set CARGO=/path/to/cargo or add it to PATH.",
  );
  process.exit(1);
}

// Wall-clock cap on any cargo invocation. A hung test or build must never be
// able to eat the machine: when the cap fires, the whole cargo process group
// (cargo plus every rustc it spawned) is killed, not just the parent.
// Override per run with SLUGTALE_CARGO_TIMEOUT=<seconds>, or 0 to disable.
const timeoutSeconds = Number(process.env.SLUGTALE_CARGO_TIMEOUT ?? 600);

if (timeoutSeconds > 0 && args.length > 0) {
  const child = spawn(cargo, args, {
    cwd: rustCrate,
    stdio: "inherit",
    shell: false,
    detached: process.platform !== "win32",
  });

  const timer = setTimeout(() => {
    console.error("");
    console.error(
      `Cargo timed out after ${timeoutSeconds}s. Killing the cargo process tree.`,
    );
    console.error("(Set SLUGTALE_CARGO_TIMEOUT=<seconds> to change the cap.)");
    try {
      // Negative pid kills the detached child's whole process group, so the
      // rustc workers die with it instead of being orphaned.
      process.kill(-child.pid, "SIGKILL");
    } catch {
      // The child already exited.
    }
  }, timeoutSeconds * 1000);
  timer.unref();

  child.on("error", (error) => {
    clearTimeout(timer);
    console.error(error.message);
    process.exit(1);
  });

  child.on("exit", (code, signal) => {
    clearTimeout(timer);
    if (signal) {
      console.error(`Cargo was terminated by ${signal} (timeout?).`);
      process.exit(124); // conventional "timed out" exit code
    }
    process.exit(code ?? 1);
  });
} else {
  const result = spawnSync(cargo, args, {
    cwd: rustCrate,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    console.error(result.error.message);
    process.exit(1);
  }

  process.exit(result.status ?? 1);
}
