#!/usr/bin/env node

// Runs tests/*.test.mjs under node --test with a hard wall-clock cap, so a
// hung frontend test cannot sit there forever. Override per run with
// SLUGTALE_NODE_TIMEOUT=<seconds>, or 0 to disable.
//
// The cap, the process group, and the tree kill live in ./process-tree.js
// because run-cargo.js needs exactly the same guarantees: this runner used to
// signal a process group it did not lead, so a hung test could survive a
// timeout that had already reported itself.

const { join } = require("node:path");
const { runChildWithTimeout } = require("./process-tree.js");

const DEFAULT_TIMEOUT_SECONDS = 120;
const defaultTestsDir = join(__dirname, "..", "tests");

/**
 * @param {object} [options] injection seam for tests.
 * @param {string} [options.testsDir] discovery root; defaults to `tests/`.
 * @param {object} [options.environment]
 * @returns {Promise<number>} the exit code for the process.
 */
async function runNodeTests({
  environment = process.env,
  testsDir = defaultTestsDir,
  ...system
} = {}) {
  const { exitCode } = await runChildWithTimeout(
    {
      command: process.execPath,
      // Discovery stays inside tests/ by running node's test runner with that
      // directory as its cwd, so a stray *.test.mjs elsewhere in the checkout
      // cannot silently join or break the frontend suite.
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
