# Dictation into another app

Slugtale records speech locally, shows a floating bar, and inserts the finished text into the app where the user started dictation.

## Sub-features

- `dictation-toggle`: shortcut starts/stops recording.
- `dictation-hold`: held shortcut records; release stops.
- `dictation-stop`: hotkey and bar Stop entry points.
- `dictation-cancel`: Escape and bar Cancel entry points.
- `dictation-quiet`: quiet period inserts one segment while recording continues.
- `dictation-recovery`: clipboard fallback when direct insertion fails.

## How to get to it (user POV)

- Put the cursor in another app's editable text field and press the configured shortcut.
- In Toggle mode, press again or use `Stop dictation` on the bar.
- In Hold mode, hold the shortcut while speaking, then release it.
- Press Escape or use `Cancel dictation` to discard speech not yet inserted.

## Driving it with native CUA

Preconditions: the private app reports Ready to dictate; its own local model is installed; its Microphone and Accessibility grants exist; a unique shortcut is configured; no other dictation run is active. The installed app's permissions/assets do not transfer. Use a new untitled TextEdit document as the disposable destination, not a live document, chat, or email.

- **Toggle + stop button.** Select Toggle through Settings. Bind TextEdit with `cua.getApp('com.apple.TextEdit')`; open a new document through its real File > New menu and focus its body. Capture empty text. Press the captured shortcut using native `pressKey`, provide synthetic local speech `Shopping list. Milk and bread.`, and capture `Recording` plus the visible bar. Bind the private Slugtale app, select the fresh `Stop dictation` button (`stop`), capture transcribing state and disappearance. Return to TextEdit and capture inserted words. A screenshot of a bar without destination text is incomplete.
- **Toggle + hotkey stop.** Repeat in a fresh scratch document; stop with the same shortcut while the destination has focus. Require the same resulting text path. Treat this as a separate entry point.
- **Cancel.** Start a short phrase and cancel before the quiet timer with Escape. Require no text change. Repeat with the bar's `Cancel dictation` (`cancel`) button. Capture both paths separately.
- **Quiet insertion.** Set `Insert after quiet` to `2 s` before starting. Speak one phrase, wait beyond two seconds, and capture its insertion while the bar still records. Speak a second phrase and stop. Require both segments once, in order. Cancel after one insertion and confirm the already inserted text remains.
- **Hold.** Select Hold and test actual modifier/key-down followed by release through a harness that can hold keys. The currently documented CUA `pressKey` is an atomic press and has no split key-down/up API; report Hold blocked if no such native facility is available. An atomic press or Toggle proof does not prove Hold.
- **Recovery.** In a dedicated private permission-denied run with working audio/model, stop a phrase and capture the fallback notification. Paste into a scratch TextEdit document through normal UI and require the recovered text. Record and restore the original clipboard through the harness if supported. Do not change the live app's Accessibility grant to create this condition.

## Gotchas

- CUA has no documented audio-injection interface. If synthetic speech cannot be supplied through a real local microphone/loopback, report audio paths blocked and ask for the missing input. ASR example binaries cannot substitute for the microphone-to-destination path.
- Speech and transcripts must never leave this device. Do not use cloud speech generation/transcription or upload proof containing private text.
- Cancel does not undo text already inserted by a quiet-period flush.
- A settings-only run intentionally has no permissions, model, or shortcut. Report those preconditions clearly.
- Close only the scratch TextEdit document without saving; never quit the user's TextEdit process or discard another document.
