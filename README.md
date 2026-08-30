# Slugtale

Slugtale is a desktop dictation app. It records and transcribes speech on your
computer, cleans the text, and inserts it into the app that you are using.

Slugtale is in active development. macOS is the only platform tested from start
to finish on real hardware. The current macOS build uses a local developer
certificate and is not notarized for public distribution.

## Your dictation stays on your device

Slugtale processes speech recognition, transcript cleanup, and the "Hi
Slugtale" wake phrase on your computer. It never sends audio, transcripts,
prompts, confidence data, vocabulary, or context to the internet. There is no
cloud transcription, cloud cleanup, or cloud fallback.

The whole app is not offline. Slugtale downloads model assets when you ask it
to. It checks GitHub Releases only after you select **Check now** in Settings.
The check does not download or install an app update. These services can see
normal connection data, such as your IP address. Slugtale does not include
dictation content in these requests.

Usage counts are off by default. If you turn them on, Slugtale stores only
counts such as words, speaking time, and dictations. Turning the setting off
deletes the stored counts. Audio and transcript text are not part of a count.

Diagnostic logs stay local and do not contain transcript text. Slugtale does
not keep a dictation history or send telemetry.

Voice activation is also off by default. If you turn it on, Slugtale keeps the
microphone open and checks for "Hi Slugtale" on your Mac.

### Voice activation model

The current experimental build uses the local Whisper model for the "Hi
Slugtale" check. This stays on your Mac, but it uses too much CPU for an
always-on feature.

The planned production version uses a small wake-word model instead. You will
install it from **Settings > Voice Activation > Install wake-word model**.
Slugtale will download fixed model files only after you select that action. It
will check each file before use. After installation, wake-word detection will
work without an internet connection.

The wake-word model contains no audio from you. Slugtale will not send your
microphone audio to the model host. The install request contains no audio,
transcript, prompt, vocabulary, or app context.

The install button and the small model are not implemented yet. Voice
activation remains experimental until the model passes local privacy, false
activation, CPU, and memory tests. See
[ADR-0027](docs/adr/0027-dedicated-openwakeword-detector-for-voice-activation.md)
for the accepted design.

See [SECURITY.md](SECURITY.md) for the full data-handling policy.

## Features

- Local English transcription with Whisper `base.en`. macOS builds use Metal
  acceleration.
- Optional local Parakeet and Apple SpeechTranscriber engines.
- An optional second local engine when the first result looks uncertain.
- A configurable hotkey with toggle and hold modes.
- Experimental voice activation on macOS. Say "Hi Slugtale" to start
  dictation without the hotkey.
- A floating dictation bar with audio level, recording, transcribing, stop, and
  cancel states.
- Bar position, display, and accent colour settings.
- Direct text insertion on macOS, with a clipboard copy if insertion fails.
- Local transcript cleanup that can remove safe hesitation words and use pauses
  to add line breaks.
- Automatic insertion after a silence period during a long dictation. The
  default is five seconds, and recording continues after the insertion.
- Optional local usage counts and typing-time estimates. This setting is off by
  default.
- Launch at login, local diagnostic logs, and manual app update checks.

## Platform status

macOS is the primary platform and the only one tested from start to finish on
real hardware.

Windows and Linux X11 adapters exist for permissions, focus, text insertion,
clipboard recovery, notifications, and sound. CI builds and tests the app on
Ubuntu, Windows, and macOS. Windows and Linux still need tests on real machines,
so do not treat them as supported releases. Linux Wayland support is not
complete. Experimental voice activation also needs a full test with a real
microphone.

## Build and run on macOS

You need:

- macOS with the Xcode Command Line Tools.
- Node.js and npm.
- Rust and Cargo.
- A local code-signing certificate.

Install the Xcode Command Line Tools:

```sh
xcode-select --install
```

Install Rust from [rustup.rs](https://rustup.rs/) if `cargo --version` does not
work.

Clone the repository and install the JavaScript dependencies:

```sh
git clone https://github.com/djfbryant/slugtale.git
cd slugtale
npm install
```

### Create the local certificate

The build scripts use a stable certificate so macOS can keep Slugtale's privacy
permissions after a rebuild.

1. Open Keychain Access.
2. Select **Keychain Access > Certificate Assistant > Create a Certificate**.
3. Enter `Slugtale Dev` as the name.
4. Set **Identity Type** to **Self Signed Root**.
5. Set **Certificate Type** to **Code Signing**.
6. Create the certificate in the login keychain.

To use another certificate, set `SLUGTALE_SIGN_IDENTITY` to its exact name.

### Install the app

Run:

```sh
npm run macos:install
```

This command builds, signs, verifies, installs, and opens
`/Applications/Slugtale.app`. It replaces an older copy at the same path.
Reusing the same bundle identifier and signing certificate keeps the app's
settings and macOS privacy grants.

Slugtale does not open a Dock window. Select its icon in the macOS menu bar,
then select **Settings...**.

### Finish the first setup

1. Grant Slugtale access under **System Settings > Privacy & Security >
   Microphone**.
2. Grant Slugtale access under **System Settings > Privacy & Security >
   Accessibility**.
3. Open Slugtale Settings and download the local `base.en` model.
4. Choose a hotkey and an activation mode.

The Status pane shows when Slugtale is ready.

If macOS does not show a new permission request, reset the installed app's
Microphone and Accessibility grants:

```sh
npm run macos:reauthorize
```

### Run a development build

Run:

```sh
npm run dev
```

This command builds, signs, verifies, and opens a debug app inside the
repository. macOS treats the debug app and `/Applications/Slugtale.app` as two
different apps. Grant Microphone and Accessibility access to each copy that you
use.

To build the release app without installing it, run:

```sh
npm run macos:install -- --build-only
```

## Use dictation

1. Put the cursor in a text field in another app.
2. Press the configured hotkey.
3. Speak while the dictation bar is visible.
4. Stop dictation with the hotkey or the stop button.
5. Wait while Slugtale transcribes and inserts the text.

In toggle mode, press the hotkey once to start and again to stop. In hold mode,
hold the hotkey while you speak and release it to stop. Press Escape or select
the cancel button to discard speech that Slugtale has not inserted.

During a long dictation, the silence timer ends the current part. The default is
five seconds. Slugtale transcribes and inserts that part while the microphone
stays on. New speech starts the next part. Escape does not remove text that
Slugtale already inserted.

## Choose transcript cleanup

Select a cleanup mode under **Settings > Dictation > Transcript cleanup**:

- **Basic** fixes spacing and the first letter.
- **Clean dictation** also removes safe hesitation words such as "um", "uh",
  and "erm". It keeps words such as "like" because removing them can change the
  meaning.
- **Pause breaks** includes Clean dictation. It can put short phrases on new
  lines after a clear pause. Normal sentences stay on one line.

For example, say "shopping list", pause, then say "milk and bread". Pause
breaks can insert:

```text
Shopping list
milk and bread
```

Pause breaks need timing data from the selected transcription engine. If the
engine has no timing data, Slugtale still removes the safe hesitation words but
does not add line breaks. Select **Basic** to turn the extra cleanup off.

## Add optional transcription engines

The `dev`, `build`, and `macos:install` scripts include Whisper. Parakeet and
Apple SpeechTranscriber are optional because they add native build tools and
model assets.

Set `SLUGTALE_ENGINE_FEATURES` to the extra Cargo features before you run a
launcher:

```sh
SLUGTALE_ENGINE_FEATURES=apple-speech-runtime,local-parakeet-runtime \
  npm run macos:install
```

| Cargo feature | Engine | Requirement |
| --- | --- | --- |
| `apple-speech-runtime` | Apple SpeechTranscriber | macOS 26 or later |
| `local-parakeet-runtime` | Parakeet TDT v2 on CPU | Model assets installed from Settings |
| `local-parakeet-runtime-coreml` | Parakeet TDT v2 with Core ML | macOS and the same model assets |

Settings shows an unavailable engine when the build does not include its Cargo
feature. The second-opinion setting needs two available local engines.

## Development

The main commands are:

```sh
npm run dev                 # Run the signed development app
npm run build               # Build a release app
npm test                    # Run frontend and Rust tests
npm run test:rust           # Run Rust library tests
npm run test:whisper-build  # Compile the Whisper runtime
```

The npm scripts find Cargo on `PATH`, then at `$HOME/.cargo/bin/cargo`. If Cargo
is elsewhere, set `CARGO` to its full path.

Run `npm test` before you open a pull request or share a branch.

## Troubleshooting

### The signing certificate is missing

Create the `Slugtale Dev` certificate as described above. To use another
certificate, run:

```sh
SLUGTALE_SIGN_IDENTITY="Your Code Signing Identity" npm run dev
```

### The app opens without a window

Select the Slugtale icon in the macOS menu bar, then select **Settings...**.

### Update a source build or fork

**Check now** can open the Slugtale release page. Slugtale does not replace the
installed app. Another build can have a different signing identity. macOS can
then ask for Microphone and Accessibility access again.

For a source build or fork, pull the new source. Install it with the same
`SLUGTALE_SIGN_IDENTITY` that you used for the current app:

```sh
SLUGTALE_SIGN_IDENTITY="Your Code Signing Identity" npm run macos:install
```

Keep that identity the same for each install. This helps macOS keep the current
privacy grants.

### Dictation copies text instead of inserting it

Grant Accessibility access to the copy of Slugtale that is running. Slugtale
copies the transcript when direct insertion fails so you can paste it yourself.

### The hotkey does not start dictation

Open the Status pane and fix each required item. If another app uses the same
shortcut, record a different hotkey.

### The model download fails

Check the network connection and start the download again from Settings.
Slugtale replaces an incomplete download when you retry.

## License

Slugtale is available under the [MIT License](LICENSE).
