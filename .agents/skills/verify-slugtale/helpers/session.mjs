#!/usr/bin/env node
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync, openSync, closeSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const skill = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(skill, '../../..');
const evidenceRoot = join(skill, 'evidence');
const lock = join(evidenceRoot, '.launch-lock');
const [command, runArgument] = process.argv.slice(2);
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

function output(executable, args) {
  const result = spawnSync(executable, args, { encoding: 'utf8', timeout: 15000 });
  if (result.error || result.status !== 0) throw new Error(`${executable} ${args.join(' ')}: ${result.error?.message || result.stderr}`);
  return result.stdout.trim();
}

function sourceHash() {
  const files = [];
  function walk(directory) {
    for (const entry of readdirSync(join(root, directory), { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) files.push(path);
    }
  }
  for (const directory of ['src', 'scripts', 'src-tauri/src', 'src-tauri/swift', 'src-tauri/phonon', 'src-tauri/capabilities', 'src-tauri/icons']) walk(directory);
  files.push('package.json', 'package-lock.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'src-tauri/build.rs', 'src-tauri/Info.plist', 'src-tauri/tauri.conf.json');
  const hash = createHash('sha256');
  for (const file of files.sort()) hash.update(file).update(readFileSync(join(root, file)));
  return hash.digest('hex');
}

function save(run, state) {
  writeFileSync(join(run, 'session.json'), JSON.stringify(state, null, 2) + '\n');
}

function load() {
  if (!runArgument) throw new Error('Supply the evidence directory printed by launch.');
  const run = resolve(runArgument);
  if (dirname(run) !== evidenceRoot) throw new Error('Run must be directly inside this skill\'s evidence directory.');
  const state = JSON.parse(readFileSync(join(run, 'session.json'), 'utf8'));
  if (state.root !== root || state.run !== run || !/^[a-f0-9]{32}$/.test(state.token)) throw new Error('Wrong session identity.');
  if (state.identifier !== `com.slugtale.verify.r${state.token}` || state.dataDir !== join(homedir(), 'Library/Application Support', state.identifier)) throw new Error('Wrong data directory.');
  if (!state.scratch.startsWith(join(realpathSync(tmpdir()), 'slugtale-verify-')) || state.executable !== join(state.scratch, state.product + '.app/Contents/MacOS/slugtale')) throw new Error('Wrong scratch directory.');
  if (existsSync(state.scratch) && readFileSync(join(state.scratch, '.owner'), 'utf8') !== state.token) throw new Error('Scratch owner changed.');
  if (existsSync(state.dataDir) && readFileSync(join(state.dataDir, '.verification-owner'), 'utf8') !== state.token) throw new Error('Data owner changed.');
  return { run, state };
}

function processIdentity(pid) {
  if (!pid) return null;
  const result = spawnSync('ps', ['-p', String(pid), '-o', 'lstart=', '-o', 'command='], { encoding: 'utf8' });
  return result.status === 0 ? result.stdout.trim() : null;
}

async function stopOwned(pid, identity, group = false) {
  if (!pid || !processIdentity(pid)) return;
  if (!identity || processIdentity(pid) !== identity) throw new Error(`Refuse to stop PID ${pid}: identity changed.`);
  process.kill(group ? -pid : pid, 'SIGTERM');
  for (let count = 0; count < 50; count++) {
    if (!processIdentity(pid)) return;
    await pause(100);
  }
  if (processIdentity(pid) !== identity) throw new Error('Process identity changed during teardown.');
  process.kill(group ? -pid : pid, 'SIGKILL');
  for (let count = 0; count < 50; count++) {
    if (!processIdentity(pid)) return;
    await pause(100);
  }
  throw new Error(`PID ${pid} did not stop.`);
}

async function cleanup(run, state) {
  if (!state.pid) {
    const matches = output('ps', ['-axo', 'pid=,command=']).split('\n').filter(line => line.trim().replace(/^\d+\s+/, '') === state.executable);
    if (matches.length > 1) throw new Error('Multiple processes for this private bundle; keep scratch state for inspection.');
    if (matches.length === 1) {
      state.pid = Number(matches[0].trim().split(/\s+/)[0]);
      state.processIdentity = processIdentity(state.pid);
    }
  }
  const descendants = [];
  if (state.pid && processIdentity(state.pid) === state.processIdentity) {
    const rows = output('ps', ['-axo', 'pid=,ppid=']).split('\n').map(line => line.trim().split(/\s+/).map(Number));
    function collect(parent) {
      for (const [pid, ppid] of rows) if (ppid === parent) {
        collect(pid);
        descendants.push({ pid, identity: processIdentity(pid) });
      }
    }
    collect(state.pid);
  }
  await stopOwned(state.pid, state.processIdentity);
  for (const child of descendants) await stopOwned(child.pid, child.identity);
  await stopOwned(state.buildPid, state.buildIdentity, true);
  rmSync(state.scratch, { recursive: true, force: true });
  rmSync(state.dataDir, { recursive: true, force: true });
  if (state.buildBundle && existsSync(state.buildBundle)) {
    const plist = join(state.buildBundle, 'Contents/Info.plist');
    if (output('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', plist]) !== state.identifier) throw new Error('Build bundle owner changed.');
    rmSync(state.buildBundle, { recursive: true, force: true });
  }
  if (existsSync(lock) && readFileSync(join(lock, 'owner'), 'utf8') === state.token) rmSync(lock, { recursive: true, force: true });
  state.cleanedAt = new Date().toISOString();
  save(run, state);
  const report = { cleanedAt: state.cleanedAt, processStopped: !processIdentity(state.pid), scratchRemoved: !existsSync(state.scratch), dataRemoved: !existsSync(state.dataDir), evidencePreserved: existsSync(join(run, 'session.json')) };
  writeFileSync(join(run, 'cleanup.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report, null, 2));
}

async function launch() {
  if (process.platform !== 'darwin') throw new Error('This launcher is proven on macOS only. See SKILL.md for other platforms.');
  if (process.env.CARGO_TARGET_DIR) throw new Error('Unset CARGO_TARGET_DIR: this helper uses the repo target directory.');
  const identity = process.env.SLUGTALE_SIGN_IDENTITY || 'Slugtale Dev';
  if (identity === '-' || !/^ *\d+\) [A-Fa-f0-9]+ "/m.test(output('security', ['find-identity', '-v', '-p', 'codesigning', '-s', identity]))) throw new Error(`Missing stable signing certificate: ${identity}`);
  mkdirSync(evidenceRoot, { recursive: true });
  const token = randomUUID().replaceAll('-', '');
  mkdirSync(lock);
  writeFileSync(join(lock, 'owner'), token);
  const run = join(evidenceRoot, new Date().toISOString().replaceAll(':', '-') + '-' + token.slice(0, 8));
  mkdirSync(run);
  const scratch = realpathSync(mkdtempSync(join(tmpdir(), 'slugtale-verify-')));
  writeFileSync(join(scratch, '.owner'), token);
  const product = `Slugtale Verify ${token.slice(0, 8)}`;
  const identifier = `com.slugtale.verify.r${token}`;
  const dataDir = join(homedir(), 'Library/Application Support', identifier);
  const state = { root, run, token, scratch, product, identifier, dataDir, bundle: join(scratch, product + '.app'), executable: join(scratch, product + '.app/Contents/MacOS/slugtale'), sourceHash: sourceHash(), version: JSON.parse(readFileSync(join(root, 'package.json'))).version, startedAt: new Date().toISOString(), signingIdentity: identity, extraFeatures: process.env.SLUGTALE_ENGINE_FEATURES || '' };
  save(run, state);
  console.log(`Evidence: ${run}`);
  try {
    mkdirSync(dataDir);
    writeFileSync(join(dataDir, '.verification-owner'), token);
    const config = JSON.parse(readFileSync(join(root, 'src-tauri/tauri.conf.json')));
    config.identifier = identifier;
    config.productName = product;
    config.bundle.createUpdaterArtifacts = false;
    const settings = config.app.windows.find(window => window.label === 'settings');
    settings.title = product + ' Settings';
    settings.visible = true;
    const configPath = join(scratch, 'tauri.verify.json');
    writeFileSync(configPath, JSON.stringify(config, null, 2));
    cpSync(configPath, join(run, 'tauri.verify.json'));
    const log = openSync(join(run, 'build.log'), 'a');
    const child = spawn(process.execPath, [join(root, 'scripts/run-tauri.js'), 'build', '--debug', '--bundles', 'app', '--config', configPath], { cwd: root, env: { ...process.env, CARGO_TARGET_DIR: join(root, 'src-tauri/target') }, detached: true, stdio: ['ignore', log, log] });
    closeSync(log);
    state.buildPid = child.pid;
    state.buildIdentity = processIdentity(child.pid);
    save(run, state);
    let timedOut = false;
    const timer = setTimeout(() => { timedOut = true; stopOwned(state.buildPid, state.buildIdentity, true).catch(error => console.error(error.message)); }, 600000);
    const exit = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', code => resolve(code)); }).finally(() => clearTimeout(timer));
    state.buildPid = null;
    save(run, state);
    if (exit !== 0 || timedOut) throw new Error(`Build failed (exit ${exit}, timed out ${timedOut}). Read ${join(run, 'build.log')}.`);
    if (state.sourceHash !== sourceHash()) throw new Error('Source changed during build. Retry after cleanup.');
    state.buildBundle = join(root, 'src-tauri/target/debug/bundle/macos', product + '.app');
    save(run, state);
    cpSync(state.buildBundle, state.bundle, { recursive: true });
    output('codesign', ['--force', '--deep', '--sign', identity, '--identifier', identifier, state.bundle]);
    output('codesign', ['--verify', '--deep', '--strict', state.bundle]);
    state.binaryHash = sha(state.executable);
    writeFileSync(join(run, 'app.log'), '');
    output('open', ['-n', '-a', state.bundle, '--stdout', join(run, 'app.log'), '--stderr', join(run, 'app.log')]);
    for (let count = 0; count < 50; count++) {
      const matches = output('ps', ['-axo', 'pid=,command=']).split('\n').filter(line => line.trim().replace(/^\d+\s+/, '') === state.executable);
      if (matches.length > 1) throw new Error('More than one process for the private bundle.');
      if (matches.length === 1) {
        state.pid = Number(matches[0].trim().split(/\s+/)[0]);
        state.processIdentity = processIdentity(state.pid);
        save(run, state);
        break;
      }
      await pause(100);
    }
    await pause(1000);
    if (!processIdentity(state.pid)) throw new Error('App exited before readiness. Read app.log.');
    console.log(JSON.stringify({ run, pid: state.pid, bundle: state.bundle, identifier, settingsTitle: settings.title }, null, 2));
  } catch (error) {
    writeFileSync(join(run, 'failure.txt'), error.stack + '\n');
    await cleanup(run, state);
    throw error;
  }
}

function doctor(state) {
  if (state.cleanedAt) throw new Error('Session already cleaned.');
  if (processIdentity(state.pid) !== state.processIdentity || !state.processIdentity?.includes(state.executable)) throw new Error('Owned app is not running.');
  if (sha(state.executable) !== state.binaryHash || sourceHash() !== state.sourceHash) throw new Error('Build or source changed. Launch a fresh session.');
  const plist = join(state.bundle, 'Contents/Info.plist');
  if (output('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleIdentifier', plist]) !== state.identifier) throw new Error('Wrong bundle identifier.');
  if (output('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleShortVersionString', plist]) !== state.version) throw new Error('Wrong app version.');
  output('codesign', ['--verify', '--deep', '--strict', state.bundle]);
  return { healthy: true, pid: state.pid, identifier: state.identifier, version: state.version, sourceHash: state.sourceHash, binaryHash: state.binaryHash, dataDir: state.dataDir, settingsTitle: state.product + ' Settings', scope: 'Process and build healthy; confirm the real UI with cua before driving. Dictation readiness is separate.' };
}

try {
  if (command === 'launch') await launch();
  else if (['doctor', 'capture', 'cleanup'].includes(command)) {
    const { run, state } = load();
    if (command === 'cleanup') await cleanup(run, state);
    else if (command === 'doctor') console.log(JSON.stringify(doctor(state), null, 2));
    else {
      const report = doctor(state);
      const settings = join(state.dataDir, 'settings.json');
      const name = `state-${new Date().toISOString().replaceAll(':', '-')}.json`;
      writeFileSync(join(run, name), JSON.stringify({ ...report, settings: existsSync(settings) ? JSON.parse(readFileSync(settings)) : null, settingsFilePresent: existsSync(settings), usageFilePresent: existsSync(join(state.dataDir, 'usage.json')), modelFiles: existsSync(join(state.dataDir, 'models')) ? readdirSync(join(state.dataDir, 'models')) : [] }, null, 2) + '\n');
      console.log(join(run, name));
    }
  } else throw new Error('Usage: session.mjs launch | doctor EVIDENCE_DIR | capture EVIDENCE_DIR | cleanup EVIDENCE_DIR');
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
