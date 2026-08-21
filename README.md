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
- Long dictations insert as you speak: a five-second pause inserts the speech so
  far and keeps recording, appending later speech after it.
- Local English transcription through `whisper-rs` when built with the
  `local-whisper-runtime` feature; signed macOS developer builds also enable
  `local-whisper-runtime-metal`.
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

The Usage section of Settings can count your dictations, words, and speaking
time, and work out roughly how much typing time that saved. It is off until you
turn it on, the counts never leave the machine, and turning it off deletes them.
No transcript or audio is ever part of a count (ADR-0025).

## Requirements

The runnable slice is macOS-first. The code keeps operating-system behavior
behind platform adapters (ADR-0021), and the Windows and Linux adapters are
written: text insertion, permission readiness, focus targeting, and audible
feedback all have implementations on each platform.

macOS is the only platform verified end to end on real hardware. Every push
builds and unit-tests the library target on Ubuntu, Windows, and macOS with
default features, so the engine runtimes behind the opt-in Cargo features are
not compiled by that check; Windows additionally builds the Whisper runtime.
The Windows adapter's runtime behaviour — hold-to-dictate key-up, WASAPI
capture, and insertion into a live app — has not been exercised on a Windows
machine at all. Treat Windows as buildable and unvalidated. See PRD
`slugtale-5pc` for what remains.

For macOS development you need:

- macOS with Xcode Command Line Tools.
- Node.js and npm.
- Rust and Cargo, usually installed with `rustup`.
- A local macOS code-signing identity for the developer-run app bundle.

The npm scripts look for Cargo on `PATH`, then at `$HOME/.cargo/bin/cargo`. If
Cargo is somewhere else, set `CARGO=/path/to/cargo`.

## Build And Run As A Local macOS App

These steps build Slugtale from source, install it as
`/Applications/Slugtale.app`, and run it like a normal local app. The result is
signed with a certificate created on your Mac; it is not a notarized build for
distribution to other people.

### 1. Install the build tools

Install the Xcode Command Line Tools:

```sh
xcode-select --install
```

Install a current Node.js release from [nodejs.org](https://nodejs.org/) if
`node --version` or `npm --version` does not work.

Install Rust if `cargo --version` does not work:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Restart the terminal after installing Rust, then confirm the tools are
available:

```sh
node --version
npm --version
cargo --version
```

### 2. Download the source and dependencies

```sh
git clone https://github.com/djfbryant/slugtale.git
cd slugtale
npm install
```

### 3. Create a local signing identity

The build scripts use a stable signing identity so macOS can retain Slugtale's
privacy permissions between rebuilds:

1. Open Keychain Access.
2. From the Keychain Access menu, choose
   `Certificate Assistant > Create a Certificate`.
3. Enter `Slugtale Dev` as the certificate name.
4. Set `Identity Type` to `Self Signed Root`.
5. Set `Certificate Type` to `Code Signing`.
6. Create the certificate in the login keychain.

If you already have a code-signing identity, you can use its exact name instead
by setting `SLUGTALE_SIGN_IDENTITY` when running the build command.

### 4. Build, install, and open Slugtale

From the repository directory, run:

```sh
npm run macos:install
```

This command:

1. Builds an optimized release app with Metal-accelerated local Whisper.
2. Signs and verifies the app with the `Slugtale Dev` identity.
3. Replaces any existing `/Applications/Slugtale.app`.
4. Opens the installed app.

Slugtale is a menu bar app, so it does not open a normal Dock window. Look for
the Slugtale icon in the macOS menu bar and choose `Settings...`.

### 5. Grant permissions and finish setup

Open `System Settings > Privacy & Security` and grant the installed Slugtale
app:

1. Microphone access.
2. Accessibility access, which Slugtale uses to insert dictated text.

Then open Slugtale settings:

1. Download the local `base.en` model.
2. Choose a hotkey and activation mode.

The app is ready when every item in the readiness checklist is marked ready.

If the installed app does not appear under Microphone, or macOS will not ask
again after access was denied, reset only Slugtale's grants and relaunch the
installed app in permission-recovery mode:

```sh
npm run macos:reauthorize
```

Accept the fresh Microphone prompt, then use `Open Accessibility` in Slugtale
settings and enable the installed app. This command intentionally clears both
existing grants for `com.slugtale.desktop`; normal builds and reinstalls do not.

### Rebuild after making changes

Run the same command after pulling or making source changes:

```sh
npm run macos:install
```

The installer quits the running copy, replaces it, verifies it, and opens the
new build. Its stable bundle identifier and signing identity preserve the
installed app's settings, model, and privacy permissions across reinstalls.

For faster development without installing into `/Applications`, run:

```sh
npm run dev
```

That command builds, signs, verifies, and opens a debug app inside the
repository. macOS treats this developer-run app and
`/Applications/Slugtale.app` as separate privacy subjects, so each needs its
own Microphone and Accessibility grants.

To use a different signing identity or install location:

```sh
SLUGTALE_SIGN_IDENTITY="Your Code Signing Identity" \
SLUGTALE_INSTALL_DIR="$HOME/Applications" npm run macos:install
```

To build and sign the release bundle without installing it:

```sh
npm run macos:install -- --build-only
```

Because the default certificate is self-signed rather than notarized by Apple,
the resulting build is trusted only on the Mac that created it. See ADR-0022
for the planned distribution story.

### Building the optional Transcription Engines

Both `npm run dev` and `npm run macos:install` compile Whisper only. Parakeet
and Apple SpeechTranscriber each need a native toolchain that an everyday build
should not pay for — ONNX Runtime and the Swift compiler respectively — so they
are opt-in. A build without them shows the engine in settings as `Unavailable`
with the reason `this build was compiled without support for this engine`.

Name the ones you want in `SLUGTALE_ENGINE_FEATURES`; Whisper is always
included:

```sh
SLUGTALE_ENGINE_FEATURES=apple-speech-runtime,local-parakeet-runtime \
  npm run macos:install
```

| Feature | Engine | Requires |
| --- | --- | --- |
| `apple-speech-runtime` | Apple SpeechTranscriber | macOS 26 or later |
| `local-parakeet-runtime` | Parakeet TDT v2 on CPU | Model assets, installed from settings |
| `local-parakeet-runtime-coreml` | Parakeet TDT v2 on the Neural Engine or GPU | macOS, plus the same assets |

Each launcher prints the feature list it is building with before it starts.

Apple SpeechTranscriber installs its speech assets per application, so the
`/Applications` copy asks for its own download even if the `npm run dev` build
already has them. The install takes well under a second.

`apple-speech-runtime` also raises the real floor of the resulting binary above
what the bundle advertises. The Swift bridge takes a hard dependency on the
operating system's own `libswift_Concurrency.dylib` rather than bundling the
back-deployment copy, so the app will not launch on a Mac whose `/usr/lib/swift`
predates that library — macOS 11 and earlier — even though the Mach-O header
still says `minos 11.0`. This costs nothing in practice, since the engine needs
macOS 26 regardless, but a build with the feature on is not the build to hand to
someone on an old Mac. Builds without it are unaffected.

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

### Transcript cleanup

Transcript cleanup happens on your device before Slugtale inserts plain text.
It does not send audio or words to a service, and it does not save a dictation
history.

- **Basic** keeps the usual spacing and first-letter cleanup.
- **Clean dictation** also removes safe hesitation words such as "um", "uh",
  and "erm". It leaves words such as "like" alone because they can change the
  meaning.
- **Pause breaks** includes clean dictation. It can put short phrases on new
  lines when you leave a clear pause between them. Continuous prose and normal
  completed sentences stay on one line. It needs timing from the selected
  transcription engine. If that engine has no timing, Slugtale still performs
  clean dictation and does not add line breaks.

For example, say "shopping list", pause, then say "milk and bread". Pause
breaks can insert this:

```text
Shopping list
milk and bread
```

Say a normal sentence without the long pause and it stays on one line.

Choose the mode in Settings, under Transcription. Pick Basic to turn the extra
cleanup off. Each change applies to your next Dictation.

1. Click into a text field, editor, or document in another app.
2. Press the configured hotkey.
3. Speak while the dictation bar is visible.
4. Stop dictation with the hotkey or the stop button.
5. Wait for the bar to switch to `Transcribing`.
6. The final text is inserted into the original text target.

In toggle mode, press the hotkey once to start and again to stop. In hold mode,
hold the hotkey while speaking and release it to stop. Press Escape or the
cancel button to discard an active recording.

### Long dictations insert as you go

You do not have to stop to see your words. If you go quiet for about five
seconds, Slugtale transcribes what you have said so far and inserts it while the
microphone stays on. Carry on speaking and the next stretch is appended after
it. Stopping inserts whatever is left. A dictation with no five-second gap in it
behaves exactly as before: one insertion, when you stop.

Three things are worth knowing:

- Each insertion types into the app you started dictating into, at wherever its
  cursor is at that moment. If you click somewhere else mid-dictation, the next
  stretch lands at the new spot — Slugtale does not track the cursor between
  insertions.
- Escape stops the dictation and throws away anything not yet inserted. It does
  not remove text that has already landed; use your app's own undo for that.
- The five seconds is fixed for now, and the pause is measured the same way the
  bar's waveform reacts, so if the bar reads you as still talking then the pause
  has not started yet.

## Development Commands

Install dependencies:

```sh
npm install
```

Run the signed macOS developer build:

```sh
npm run dev
```

On macOS this enables Whisper Metal acceleration through the
`local-whisper-runtime-metal` Cargo feature. CPU fallback builds can still use
only `local-whisper-runtime`. whisper-rs also offers a `coreml` feature, but it
was evaluated and deferred: it requires shipping a separately converted CoreML
encoder (`.mlmodelc`) alongside the ggml model and only accelerates the
encoder, which Metal already offloads. Decode strategy and thread settings were
chosen from measurements — see `docs/research/whisper-decode-benchmark.md`.

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

For a macOS release build with local Whisper acceleration enabled, run the Tauri
build with both runtime features:

```sh
node scripts/run-tauri.js build --features local-whisper-runtime,local-whisper-runtime-metal --ci
```

`npm run macos:install` wraps that command and also signs and installs the
result — see
[Build And Run As A Local macOS App](#build-and-run-as-a-local-macos-app).

Release builds compile whisper.cpp from source, which needs an explicit macOS
deployment target because `ggml` uses `std::filesystem`. That target is set once
in `src-tauri/tauri.conf.json` as `bundle.macOS.minimumSystemVersion`; the build
launchers pass the same value to CMake. Lowering it below `10.15` breaks the
build. On Apple Silicon the Rust target raises the real floor to macOS 11.0
regardless of that setting.

Signing here means a local self-signed identity. Apple notarization, installer
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
The developer-run build from `npm run dev` and the installed build at
`/Applications/Slugtale.app` have separate grants, so make sure you enable the
copy you are currently running. Quit and reopen that copy after changing the
permission.

If the installed app is missing from the Microphone list or macOS will not
prompt again, run:

```sh
npm run macos:reauthorize
```

This quits the installed app, resets its Microphone and Accessibility grants,
and relaunches it to produce a fresh Microphone prompt.

### Text insertion or Accessibility permission stays missing

Open `System Settings > Privacy & Security > Accessibility` and allow Slugtale.
The developer-run and installed builds have separate grants, so make sure you
enable the copy you are currently running.
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
