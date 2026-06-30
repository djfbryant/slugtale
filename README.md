# Slugtale

Slugtale is a local-first desktop dictation app. Its job is to capture speech, transcribe it locally, and insert the final transcription into the text target the user was already using.

The first implementation is a developer-run Tauri app for macOS. The code keeps operating-system behavior behind platform adapters so Windows support can be added later without changing the core dictation workflow.

## Current Status

Slugtale currently includes:

- A Tauri 2 desktop shell with a menu bar tray icon and settings window.
- A local settings file for non-secret preferences.
- Dictation readiness checks for microphone permission, text insertion permission, hotkey configuration, and local model availability.
- Managed download and deletion for the default local Whisper model, `base.en`.
- A Rust ASR boundary with an optional local Whisper runtime.
- Unit tests for the core settings, readiness, model, transcription, hotkey, and tray behavior.

Some product workflow pieces are still intentionally early. The repository documents the intended shape in `docs/adr/`.

## Requirements

- macOS for the first runnable slice.
- Node.js and npm.
- Rust and Cargo, usually installed with `rustup`.
- Xcode Command Line Tools on macOS.

The npm scripts look for Cargo on `PATH`, then at the standard `rustup`
location (`$HOME/.cargo/bin/cargo`). If Cargo is installed somewhere else, set
`CARGO=/path/to/cargo`.

## Install

```sh
npm install
```

## Run Locally

```sh
npm run dev
```

This starts the Tauri app with the `local-whisper-runtime` feature enabled. On macOS, Slugtale runs as an accessory app with a menu bar tray icon. Use the tray menu to open the settings window.

The settings window can show readiness state and manage the default local model. Model download uses the Whisper `ggml-base.en.bin` file from the `whisper.cpp` Hugging Face repository and stores it in the app data directory.

## Test

```sh
npm test
```

This runs the Rust library tests:

```sh
npm run test:rust
```

Use the npm scripts for project checks instead of bare `cargo ...` commands in
agent shells. Some agent environments do not include `$HOME/.cargo/bin` on
`PATH`, even when Rust is installed there.

## Build

```sh
npm run build
```

Packaging is not the first target for this project. The current direction is developer-run builds first, with signing, notarization, installer work, and release packaging deferred.

## Project Structure

```text
src/
  index.html              Static settings UI loaded by Tauri

src-tauri/
  src/main.rs             Tauri entry point, commands, app setup
  src/lib.rs              Core app types, readiness logic, model handling, ASR boundary, tests
  tauri.conf.json         Tauri app configuration
  Cargo.toml              Rust package, dependencies, and feature flags
  icons/                  Tray/app icon source

docs/
  adr/                    Architecture decision records
  research/               Research notes

CONTEXT.md                Product vocabulary and domain definitions
AGENTS.md                 Agent workflow instructions
```

## Design Notes

The key design decisions are captured as ADRs in `docs/adr/`. Useful starting points:

- `0001-local-only-processing.md`
- `0006-whisper-first-asr.md`
- `0007-rust-tauri-desktop-shell.md`
- `0008-tray-menu-bar-resident-first.md`
- `0010-managed-local-model-downloads.md`
- `0020-developer-run-first.md`
- `0021-macos-first-with-platform-adapters.md`

`CONTEXT.md` defines the project language used across the codebase and docs.

## Issue Tracking

This repository uses Beads (`bd`) for issue tracking.

```sh
bd ready
bd show <id>
bd update <id> --claim
bd close <id>
```

Run `bd prime` for the full local workflow context.

## Security

See [SECURITY.md](SECURITY.md) for supported versions, vulnerability reporting,
and the current local-only data-handling policy.

## License

Slugtale is open source under the [MIT License](LICENSE).
