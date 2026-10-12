#!/usr/bin/env node

// hung frontend test cannot sit there forever. Override per run with
// SLUGTALE_NODE_TIMEOUT=<seconds>, or 0 to disable.

const { join } = require("node:path");
const { runChildWithTimeout } = require("./process-tree.js");

const DEFAULT_TIMEOUT_SECONDS = 120;
const defaultTestsDir = join(__dirname, "..", "tests");

async function runNodeTests({
  environment = process.env,
  testsDir = defaultTestsDir,
  ...system
} = {}) {
  const { exitCode } = await runChildWithTimeout(
    {
      command: process.execPath,
      args: ["--test"],
      options: { cwd: testsDir, stdio: "inherit" },
      timeoutSeconds: Number(environment.SLUGTALE_NODE_TIMEOUT ?? DEFAULT_TIMEOUT_SECONDS),
      label: "Frontend tests",
    },
    system,
  );

  return exitCode;
}

if (require.main === module) {
  runNodeTests().then(
    (exitCode) => {
      process.exit(exitCode);
    },
    (error) => {
      console.error(error.message);
      process.exit(1);
    },
  );
}

module.exports = { runNodeTests };
