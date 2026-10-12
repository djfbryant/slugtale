#!/usr/bin/env node

// 1. **The child leads its own process group.** On POSIX that means spawning
//    `detached: true`. Without it the child stays in *this* process's group, so
//    a negative-pid kill is aimed at a group this runner does not lead: it
//    either fails with ESRCH and kills nothing, or signals an unrelated group.
//    That is exactly how a runner could report "timed out" while a hung
//    frontend test was still running. The group is also what keeps the child
//    out of the runner's own group, so the timeout kill cannot reach the
//    runner itself.
// 2. **A timeout kills the whole tree, not just the direct child.** Cargo
//    spawns rustc; a hung screen test spawns its own children. POSIX has the
//    group signal for this. Windows has no process groups for this purpose and
//    a negative pid means nothing there, so it needs `taskkill /T /F`, which
//    walks the process tree natively.
// 3. **A timeout exits 124 on every platform.** The runner reports the
//    conventional "timed out" status rather than whatever the OS happened to
//    report for a killed process, because a Windows kill arrives as an exit
//    code while a POSIX kill arrives as a signal. Deriving the status from the
//    signal alone is what made the old Node runner's Windows path report a
//    plain failure instead of a timeout.
// A regular non-timeout exit code is always passed through untouched, and the
// timer is always cleared when the child ends so a finished run holds no timer
// open. While the call is pending, both timers stay referenced: they are the
// work that settles the returned promise, so an otherwise idle event loop must
// not exit out from under it (Node 20's test runner fails a test whose promise
// is still pending then, and a caller would never see its 124). The hold is
// bounded by the cap plus one grace window, and `finish` clears both timers.

const { spawn: defaultSpawn, spawnSync: defaultSpawnSync } = require("node:child_process");

const TIMEOUT_EXIT_CODE = 124;

/**
 * Grace period between the tree kill and giving up on the exit event. SIGKILL
 * and `taskkill /F` are not refusable, so this only fires when the kill never
 * reached the child — the failure mode that made this cap unreliable in the
 * first place. Exiting 124 anyway is what keeps the promise that a hung run
 * cannot hold the runner open forever.
 */
const POST_KILL_GRACE_MS = 2000;

function spawnInOwnGroup(command, args, options = {}, system = {}) {
  const {
    spawn = defaultSpawn,
    platform = process.platform,
  } = system;

  return spawn(command, args, {
    ...options,
    shell: false,
    detached: platform !== "win32",
  });
}

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
    return false;
  }
}

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

        graceTimer = setTimeout(() => finish(TIMEOUT_EXIT_CODE, null), graceMs);
      }, timeoutSeconds * 1000);
    }

    child.on("error", (error) => {
      log(error.message);
      finish(1, null);
    });

    child.on("exit", (code, signal) => {
      if (timedOut) {
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
