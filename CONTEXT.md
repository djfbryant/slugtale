# Slugtale

Slugtale is a local-first desktop dictation product. Its core job is to turn a user's speech into text and insert that text into the application they were already using.

## Language

**Dictation**:
A recording session where the user speaks and expects resulting text to be written into a text target.
_Avoid_: Speech job, voice note

**Dictation Workflow**:
The full path from a started dictation through final transcription, transcript cleanup, immediate insertion, and insertion rescue when insertion fails.
_Avoid_: Speech pipeline, transcription flow

**Transcription**:
The text produced from captured speech before it is inserted into a text target.
_Avoid_: Output, result

**Final Transcription**:
The completed transcription produced after the user stops dictating. Slugtale's first version only needs to show or insert final transcriptions, not live partial text.
_Avoid_: Finished output, final text

**Transcript Cleanup**:
Deterministic formatting applied to a final transcription before insertion, such as trimming whitespace, normalizing spaces, or capitalizing the first character. Transcript cleanup is not rewriting.
_Avoid_: Enhancement, post-processing

**Text Target**:
The external text input, editor, or document location where dictated text should appear.
_Avoid_: Destination app, textbox

**Text Insertion**:
The act of writing a transcription into the current text target without requiring the user to switch applications.
_Avoid_: Typing, paste

**Immediate Insertion**:
Text insertion performed automatically after final transcription and cleanup, without a confirm/edit step.
_Avoid_: Auto-submit, auto-paste

**Clipboard-Free Insertion**:
Text insertion that does not place the transcription on the system clipboard. Slugtale tries clipboard-free insertion before any clipboard-based fallback.
_Avoid_: Direct typing, standard insert

**Insertion Rescue**:
The failure path where Slugtale preserves a transcription after text insertion fails. In v1, insertion rescue copies the transcription to the clipboard and notifies the user.
_Avoid_: Failed paste recovery, backup text

**Hotkey**:
A user-defined keyboard shortcut that starts or controls dictation while another application has focus.
_Avoid_: Shortcut, trigger

**Hotkey Activation Mode**:
The behavior assigned to a hotkey when controlling dictation. Slugtale supports both hold-to-dictate and toggle-to-dictate in the first version.
_Avoid_: Shortcut style, trigger mode

**Resident App**:
The background desktop application that stays available while the user works in other apps. Slugtale's first version is a tray/menu-bar resident app with a small settings surface.
_Avoid_: Main app, full app

**Developer-Run Build**:
A build run directly by a developer from the source tree rather than a signed installer or packaged release.
_Avoid_: Prototype, dev mode

**Platform Adapter**:
An implementation boundary around operating-system-specific behavior such as hotkeys, permissions, audio capture, and text insertion. Slugtale's first implementation target is macOS, but platform adapters must keep Windows support visible.
_Avoid_: OS helper, native bridge

**Launch at Login**:
The setting that starts Slugtale automatically when the user signs in. Slugtale asks about launch at login during onboarding instead of enabling it silently.
_Avoid_: Autostart, startup item

**Dictation Readiness**:
The state where Slugtale can actually start dictation. Dictation readiness requires microphone permission, text insertion permission, a configured hotkey, and a downloaded local model.
_Avoid_: Setup complete, onboarding complete

**Dictation Bar**:
A small on-screen surface shown while dictation is active. It communicates that Slugtale is recording and gives the user explicit stop and cancel controls.
_Avoid_: Overlay, live preview

**Local-Only Processing**:
The product promise that captured speech, transcriptions, and optional enhancement stay on the user's machine unless the user explicitly exports them.
_Avoid_: Privacy mode, offline mode

**Local Model**:
A speech recognition model file stored on the user's machine and used for local transcription. Slugtale may download local models during onboarding, but transcription runs from the stored model file.
_Avoid_: AI backend, remote model

**Settings File**:
The local non-secret configuration file that stores Slugtale preferences such as hotkey, activation mode, model choice, and launch-at-login preference.
_Avoid_: Database, keychain

**Local Diagnostic Log**:
A local troubleshooting log for development and support. It must not include captured audio or transcription text.
_Avoid_: Telemetry, analytics, crash reporting

**Dictation Language**:
The spoken language Slugtale expects during dictation. The first version is English-only by default.
_Avoid_: Locale, model language

**Dictation History**:
A durable record of prior dictations, potentially including transcript text, target application context, or captured audio. Slugtale does not create dictation history in the first version.
_Avoid_: Recent dictations, transcript log

**Rewrite**:
A future workflow where selected text in a text target is replaced or transformed using dictated instructions. Rewrite is not part of the first version.
_Avoid_: Edit mode, write mode

**Live Preview**:
A future workflow where partial transcription text is shown while the user is still speaking. Live preview is not part of the first version.
_Avoid_: Streaming preview, partial output

**Beam Search**:
A search strategy used by the local speech recognition model to balance accuracy against transcription speed. Beam Search with higher values (wider beam) produces more accurate results but takes longer; lower values (narrower beam) produce faster results with potential accuracy loss.
_Avoid_: Beam size, decode beam, search width

**Transcription Speed Profile**:
A user-configurable global setting stored in the Settings File that determines the accuracy/speed tradeoff for all future transcriptions. The user selects a profile once and it persists across app restarts. Each profile maps to an underlying Beam Search value. The profile applies to all dictations until the user changes the setting again. Three profiles are available: Fast (`best_of: 1`), Balanced (`best_of: 5`, default), and Accurate (`best_of: 10`). The setting appears in its own "Transcription" section of the settings UI.
_Avoid_: Quality setting, transcription mode, decode mode, per-dictation quality
