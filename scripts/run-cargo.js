#!/usr/bin/env node

const { accessSync, constants } = require("node:fs");
const { delimiter, join } = require("node:path");
const { spawnSync } = require("node:child_process");

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
