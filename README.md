# Slugtale

Slugtale is a local-first desktop dictation app. It records speech, transcribes
it on the user's machine, performs a small deterministic cleanup pass, and
inserts the final text into the app the user was already typing in.

The current implementation is a developer-run macOS app built with Tauri 2,
Rust, a static HTML settings UI, and a local Whisper runtime. It is intentionally
early, but it is already shaped around the production intent: a resident menu
bar app that stays out of the way until the user presses a hotkey.

## What Works Today

- macOS menu bar resident app with a tray menu and settings window.
- Dictation readiness checks for microphone access, Accessibility/text
  insertion access, hotkey configuration, and local model availability.
- Configurable hotkey with toggle-to-dictate and hold-to-dictate modes.
- Floating dictation bar with recording, stop, cancel, audio level, and
  transcribing states.
- Local model download, reveal, and deletion for Whisper `base.en`
  (`ggml-base.en.bin`).
- Local audio capture through `cpal`.
- Local English transcription through `whisper-rs` when built with the
  `local-whisper-runtime` feature.
- Clipboard-free macOS text insertion first, with clipboard rescue and a local
  notification if direct insertion fails.
- Local non-secret settings file and optional local diagnostic log.
- Unit tests for settings, readiness, model handling, hotkeys, shell behavior,
  audio capture, transcription boundaries, and workflow behavior.

## Product Intent

Slugtale is meant to become a small, trustworthy dictation tool for people who
want voice input without sending captured audio or transcript text to a remote
service.

The first version deliberately keeps the workflow narrow:

1. The user focuses a text field in another app.
2. The user starts dictation with a configured hotkey.
3. Slugtale records locally and shows the dictation bar.
4. The user stops dictation.
5. Slugtale transcribes locally with Whisper.
6. Slugtale cleans simple whitespace/capitalization issues.
7. Slugtale inserts the final transcription into the original text target.
8. If insertion fails, Slugtale copies the transcription to the clipboard so the
   user can paste it manually.

There is no dictation history in the first version, no live partial transcript,
no remote transcription service, and no telemetry.

## Requirements

The runnable slice is macOS-first. The code keeps operating-system behavior
behind platform adapters so Windows support can be added later, but text
insertion and permission shortcuts are currently implemented for macOS.

For macOS development you need:

- macOS with Xcode Command Line Tools.
- Node.js and npm.
- Rust and Cargo, usually installed with `rustup`.
- A local macOS code-signing identity for the developer-run app bundle.

The npm scripts look for Cargo on `PATH`, then at `$HOME/.cargo/bin/cargo`. If
Cargo is somewhere else, set `CARGO=/path/to/cargo`.

## Quick Start On macOS

Clone and install dependencies:

```sh
git clone https://github.com/djfbryant/slugtale.git
cd slugtale
npm install
```

Install system prerequisites if needed:

```sh
xcode-select --install
```

Install Rust if `cargo --version` does not work:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Create the local signing identity expected by `npm run dev`:

1. Open Keychain Access.
2. Choose `Certificate Assistant > Create a Certificate`.
3. Name it `Slugtale Dev`.
4. Set `Identity Type` to `Self Signed Root`.
5. Set `Certificate Type` to `Code Signing`.
6. Create it in the login keychain.

Then run the app:

```sh
npm run dev
```

On macOS, this builds a debug `.app`, signs it with the `Slugtale Dev` identity,
verifies the signature, and opens the app bundle. Slugtale runs as an accessory
app, so look for the Slugtale icon in the menu bar rather than the Dock. Use the
menu bar item to open settings.

If you already have a different code-signing identity, use it like this:

```sh
SLUGTALE_SIGN_IDENTITY="Your Code Signing Identity" npm run dev
```

## First Run Setup

Open Slugtale from the menu bar and complete the readiness checklist:

1. Grant microphone permission.
2. Grant Accessibility permission so Slugtale can insert text into other apps.
3. Download the local `base.en` model from the settings window.
4. Choose a hotkey.
5. Choose toggle or hold activation mode.

The local model is downloaded from the `whisper.cpp` Hugging Face repository and
stored in the app data directory. The settings window shows the exact model
path and can reveal it in Finder.

## Using Dictation

1. Click into a text field, editor, or document in another app.
2. Press the configured hotkey.
3. Speak while the dictation bar is visible.
4. Stop dictation with the hotkey or the stop button.
5. Wait for the bar to switch to `Transcribing`.
6. The final text is inserted into the original text target.

In toggle mode, press the hotkey once to start and again to stop. In hold mode,
hold the hotkey while speaking and release it to stop. Press Escape or the
cancel button to discard an active recording.

## Development Commands

Install dependencies:

```sh
npm install
```

Run the signed macOS developer build:

```sh
npm run dev
```

Run the raw Tauri dev command:

```sh
npm run dev:raw
```

`dev:raw` is useful for low-level Tauri debugging, but `npm run dev` is the
normal path on macOS because the stable bundle identifier and signing identity
make privacy permissions less fragile.

Run all project checks:

```sh
npm test
```

Run only Rust library tests:

```sh
npm run test:rust
```

Build a release app:

```sh
npm run build
```

Release packaging is not the first target yet. Signing, notarization, installer
polish, and release distribution still need product work before this is a
friendlier end-user install.

## How The App Is Built

Slugtale uses a thin Tauri shell around Rust domain modules:

- `src/index.html` is the settings UI.
- `src/dictation-bar.html` is the floating recording/transcribing UI.
- `src-tauri/src/main.rs` wires Tauri commands, windows, tray setup, hotkeys,
  audio capture, model paths, diagnostics, and platform adapters.
- `src-tauri/src/*.rs` contains the core app behavior in small Rust modules.
- `src-tauri/tauri.conf.json` defines the hidden settings window, transparent
  dictation bar, bundle identifier, and macOS Info.plist wiring.
- `scripts/run-dev.js` builds, signs, verifies, and opens the developer macOS
  app bundle.
- `scripts/reset-dev-permissions.js` resets macOS Accessibility grants for the
  Slugtale bundle identifier when privacy state gets stale.

The important boundaries are:

- **Settings**: local non-secret JSON preferences such as hotkey, activation
  mode, model path, launch-at-login preference, and diagnostic logging.
- **Readiness**: a single gate that requires microphone access, text insertion
  access, configured hotkey, and local model availability.
- **Audio capture**: records mono 16 kHz samples and reports a perceptual voice
  level to the dictation bar.
- **ASR runtime**: wraps the local Whisper model and caches the loaded runtime
  across transcriptions.
- **Dictation workflow**: transcribes captured audio, cleans the final text, and
  inserts it into the text target.
- **Platform adapters**: isolate macOS-specific permissions, activation, text
  insertion, notifications, and future OS-specific work.

## Project Structure

```text
src/
  index.html              Settings UI loaded by Tauri
  dictation-bar.html      Floating recording/transcribing surface

src-tauri/
  src/main.rs             Tauri entry point, commands, app setup
  src/lib.rs              Re-exported Rust domain modules
  src/*.rs                Settings, readiness, hotkey, ASR, model, audio, etc.
  tauri.conf.json         Tauri windows, bundle, and app configuration
  Info.plist              macOS privacy strings
  Cargo.toml              Rust package, dependencies, and feature flags
  icons/                  Tray/app icon source

scripts/
  run-dev.js              Signed macOS developer-run app launcher
  run-cargo.js            Cargo runner used by npm scripts
  reset-dev-permissions.js macOS Accessibility reset helper

docs/
  adr/                    Architecture decision records
  research/               Research notes

CONTEXT.md                Product vocabulary and domain definitions
SECURITY.md               Security policy and current data-handling guarantees
AGENTS.md                 Agent workflow instructions
```

## Troubleshooting

### `Missing macOS code-signing identity: Slugtale Dev`

Create the `Slugtale Dev` self-signed code-signing certificate in Keychain
Access, or run with `SLUGTALE_SIGN_IDENTITY="Your Identity" npm run dev`.

### The app opens but I do not see a window

Slugtale is a menu bar resident app. Look in the macOS menu bar and choose
`Settings...` from the Slugtale menu.

### Microphone permission stays missing

Open `System Settings > Privacy & Security > Microphone` and allow Slugtale.
Quit and rerun `npm run dev` after changing the permission.

### Text insertion or Accessibility permission stays missing

Open `System Settings > Privacy & Security > Accessibility` and allow Slugtale.
If stale entries remain from older developer builds, reset the grant:

```sh
npm run macos:reset-permissions
```

Then quit Slugtale, run `npm run dev`, grant Accessibility again, and rerun the
app. If stale rows still remain, use the broader reset:

```sh
npm run macos:reset-permissions -- --all-accessibility
```

### Dictation copies text to the clipboard instead of inserting it

Direct insertion likely failed. Make sure Slugtale has Accessibility permission
and that the original text target is still available. The clipboard rescue is
intentional: it preserves the transcription so you can paste it with Cmd+V.

### The hotkey does not start dictation

Check that the readiness checklist shows the hotkey as ready. If saving the
hotkey fails, it may conflict with a system shortcut or another app. Choose a
different combination and save again.

### Model download fails

Check your network connection and try again from the settings window. The model
is staged as a `.download` file before being moved into place, so retrying should
replace incomplete downloads.

### `cargo` is not found

Install Rust with `rustup`, restart your shell, and confirm `cargo --version`
works. If Cargo is installed in a custom location, run commands with:

```sh
CARGO=/path/to/cargo npm test
```

### `npm run build` works but the app is not ready to ship

That is expected. The current project direction is developer-run builds first.
Shipping to non-developers still needs signing, notarization, packaging, and a
release process.

## Design Notes

The `docs/adr/` directory records the design decisions behind the current
implementation. Useful starting points:

- `0001-local-only-processing.md`
- `0004-support-hold-and-toggle-hotkey-modes.md`
- `0006-whisper-first-asr.md`
- `0007-rust-tauri-desktop-shell.md`
- `0008-tray-menu-bar-resident-first.md`
- `0009-clipboard-free-insertion-first.md`
- `0010-managed-local-model-downloads.md`
- `0013-settings-access-with-gated-dictation.md`
- `0016-clipboard-rescue-on-insertion-failure.md`
- `0020-developer-run-first.md`
- `0021-macos-first-with-platform-adapters.md`

`CONTEXT.md` defines the project language used across code, docs, and issues.

## Contributing

This repository uses Beads (`bd`) for local issue tracking:

```sh
bd prime
bd ready
bd show <id>
bd update <id> --claim
bd close <id>
```

For code changes, prefer the existing module boundaries and run `npm test`
before opening a pull request or sharing a branch.

## Security

See [SECURITY.md](SECURITY.md) for supported versions, vulnerability reporting,
and the current local-only data-handling policy.

## License

Slugtale is open source under the [MIT License](LICENSE).
