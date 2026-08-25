# Slugtale

Slugtale is a local-first desktop dictation product. Its core job is to turn a user's speech into text and insert that text into the application they were already using.

## Language

**Dictation**:
A recording session where the user speaks and expects resulting text to be written into a text target.
_Avoid_: Speech job, voice note

**Dictation Workflow**:
The full path from a started dictation through final transcription, transcript cleanup, immediate insertion, and insertion rescue when insertion fails. It runs once per Dictation Segment, so a single dictation may run it several times.
_Avoid_: Speech pipeline, transcription flow

**Transcription**:
The text produced from captured speech before it is inserted into a text target.
_Avoid_: Output, result

**Final Transcription**:
The completed transcription of one Dictation Segment. A dictation produces one per Segment Pause plus one when the user stops, so a long dictation yields several. Every one of them is final: Slugtale still never shows or inserts live partial text.
_Avoid_: Finished output, final text

**Dictation Segment**:
A span of a dictation's speech that Slugtale transcribes and inserts on its own. A Segment Pause ends one and the next begins immediately, so the microphone never stops. A dictation containing no pause is a single segment inserted when the user stops, which is exactly the original one-insertion behaviour.
_Avoid_: Chunk, part, utterance, block

**Segment Pause**:
Roughly five seconds during which the user stays at or below the voice level the Dictation Bar treats as speech. It ends the current Dictation Segment while recording carries on. The length is fixed, and the pause only counts once the user has actually said something, so a dictation that opens with silence never flushes an empty segment.
_Avoid_: Silence timeout, VAD gap, endpointing

**Pause Flush**:
Transcribing and inserting a Dictation Segment while the dictation is still running. Each pause flush is an ordinary Immediate Insertion at the caret, and segments are inserted in the order they were spoken however long each one takes to decode.
_Avoid_: Partial insert, streaming insert, live insert

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
An implementation boundary around operating-system-specific behavior such as hotkeys, permissions, audio capture, and text insertion. Slugtale's first implementation target is macOS, but platform adapters must keep Windows and Linux support visible.
_Avoid_: OS helper, native bridge

**Display Server Session**:
The kind of graphical session a Linux user is signed into — X11 or Wayland. The session kind determines whether Slugtale can grab hotkeys and synthesize text insertion directly, so Slugtale must detect it and tell the user when a session kind is not yet supported.
_Avoid_: Window system, graphics mode

**Launch at Login**:
The setting that starts Slugtale automatically when the user signs in. Slugtale asks about launch at login during onboarding instead of enabling it silently.
_Avoid_: Autostart, startup item

**Dictation Readiness**:
The state where Slugtale can actually start dictation. Dictation readiness requires microphone permission, text insertion permission, a configured hotkey, a downloaded local model, and a transcription engine that can actually run — a downloaded model is not enough on its own, because a build compiled without that engine's runtime has the weights on disk and nothing able to decode them.
_Avoid_: Setup complete, onboarding complete

**Dictation Runtime**:
The module that coordinates the Dictation Workflow's execution: it owns the segment channel, the worker that keeps Final Transcriptions inserting in spoken order, the Pause Flush trigger, and Usage counting for Counted Segments. It reaches the rest of the app through one small adapter (settings, Dictation Bar control and feedback, focus target, usage file). Microphone ownership, window management, and file writes stay outside it.
_Avoid_: Dictation service, dictation manager, pipeline runner

**Dictation Bar**:
A small on-screen surface shown while dictation is active. It communicates that Slugtale is recording and gives the user explicit stop and cancel controls. It rests as the **Orb** and grows into the **Pill** to show those controls when the user reaches for them (slugtale-z7a).
_Avoid_: Overlay, live preview

**Orb**:
The Dictation Bar at rest: a small circle carrying the mic glyph in the user's chosen Accent Colour, with a halo that reads the voice level. It is the whole of the app's presence during a normal dictation.
_Avoid_: Blob, dot, bubble

**Pill**:
The Dictation Bar expanded: the Orb plus the state label, elapsed clock, and the stop and cancel controls. Shown while the pointer is on the bar, and for the whole transcribing phase.
_Avoid_: Panel, toolbar, expanded bar

**Accent Colour**:
The user's chosen colour from a fixed six-swatch palette, painted by the Orb. Once the resting state carries no label, the colour is the app's identity in use.
_Avoid_: Theme, brand colour, highlight

**Local-Only Processing**:
The product promise that captured speech, transcriptions, and optional enhancement stay on the user's machine unless the user explicitly exports them.
_Avoid_: Privacy mode, offline mode

**Local Model**:
A speech recognition model file stored on the user's machine and used for local transcription. Slugtale may download local models during onboarding, but transcription runs from the stored model file.
_Avoid_: AI backend, remote model

**Settings File**:
The local non-secret configuration file that stores Slugtale preferences such as hotkey, activation mode, model choice, launch-at-login, and Typing Baseline.
_Avoid_: Database, keychain

**Usage File**:
The optional local file of Daily Usage Records, written only when the user chooses to store them. It is not the Settings File.
_Avoid_: Stats store, usage database, telemetry file

**Local Diagnostic Log**:
A local troubleshooting log for development and support. It must not include captured audio or transcription text.
_Avoid_: Telemetry, analytics, crash reporting

**Dictation Language**:
The spoken language Slugtale expects during dictation. The first version is English-only by default.
_Avoid_: Locale, model language

**Dictation History**:
A durable record of prior dictations, potentially including transcript text, target application context, or captured audio. Slugtale does not create dictation history in the first version. Daily Usage Records are not dictation history: they contain no transcription, audio, or text target.
_Avoid_: Recent dictations, transcript log

**Usage**:
The Settings window section that shows counted dictation totals and Time Saved. It is not Dictation History and not telemetry.
_Avoid_: Statistics, analytics, dashboard

**Daily Usage Record**:
One local day's counted dictation totals: dictation count, word count, and speaking duration, with no transcription text.
_Avoid_: History entry, session log, day bucket

**Counted Segment**:
A Dictation Segment that was inserted or rescued, and whose words and speaking duration therefore belong to Usage.
_Avoid_: Successful dictation, completed job

**Typing Baseline**:
The user's typing speed in whitespace words per minute, taken from three Typing Challenges or from a typed estimate until those challenges are finished.
_Avoid_: WPM setting, typing speed, perceived WPM

**Typing Challenge**:
A thirty-second English prose typing run used to measure the Typing Baseline. Three completed challenges produce the measured baseline.
_Avoid_: Typing test, WPM quiz

**Time Saved**:
An estimate of how much typing time counted dictation avoided, using the current Typing Baseline minus speaking duration. It is computed from Daily Usage Records, never stored as its own total.
_Avoid_: Hours saved, productivity score

**Rewrite**:
A future workflow where selected text in a text target is replaced or transformed using dictated instructions. Rewrite is not part of the first version.
_Avoid_: Edit mode, write mode

**Live Preview**:
A future workflow where partial transcription text is shown while the user is still speaking. Live preview is not part of the first version. A Pause Flush is not live preview: it inserts completed text for speech the user has finished saying, never a partial guess at speech in progress.
_Avoid_: Streaming preview, partial output

**Beam Search**:
A search strategy used by the local speech recognition model to balance accuracy against transcription speed. Beam Search with higher values (wider beam) produces more accurate results but takes longer; lower values (narrower beam) produce faster results with potential accuracy loss.
_Avoid_: Beam size, decode beam, search width

**Transcription Speed Profile**:
A user-configurable global setting stored in the Settings File that determines the accuracy/speed tradeoff for all future transcriptions. The user selects a profile once and it persists across app restarts. Each profile maps to an underlying decode strategy. The profile applies to all dictations until the user changes the setting again. Three profiles are available: Fast (greedy decoding, no Beam Search), Balanced (Beam Search value 2, default), and Accurate (Beam Search value 5). The values were chosen from measured latency and accuracy on real speech clips (docs/research/whisper-decode-benchmark.md). The setting appears in its own "Transcription" section of the settings UI.
_Avoid_: Quality setting, transcription mode, decode mode, per-dictation quality

**Transcription Engine**:
A local speech recognition implementation Slugtale can ask for a final transcription. Every transcription engine runs entirely on the user's device; there is no cloud engine and no remote fallback. Slugtale knows a closed set of them — Whisper, Parakeet, and Apple SpeechTranscriber — because each carries its own licence, attribution, and platform constraints the settings surface has to state accurately.
_Avoid_: Backend, recognizer, ASR model

**Second Opinion**:
Running a second transcription engine on the same recording when the first engine's result looks uncertain or anomalous, then inserting exactly one of the two complete transcripts. Slugtale never merges words from two engines. Second opinion is off by default and, when on, does nothing at all on a healthy dictation.
_Avoid_: Ensemble, voting, fallback model

**Escalation**:
The decision to ask for a second opinion, taken by fixed, inspectable rules rather than a learned model. Every escalation produces a reason code that names the shape of the result — empty, looping, implausibly short for the recording, or below the engine's own confidence threshold — and never its content.
_Avoid_: Retry, rerun, fallback trigger

**Engine Availability**:
Whether a transcription engine can actually run on this machine and this build right now. Unavailability is always explained: the wrong operating system, an operating system too old, an unsupported dictation language, assets the user has not installed, or a build compiled without that engine. Only missing assets are something the user can fix from settings.
_Avoid_: Engine status, model state
