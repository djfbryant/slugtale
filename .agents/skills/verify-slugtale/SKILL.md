---
name: verify-slugtale
description: Verify Slugtale's real Tauri desktop settings, dictation, and local file side effects. Use after a user-facing change or to collect desktop behavior evidence on macOS.
---

# Verify Slugtale

**Draft: the UI proof is blocked.** On 2026-10-12, the helper built and launched a signed, isolated app, passed doctor, captured local state, and cleaned up while preserving evidence. Native desktop control returned `cgWindowNotFound` for the test window and timed out for T3 Code. No mapped UI feature has been driven. Complete the one-feature proof below before calling this skill verified. See [validation.md](validation.md).

Slugtale is a native Tauri desktop app with embedded HTML in WKWebView, a tray menu, and a floating dictation bar. It has no web dev server, port, account, or login. Its ASR example CLIs and `tests/harness.mjs` are secondary surfaces. The test harness supplies simulated DOM and Tauri calls: it is useful for unit tests, but cannot prove desktop behavior. Serving `src/` in a browser also runs fallback state and cannot prove a saved setting or dictation.

Read [features/README.md](features/README.md), then every feature file relevant to the change. The feature map is the maintained coverage source.

## Launch

From the repository root:

```sh
node .agents/skills/verify-slugtale/helpers/session.mjs launch
```

Prerequisites: macOS, Xcode Command Line Tools, Rust/Cargo, Node/npm, installed npm dependencies (`npm ci`), and the stable `Slugtale Dev` signing certificate described in the root README. Set `SLUGTALE_SIGN_IDENTITY` for another existing certificate. Do not create a certificate, reset permissions, install over `/Applications/Slugtale.app`, or change live settings as part of a verification run.

The helper uses the repository's `scripts/run-tauri.js build --debug --bundles app --config` path with its normal macOS runtime features: Whisper, Metal, Voice Activation, and Phonon MLX. Optional features come from `SLUGTALE_ENGINE_FEATURES`. It caps the build at ten minutes. Unset `CARGO_TARGET_DIR` before launch. Do not run another Tauri build in this checkout during verification.

Verification scaffolding changes only the bundle identifier, product/window title, initial Settings visibility, and updater artifact signing. Source, frontend, backend commands, and real persistence remain intact. A unique identifier gives the run its own `~/Library/Application Support/com.slugtale.verify.r…` directory. Settings start at real defaults: no hotkey/model, usage off, voice activation off, and login launch off. No seed files or permission grants are injected. The helper signs a private bundle under the system temporary directory and opens it through Launch Services.

The helper prints `Evidence:` and JSON with `run`, `bundle`, `identifier`, `pid`, and `settingsTitle`. Copy the printed evidence path into the shell variable `SLUGTALE_VERIFY_RUN`. Keep it for all later commands. Read `session.json` there when resuming a run.

Readiness requires both a passing doctor and a native UI snapshot showing the printed Settings title, `Search settings`, and loaded controls. `Not ready to dictate` is expected for fresh settings; it does not prevent settings checks. The initial visible window is scaffolding, so it does not prove the tray's `Settings…` entry point.

Run cleanup after each failure and at the end:

```sh
node .agents/skills/verify-slugtale/helpers/session.mjs cleanup "$SLUGTALE_VERIFY_RUN"
```

The helper is proven only on macOS. Windows/Linux have adapters but need real hardware verification. Do not present a Mac proof as cross-platform proof.

## Doctor

One read-only check, run before driving and whenever the instance looks wrong:

```sh
node .agents/skills/verify-slugtale/helpers/session.mjs doctor "$SLUGTALE_VERIFY_RUN"
```

Require `healthy: true`. This checks the owned PID and start identity, bundle identifier/version, code signature, executable SHA-256, source fingerprint, and owned data directory. It prints the expected window title. It does not certify Microphone/Accessibility grants, model availability, or dictation readiness; observe those in the real UI. Do not use `strings` to identify embedded frontend contents: Tauri compresses them.

## Drive

Use native desktop control through `mcp__cua_repl.js`. Follow its first-call documentation. Bind with `await cua.getApp(...)`, passing the exact bundle path printed by launch. The printed identifier is another valid selector. Check the window title against `settingsTitle`; never bind to plain `Slugtale`, which can select the user's installed app.

Read `await app.getAXState()` after each action. Choose the numeric index of the named control from that fresh tree, assign it to `index`, then use `await app.click(index)`. Numeric indices are temporary, not stable selectors. For input, use `await app.setValue(index, 'pause')` or focus the field with `click` and use `await app.typeText('pause')`. Use `await app.pressKey('Escape')` for cancel. Prefer accessible names listed in the feature map. DOM IDs there identify the source control; native `click` does not accept CSS selectors. Use a fresh screenshot and coordinates only if a real control is absent from the AX tree.

Do not call Tauri commands, frontend setters, synthetic DOM events, or test-only endpoints to produce the result. A real user must be able to take the same actions.

When desktop control is unavailable, record its exact error and the failed entry point, run cleanup, and report the UI path as blocked. A passing doctor or unit test is not an alternative UI proof.

For the first proof, use `transcription-cleanup` in [transcription-and-cleanup.md](features/transcription-and-cleanup.md): open `Text & cleanup`, choose `Clean`, observe the changed example and `Saved. Applies to your next dictation.`, capture state, then choose `Basic` and capture again. Require persisted `transcript_cleanup` to change to `clean-dictation` and back to `basic`. This path needs no model, microphone, external app, or permission grant.

## Evidence

Artifacts live in the printed run directory under `.agents/skills/verify-slugtale/evidence/`. They are ignored by Git and survive cleanup. Build logs, launch config, session identity, app logs, and cleanup report are automatic.

Capture read-only persisted state after each meaningful action:

```sh
node .agents/skills/verify-slugtale/helpers/session.mjs capture "$SLUGTALE_VERIFY_RUN"
```

This writes a timestamped `state-*.json` with build identity, real settings, usage-file presence, and model-directory entries. A missing settings file means defaults, not a completed save. For usage/model checks, also copy or inspect the relevant real files under the printed `dataDir`; directory presence alone is insufficient.

Save native AX snapshots and screenshots before the action and after the result. In the CUA runtime, use `await app.getAXState({emit: false, disableDiffing: true})` and `await app.getScreenshot({emit: false})`. Write the returned string/PNG bytes with `node:fs/promises` into the same run directory. Preserve the observed indices, control names, actions, results, and errors in `actions.jsonl`; include feature ID and entry point. Keep the local evidence on this machine.

Proof must show the action and result, not just a final screen. For a mutation, require the visible result plus real persisted state. To prove restart persistence, quit the private app through its tray, reopen the same private bundle, refresh the session PID/start identity before doctor, and observe the setting again; the current helper intentionally does not automate that extra lifecycle. Do not claim restart persistence from a file snapshot alone.

For dictation, also capture the destination's text before/after, stop/cancel state transitions, and insertion or clipboard fallback. Settings examples are demonstrations, not ASR output. Use only synthetic, non-sensitive speech in scratch documents. Audio/transcripts must stay local. Mocks are allowed only at an existing production boundary and must be disclosed; they do not prove microphone capture or OS insertion.

This launcher is not a dry run. Builds may fetch dependencies. `Download`/`Install` and `Check now` use the network; `Open release page` opens a browser. Do not select them for a settings-only proof. If proving a claimed offline/dry-run path, record the network/file/browser effects directly; a label or unchanged settings file does not prove no network traffic.

## Cleanup

Run the cleanup command even after failed iterations. It stops only the owned PID and owned descendants, checks process identity before sending signals, removes the private bundle/build bundle and marked data directory, and releases its launch lock. It never kills by process name. It preserves the evidence directory and writes `cleanup.json`. Repeated cleanup is safe.

Confirm `processStopped`, `scratchRemoved`, `dataRemoved`, and `evidencePreserved` are true. Also confirm the action/screenshot/state artifacts still exist. If ownership checks fail, retain scratch state and report the exact failure; do not delete unknown paths or remove a lock blindly. The helper prevents simultaneous verification launches in this checkout. Different app identifiers isolate files, but global hotkeys, microphone, clipboard, and destination focus are still shared OS resources: never run two dictation drives at once.

Do not enable Launch at login for the disposable bundle; it would create an OS login item beyond the run's files. Permission grants are tied to the run identifier and do not transfer from the installed app. If a full speech proof needs new grants, let the user handle the OS permission prompt. Report that path as blocked until grants exist.

## Helpers

The executable [helpers/session.mjs](helpers/session.mjs) supports only `launch`, `doctor EVIDENCE_DIR`, `capture EVIDENCE_DIR`, and `cleanup EVIDENCE_DIR`. All four invocations appear above. It needs no extra npm packages. `launch` prints its paths before building so a failed run can be inspected and cleaned up. Build failures automatically attempt cleanup; inspect `failure.txt` and `cleanup.json`.

Run `npm test` for repository checks when app code changes. These checks supplement real desktop evidence. Use `/maintain-verification-skill` when app behavior or the map changes.
