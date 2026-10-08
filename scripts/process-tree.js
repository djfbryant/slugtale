#!/usr/bin/env node

// One wall-clock cap for the runners in this directory (`run-node-tests.js` and
// `run-cargo.js`). Both need the same three guarantees, and all three are easy
// to get subtly wrong in one copy of the code and correct in the other:
//
// 1. **The child leads its own process group.** On POSIX that means spawning
//    `detached: true`. Without it the child stays in *this* process's group, so
//    a negative-pid kill is aimed at a group this runner does not lead: it
//    either fails with ESRCH and kills nothing, or signals an unrelated group.
//    That is exactly how a runner could report "timed out" while a hung
//    frontend test was still running. The group is also what keeps the child
//    out of the runner's own group, so the timeout kill cannot reach the
//    runner itself.
//
// 2. **A timeout kills the whole tree, not just the direct child.** Cargo
//    spawns rustc; a hung screen test spawns its own children. POSIX has the
//    group signal for this. Windows has no process groups for this purpose and
//    a negative pid means nothing there, so it needs `taskkill /T /F`, which
//    walks the process tree natively.
//
// 3. **A timeout exits 124 on every platform.** The runner reports the
//    conventional "timed out" status rather than whatever the OS happened to
//    report for a killed process, because a Windows kill arrives as an exit
//    code while a POSIX kill arrives as a signal. Deriving the status from the
//    signal alone is what made the old Node runner's Windows path report a
//    plain failure instead of a timeout.
//
// A regular non-timeout exit code is always passed through untouched, and the
// timer is always cleared when the child ends so a finished run holds no timer
// open.

const { spawn: defaultSpawn, spawnSync: defaultSpawnSync } = require("node:child_process");

/** The conventional shell status for "killed because it ran too long". */
const TIMEOUT_EXIT_CODE = 124;

/**
 * Grace period between the tree kill and giving up on the exit event. SIGKILL
 * and `taskkill /F` are not refusable, so this only fires when the kill never
 * reached the child — the failure mode that made this cap unreliable in the
 * first place. Exiting 124 anyway is what keeps the promise that a hung run
 * cannot hold the runner open forever.
 */
const POST_KILL_GRACE_MS = 2000;

/**
 * Spawn a child that leads its own process group on POSIX, so that
 * {@link killProcessTree} can later reach it *and its descendants* with a
 * single signal.
 *
 * @param {object} [system] injection seam for tests.
 * @param {Function} [system.spawn]
 * @param {string} [system.platform]
 * @param {string} command
 * @param {string[]} args
 * @param {object} options node child_process spawn options.
 */
function spawnInOwnGroup(command, args, options = {}, system = {}) {
  const {
    spawn = defaultSpawn,
    platform = process.platform,
  } = system;

  return spawn(command, args, {
    ...options,
    shell: false,
    // Windows keeps its own tree semantics; `killProcessTree` uses `taskkill`
    // there instead of a group signal.
    detached: platform !== "win32",
  });
}

/**
 * Kill a child and every process below it, on whichever platform this is.
 *
 * @param {number|undefined} pid
 * @param {object} [system] injection seam for tests.
 * @returns {boolean} whether a kill was delivered.
 */
function killProcessTree(pid, system = {}) {
  const {
    platform = process.platform,
    spawnSync = defaultSpawnSync,
    kill = (targetPid, signal) => process.kill(targetPid, signal),
  } = system;

  if (!pid) {
    return false;
  }

  if (platform === "win32") {
    // `/T` walks the tree, `/F` forces it. Without a pid on Windows there is
    // no group to signal, so this is the only way to stop a hung cargo build.
    const result = spawnSync("taskkill", ["/pid", String(pid), "/T", "/F"], {
      stdio: "ignore",
    });
    return result.error == null && result.status === 0;
  }

  try {
    // A negative pid is the process *group* led by the detached child, so the
    // rustc workers and any test-spawned grandchildren die with it instead of
    // being orphaned and left holding the machine.
    kill(-pid, "SIGKILL");
    return true;
  } catch {
    // The tree is already gone; the caller still reports the timeout.
    return false;
  }
}

/**
 * Run a child with a hard wall-clock cap that stops the whole tree.
 *
 * Resolves once the child has ended (or once the cap has proven it cannot be
 * stopped), so callers can turn the result into a process exit status without
 * duplicating the signal/exit-code reasoning.
 *
 * @param {object} run
 * @param {string} run.command
 * @param {string[]} run.args
 * @param {object} [run.options] spawn options, e.g. `cwd` and `stdio`.
 * @param {number} run.timeoutSeconds `0` disables the cap.
 * @param {string} run.label human name used in the timeout message.
 * @param {object} [system] injection seam for tests.
 * @returns {Promise<{exitCode: number, timedOut: boolean, signal: string|null}>}
 */
function runChildWithTimeout(run, system = {}) {
  const { command, args, options = {}, timeoutSeconds, label } = run;
  const {
    spawn = defaultSpawn,
    platform = process.platform,
    spawnSync = defaultSpawnSync,
    kill,
    log = console.error,
    graceMs = POST_KILL_GRACE_MS,
  } = system;

  const child = spawnInOwnGroup(command, args, options, { spawn, platform });
  const killSystem = { platform, spawnSync, kill };

  return new Promise((resolve) => {
    let timedOut = false;
    let settled = false;
    let timer = null;
    let graceTimer = null;

    // Every exit path goes through here, so the timers are released exactly
    // once and a late `exit` after the grace window cannot resolve twice.
    const finish = (exitCode, signal) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      clearTimeout(graceTimer);
      resolve({ exitCode, timedOut, signal });
    };

    if (timeoutSeconds > 0) {
      timer = setTimeout(() => {
        timedOut = true;
        log("");
        log(`${label} timed out after ${timeoutSeconds}s. Killing the process tree.`);
        killProcessTree(child.pid, killSystem);

        // If the kill never reached the child there is no exit event coming,
        // and waiting for one is the very hang this cap exists to prevent.
        graceTimer = setTimeout(() => finish(TIMEOUT_EXIT_CODE, null), graceMs);
        graceTimer.unref();
      }, timeoutSeconds * 1000);
      timer.unref();
    }

    child.on("error", (error) => {
      log(error.message);
      finish(1, null);
    });

    child.on("exit", (code, signal) => {
      if (timedOut) {
        // The OS's own report for a killed process is not the contract: a
        // caller (npm, CI) only needs to know the cap fired.
        finish(TIMEOUT_EXIT_CODE, signal ?? null);
        return;
      }
      if (signal) {
        log(`${label} was terminated by ${signal} (timeout?).`);
        finish(TIMEOUT_EXIT_CODE, signal);
        return;
      }
      finish(code ?? 1, null);
    });
  });
}

module.exports = {
  TIMEOUT_EXIT_CODE,
  POST_KILL_GRACE_MS,
  spawnInOwnGroup,
  killProcessTree,
  runChildWithTimeout,
};
