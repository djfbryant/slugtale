#!/usr/bin/env node

const { execFileSync } = require("node:child_process");
const { readdirSync } = require("node:fs");
const { join } = require("node:path");

if (process.platform !== "darwin") {
  console.error("The clean-exit check only runs on macOS.");
  process.exit(1);
}

const root = join(__dirname, "..");
const appPath = join(
  root,
  "src-tauri",
  "target",
  "debug",
  "bundle",
  "macos",
  "Slugtale.app",
);
const executablePath = join(appPath, "Contents", "MacOS", "slugtale");
const reportsPath = join(
  process.env.HOME,
  "Library",
  "Logs",
  "DiagnosticReports",
);

function crashReports() {
  return new Set(
    readdirSync(reportsPath)
      .filter((name) => name.startsWith("slugtale-") && name.endsWith(".ips"))
      .map((name) => join(reportsPath, name)),
  );
}

function run(command, args, options = {}) {
  return execFileSync(command, args, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  }).trim();
}

function waitFor(predicate, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return true;
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100);
  }
  return predicate();
}

function appIsRunning() {
  try {
    return run("pgrep", ["-f", executablePath]).length > 0;
  } catch {
    return false;
  }
}

const reportsBefore = crashReports();
if (appIsRunning()) {
  console.error("Quit the existing Slugtale instance before running this check.");
  process.exit(1);
}

run("open", ["-n", appPath]);

if (!waitFor(appIsRunning, 5_000)) {
  console.error("Slugtale did not start within five seconds.");
  process.exit(1);
}

// Give setup enough time to enter the eager Whisper/Metal warm-up path.
Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 1_000);
run("osascript", [
  "-e",
  'tell application id "com.slugtale.desktop" to quit',
]);

if (!waitFor(() => !appIsRunning(), 5_000)) {
  console.error("Slugtale did not exit within five seconds.");
  process.exit(1);
}

let newReport;
waitFor(() => {
  newReport = [...crashReports()].find((report) => !reportsBefore.has(report));
  return Boolean(newReport);
}, 15_000);

if (newReport) {
  console.error(`Slugtale generated a crash report during Quit: ${newReport}`);
  process.exit(1);
}

console.log("Slugtale exited cleanly after Whisper warm-up.");
