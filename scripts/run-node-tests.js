#!/usr/bin/env node

// Runs tests/*.test.mjs under node --test with a hard wall-clock cap, so a
// hung frontend test cannot sit there forever. Override per run with
// SLUGTALE_NODE_TIMEOUT=<seconds>, or 0 to disable.

const { join } = require("node:path");
const { spawn } = require("node:child_process");

const timeoutSeconds = Number(process.env.SLUGTALE_NODE_TIMEOUT ?? 120);
const testsDir = join(__dirname, "..", "tests");

const child = spawn(process.execPath, ["--test"], {
  cwd: testsDir,
  stdio: "inherit",
  shell: false,
});

if (timeoutSeconds > 0) {
  const timer = setTimeout(() => {
    console.error("");
    console.error(
      `Frontend tests timed out after ${timeoutSeconds}s. Killing the test tree.`,
    );
    try {
      if (process.platform !== "win32") {
        process.kill(-child.pid, "SIGKILL");
      } else {
        child.kill("SIGKILL");
      }
    } catch {
      // Already exited.
    }
  }, timeoutSeconds * 1000);
  timer.unref();
}

child.on("error", (error) => {
  console.error(error.message);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    console.error(`Frontend tests were terminated by ${signal} (timeout?).`);
    process.exit(124);
  }
  process.exit(code ?? 1);
});
