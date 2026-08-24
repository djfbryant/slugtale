#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri_plugin_updater::UpdaterExt;

mod voice_activation;

use slugtale_lib::AppFiles;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
const DICTATION_ESCAPE_KEY: &str = "Escape";

/// The Typing Challenge window's label. It is created on demand rather than
/// declared in tauri.conf.json: most users never open it, and a hidden window
/// carrying a live webview for the life of the app is a cost with no benefit.
const TYPING_CHALLENGE_WINDOW: &str = "typing-challenge";

#[derive(Default)]
struct RecordingFeedbackState(Mutex<slugtale_lib::RecordingFeedback>);

/// The process id of the app the user was dictating into, captured when recording
/// starts so insertion can re-target it after transcription (slugtale-squ).
#[derive(Default)]
struct FocusTargetState(Mutex<Option<i32>>);

/// What the Dictation Bar is currently doing, sent to its frontend so it can show
/// the matching state. The bar stays on screen through transcription (slugtale-0t4).
#[derive(Clone, Copy)]
enum DictationPhase {
    Recording,
    Transcribing,
}

impl DictationPhase {
    fn as_str(self) -> &'static str {
        match self {
            DictationPhase::Recording => "recording",
            DictationPhase::Transcribing => "transcribing",
        }
    }
}

struct AudioCaptureState(Mutex<slugtale_lib::AudioCaptureSession<slugtale_lib::CpalAudioRecorder>>);

impl Default for AudioCaptureState {
    fn default() -> Self {
        Self(Mutex::new(slugtale_lib::AudioCaptureSession::new(
            slugtale_lib::CpalAudioRecorder::new(),
        )))
    }
}

/// Tauri transport for the ordered Dictation Segment module. The module owns
/// the execution policy; this state only owns the channel that crosses into
/// the worker thread.
#[derive(Default)]
struct DictationSegments {
    jobs: Mutex<Option<std::sync::mpsc::Sender<slugtale_lib::DictationSegmentJob>>>,
    control: slugtale_lib::DictationSegmentControl,
}

impl DictationSegments {
    fn current(&self) -> u64 {
        self.control.current()
    }

    /// Open a new dictation and return its number.
    fn begin(&self) -> u64 {
        self.control.begin()
    }

    /// Abandon the active dictation's un-inserted remainder.
    fn abandon(&self) {
        self.control.abandon();
    }

    fn pause_flushes_suspended(&self) -> bool {
        self.control.pause_flushes_suspended()
    }

    fn control(&self) -> &slugtale_lib::DictationSegmentControl {
        &self.control
    }

    /// Queue a job, reporting whether the worker accepted it.
    fn send(&self, job: slugtale_lib::DictationSegmentJob) -> bool {
        self.jobs
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|sender| sender.send(job).is_ok()))
            .unwrap_or(false)
    }
}

/// One Counted Segment on its way to the Usage File (ADR-0025).
///
/// The local date rides along rather than being resolved by the writer, because
/// a Counted Segment belongs to the date it landed on — and by the time a
/// backed-up queue is drained, midnight may have passed.
struct UsageUpdate {
    date: slugtale_lib::LocalDate,
    segment: slugtale_lib::CountedSegment,
}

/// Whether the Typing Challenge window is on screen.
///
/// A flag rather than asking the window itself, because the only reader is the
/// global key worker and that runs on every hotkey press. Querying window
/// visibility from a background thread costs a round trip to the main thread,
/// and the hotkey path is the one place in this app where latency is felt.
#[derive(Default)]
struct TypingChallengeOpen(std::sync::atomic::AtomicBool);

impl TypingChallengeOpen {
    fn set(&self, open: bool) {
        self.0.store(open, std::sync::atomic::Ordering::SeqCst);
    }

    fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The queue that carries Counted Segments to the Usage File.
///
/// Usage must never slow or fail Dictation (ADR-0025), so the dictation path
/// only ever does a non-blocking channel send. Reading the Settings File,
/// reading the Usage File, and writing it back all happen on the writer thread,
/// where being slow costs nothing and failing costs only a count.
#[derive(Default)]
struct UsageRecorder(Mutex<Option<std::sync::mpsc::Sender<UsageUpdate>>>);

impl UsageRecorder {
    /// Hand a Counted Segment to the writer without waiting for it. A closed or
    /// unstarted queue is dropped on the floor: the insertion already happened,
    /// which is the part that mattered.
    fn record(&self, update: UsageUpdate) {
        if let Ok(guard) = self.0.lock() {
            if let Some(sender) = guard.as_ref() {
                let _ = sender.send(update);
            }
        }
    }
}

use slugtale_lib::{
    DiagnosticInsertionRescue, DiagnosticTextInsertion, FileDiagnosticSink, SharedDiagnosticLog,
    TranscriptionProvider,
};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistrationState(Mutex<HotkeyRegistration>);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistration {
    current_hotkey: Option<String>,
    control: slugtale_lib::DictationControl,
    key_commands: Option<std::sync::mpsc::Sender<GlobalKeyCommand>>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Clone, Copy)]
enum GlobalKeyCommand {
    Input(slugtale_lib::DictationKey, slugtale_lib::HotkeyInput),
    SyncEscape(bool),
}

#[tauri::command]
fn show_settings(app: tauri::AppHandle) {
    slugtale_lib::show_settings(app);
}

/// Drive the recording surface (ADR-0014) from a dictation lifecycle event:
/// play the start/stop sound and show or hide the Dictation Bar. The bar's Stop
/// and Cancel controls route here; the global hotkey lifecycle routes the
/// configured activation hotkey and Escape here while preserving text-target
/// focus.
#[tauri::command]
fn dictation_event(app: tauri::AppHandle, event: String) -> Result<(), String> {
    let event = match event.as_str() {
        "start" => slugtale_lib::DictationEvent::Start,
        "stop" => return stop_active_dictation(&app),
        "cancel" => return cancel_active_dictation(&app),
        other => return Err(format!("unknown dictation event: {other}")),
    };

    handle_dictation_event(&app, event)
}

/// Stop from the Dictation Bar and reset the shared control at the same time.
/// Voice Activation can then trigger again without a stale active state.
fn stop_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.stop(),
        slugtale_lib::DictationEvent::Stop,
    )
}

/// Cancel through the same lifecycle bridge used by the global Escape handler
/// so a click on the Dictation Bar cannot leave toggle/hold state believing a
/// discarded dictation is still active.
fn cancel_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.cancel(),
        slugtale_lib::DictationEvent::Cancel,
    )
}

/// End the active dictation through the shared lifecycle bridge, disarming bare
/// Escape while the registration lock is held. When no lifecycle answered — no
/// registration yet, or nothing active — the fallback event still runs so a
/// leftover Dictation Bar never outlives its dictation.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn end_active_dictation(
    app: &tauri::AppHandle,
    end: impl FnOnce(&mut slugtale_lib::DictationControl) -> Option<slugtale_lib::DictationEvent>,
    fallback: slugtale_lib::DictationEvent,
) -> Result<(), String> {
    let event = {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        let event = end(&mut registration.control);
        if event.is_some() {
            request_escape_registration(&registration, false);
        }
        event
    };

    match event {
        Some(event) => handle_dictation_event(app, event),
        None => handle_dictation_event(app, fallback),
    }
}

/// Begin a dictation from any activation input — a Hotkey press or a Voice
/// Activation wake phrase — through one readiness-gated sequence. The hotkey
/// worker and Voice Activation used to run two private copies of this dance
/// and had already drifted on the typing-challenge guard and the rollback.
///
/// `set_escape(true)` arms bare Escape before recording starts, so there is no
/// active but uncancellable dictation; `set_escape(false)` disarms it. The
/// hotkey worker arms synchronously, Voice Activation asks the global-key
/// worker — the caller owns that difference, everything else is shared.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn begin_dictation(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
    set_escape: &mut dyn FnMut(bool) -> Result<(), String>,
) -> Result<(), String> {
    // The Typing Challenge measures how fast the user types, so their hotkey
    // has to stay plain text for those thirty seconds. Swallowed here, before
    // any lifecycle state moves, so releasing it later cannot resume anything.
    let challenge_open = typing_challenge_is_open(app);
    if challenge_open {
        return Ok(());
    }

    let (activation, dictation_available) = {
        let activation = build_activation_snapshot_for(app, input);
        let available = activation.dictation_available();
        if !available {
            report_not_ready(app, &activation.report);
        }
        (Some(activation), available)
    };

    let event = {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration
            .control
            .begin(challenge_open, dictation_available)
    };
    let Ok(event) = event else {
        // ChallengeOpen and NotReady have already had their user-facing
        // report; AlreadyDictating means a later input changes nothing.
        return Ok(());
    };

    // Recording has not started yet; arming Escape here keeps the window where
    // the lifecycle says dictating but Escape is not global down to nothing.
    if let Err(error) = set_escape(true) {
        if let Ok(mut registration) = app.state::<HotkeyRegistrationState>().0.lock() {
            registration.control.abandon_begin();
        }
        eprintln!("dictation did not start because global Escape could not be registered");
        return Err(error);
    }

    if let Err(error) = handle_dictation_event_with(app, event, activation) {
        // Roll the lifecycle back so the next activation can try again instead
        // of finding a discarded dictation still marked active.
        if let Ok(mut registration) = app.state::<HotkeyRegistrationState>().0.lock() {
            registration.control.abandon_begin();
            request_escape_registration(&registration, false);
        }
        return Err(error);
    }

    Ok(())
}

fn handle_dictation_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
) -> Result<(), String> {
    handle_dictation_event_with(app, event, None)
}

/// `activation` is the snapshot a Hotkey press built for its readiness gate;
/// Start consumes it so the rest of the activation reuses the same Settings
/// value instead of reloading (slugtale-g1o.6). Callers without one — Cancel
/// from the tray, tests — pass `None`.
fn handle_dictation_event_with(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
    mut activation: Option<slugtale_lib::DictationActivation>,
) -> Result<(), String> {
    record_diagnostic_event(app, slugtale_lib::DiagnosticEvent::hotkey_transition(event));

    match event {
        slugtale_lib::DictationEvent::Start => {
            // Capture the app the user is dictating into before our own bar can
            // take focus, so insertion can re-target it later (slugtale-squ).
            capture_focus_target(app);
            // Open the dictation before capture starts: the level callback
            // installed below stamps every Pause Flush with this number.
            app.state::<DictationSegments>().begin();
            // If the microphone cannot start, do not show a recording state.
            handle_audio_capture_event(app, event)?;
            let settings = match activation.take() {
                Some(activation) => activation.settings,
                None => load_current_settings(app),
            };
            apply_recording_feedback(app, event, Some(&settings))?;
        }
        // Stop plays its cue but leaves the bar on screen: the audio-capture step
        // switches it to a transcribing state and hides it once the workflow
        // finishes, so the user sees the model working (slugtale-0t4). Its bar
        // update is this Stop press's own activation, so read Settings once here.
        slugtale_lib::DictationEvent::Stop => {
            advance_recording_feedback(app, event)?;
            let settings = load_current_settings(app);
            handle_audio_capture_event_with_settings(app, event, Some(&settings))?;
        }
        // Cancel clears the bar immediately and discards the audio. It also
        // drops any Dictation Segment still queued, so nothing further is typed
        // after the user asks Slugtale to stop. Text inserted by an earlier
        // Segment Pause is not undone (ADR-0014). It reads no Settings at all.
        slugtale_lib::DictationEvent::Cancel => {
            app.state::<DictationSegments>().abandon();
            apply_recording_feedback(app, event, None)?;
            handle_audio_capture_event(app, event)?;
        }
    }

    Ok(())
}

/// Advance the recording-feedback state machine and play its audible cue without
/// touching the Dictation Bar window. Callers that own the bar's visibility (Stop,
/// which keeps it up for transcription) use this directly.
fn advance_recording_feedback(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
) -> Result<slugtale_lib::RecordingFeedbackEffect, String> {
    let feedback = app.state::<RecordingFeedbackState>();
    let effect = {
        let mut guard = feedback
            .0
            .lock()
            .map_err(|_| "recording feedback mutex poisoned".to_string())?;
        guard.on_event(event)
    };

    if let Some(sound) = effect.sound {
        let _ = slugtale_lib::play_dictation_sound(sound);
    }

    Ok(effect)
}

fn apply_recording_feedback(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
    settings: Option<&slugtale_lib::Settings>,
) -> Result<(), String> {
    let effect = advance_recording_feedback(app, event)?;

    if effect.bar_visible {
        // Only the visible branch needs Settings; Cancel passes `None` and
        // never pays for a read.
        let owned;
        let settings = match settings {
            Some(settings) => settings,
            None => {
                owned = load_current_settings(app);
                &owned
            }
        };
        show_dictation_bar(app, DictationPhase::Recording, settings);
    } else {
        hide_dictation_bar(app);
    }

    Ok(())
}

fn capture_focus_target(app: &tauri::AppHandle) {
    if let Ok(mut guard) = app.state::<FocusTargetState>().0.lock() {
        *guard = slugtale_lib::capture_text_target();
    }
}

fn handle_audio_capture_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
) -> Result<(), String> {
    handle_audio_capture_event_with_settings(app, event, None)
}

/// `bar_settings` is needed only when a Stop completes and the bar switches to
/// its transcribing state; passing it in spares that path a Settings reload
/// (slugtale-g1o.6).
fn handle_audio_capture_event_with_settings(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
    bar_settings: Option<&slugtale_lib::Settings>,
) -> Result<(), String> {
    let capture = app.state::<AudioCaptureState>();
    let outcome = {
        let mut guard = capture
            .0
            .lock()
            .map_err(|_| "audio capture mutex poisoned".to_string())?;
        if matches!(event, slugtale_lib::DictationEvent::Start) {
            guard
                .recorder_mut()
                .set_level_callback(Some(dictation_audio_level_callback(app.clone())));
        }
        guard.on_event(event)
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            clear_dictation_audio_level_callback(app);
            hide_dictation_bar(app);
            record_diagnostic_event(
                app,
                slugtale_lib::DiagnosticEvent::audio_capture_failed(&error),
            );
            #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
            let _ = slugtale_lib::notify("Slugtale could not capture audio", &error.to_string());
            return Err(error.to_string());
        }
    };

    match outcome {
        Some(slugtale_lib::AudioCaptureOutcome::Completed(audio)) => {
            clear_dictation_audio_level_callback(app);
            eprintln!(
                "captured dictation audio: {} samples at {} Hz",
                audio.samples.len(),
                audio.sample_rate_hz
            );
            // Keep the bar on screen in a transcribing state while the model runs,
            // then hide it once insertion completes (slugtale-0t4). The worker
            // hides it, so it stays up until every earlier Segment Pause has
            // landed too, not just this last one.
            show_dictation_bar(
                app,
                DictationPhase::Transcribing,
                bar_settings.unwrap_or(&load_current_settings(app)),
            );
            let segments = app.state::<DictationSegments>();
            let queued = segments.send(slugtale_lib::DictationSegmentJob::Last {
                dictation: segments.current(),
                audio,
            });
            if !queued {
                eprintln!("dictation segment worker is unavailable; dropping final segment");
                hide_dictation_bar(app);
            }
        }
        Some(slugtale_lib::AudioCaptureOutcome::Discarded) => {
            clear_dictation_audio_level_callback(app);
            eprintln!("discarded dictation audio");
            hide_dictation_bar(app);
        }
        // No active session to drain. A terminal event still clears any bar left
        // on screen (e.g. Stop with nothing captured); Start has none to hide.
        None => {
            if matches!(event, slugtale_lib::DictationEvent::Stop) {
                hide_dictation_bar(app);
            }
        }
    }

    Ok(())
}

fn dictation_audio_level_callback(app: tauri::AppHandle) -> slugtale_lib::AudioLevelCallback {
    // One detector per dictation. The callback is installed on Start, so every
    // dictation begins with a detector that has heard nothing and therefore
    // cannot flush before the user has said anything.
    let detector = Mutex::new(slugtale_lib::SegmentPauseDetector::new());
    Arc::new(move |level| {
        emit_dictation_audio_level(&app, level);
        request_pause_flush_if_due(&app, &detector, level);
    })
}

/// Feed the voice level to the Segment Pause detector and queue a Pause Flush
/// when one has elapsed.
///
/// This runs on the recorder's level-emitter thread, so it must never block: it
/// takes only its own detector lock plus a brief capture-state lock to read the
/// voiced-sample watermark, and hands the queue a request rather than touching
/// the audio session.
fn request_pause_flush_if_due(
    app: &tauri::AppHandle,
    detector: &Mutex<slugtale_lib::SegmentPauseDetector>,
    level: f32,
) {
    let due = detector
        .lock()
        .map(|mut detector| detector.on_level(level, std::time::Instant::now()))
        .unwrap_or(false);
    if !due {
        return;
    }

    let segments = app.state::<DictationSegments>();
    if segments.pause_flushes_suspended() {
        return;
    }

    // Cut at the last voiced sample the ring knows about, not at whatever has
    // arrived by the time the worker gets here — queue delay must not turn
    // into extra tail audio in the segment (slugtale-g1o.4).
    let cut = app
        .state::<AudioCaptureState>()
        .0
        .lock()
        .map(|guard| slugtale_lib::AudioRecorder::voice_watermark(guard.recorder()))
        .unwrap_or(0);

    segments.send(slugtale_lib::DictationSegmentJob::PauseFlush {
        dictation: segments.current(),
        cut,
    });
}

fn clear_dictation_audio_level_callback(app: &tauri::AppHandle) {
    if let Ok(mut guard) = app.state::<AudioCaptureState>().0.lock() {
        guard.recorder_mut().set_level_callback(None);
    }
    emit_dictation_audio_level(app, 0.0);
}

fn emit_dictation_audio_level(app: &tauri::AppHandle, level: f32) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let _ = window.emit("dictation-audio-level", level.clamp(0.0, 1.0));
    }
}

/// Warm whichever Transcription Engine the current Settings resolve to,
/// including fallback rules, and release large models it does not need. The
/// model load itself runs off this thread, so startup and Settings saves never
/// wait on a multi-second read.
fn warm_effective_primary_engine(app: &tauri::AppHandle) {
    let settings = load_current_settings(app);
    let catalogue = app.state::<slugtale_lib::TranscriptionEngineCatalogue>();
    let Some(warm_up) = catalogue.prepare_primary_warm_up(&settings) else {
        return;
    };
    // Release before loading so switching engines never leaves two large
    // models resident on a memory-constrained Mac.
    catalogue.release_models_except(warm_up.engine());
    tauri::async_runtime::spawn_blocking(move || {
        let _ = warm_up.run();
    });
}

/// Transcribe and insert one Dictation Segment, start to finish.
///
/// Runs synchronously on the Dictation Segment worker thread. Everything it
/// touches is resolved per segment rather than per dictation, so a Settings
/// change part-way through a long dictation takes effect at the next Segment
/// Pause instead of being pinned at Start.
fn run_dictation_segment(
    app: &tauri::AppHandle,
    audio: slugtale_lib::CapturedAudio,
    position: slugtale_lib::DictationSegmentPosition,
) -> Result<slugtale_lib::DictationSegmentOutcome, String> {
    let settings = load_current_settings(app);
    let diagnostic_log = current_diagnostic_log(app, &settings);
    let stack = app
        .state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .dictation_stack(&settings, diagnostic_log.clone())
        .map_err(|error| error.to_string())?;
    let target_pid = app
        .state::<FocusTargetState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| *guard);

    let prepared = slugtale_lib::prepare_text_insertion(target_pid)?;
    let runtime = stack.asr_runtime();
    let insertion = DiagnosticTextInsertion::new(&prepared.insertion, diagnostic_log.clone());
    let rescue = DiagnosticInsertionRescue::new(prepared.rescue.as_ref(), diagnostic_log);
    slugtale_lib::DictationWorkflow::new(&runtime, &insertion, &rescue, settings.transcript_cleanup)
        .complete(audio, position)
        .map_err(|error| error.to_string())
}

/// Take the speech captured so far as a Dictation Segment, leaving the
/// microphone running. Called only from the worker thread. `cut` is the sample
/// watermark the Pause Flush was queued with: the segment ends there (plus a
/// small acoustic guard), whatever else has arrived since.
fn take_dictation_segment(app: &tauri::AppHandle, cut: u64) -> Option<slugtale_lib::CapturedAudio> {
    let capture = app.state::<AudioCaptureState>();
    let flushed = capture
        .0
        .lock()
        .map_err(|_| "audio capture mutex poisoned".to_string())
        .and_then(|mut guard| {
            guard
                .flush_segment_through(cut)
                .map_err(|error| error.to_string())
        });

    match flushed {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("could not take dictation segment: {error}");
            None
        }
    }
}

struct AppSegmentExecution<'a> {
    app: &'a tauri::AppHandle,
}

impl slugtale_lib::DictationSegmentExecution for AppSegmentExecution<'_> {
    type Error = String;

    fn take_pause_segment(&mut self, cut: u64) -> Option<slugtale_lib::CapturedAudio> {
        take_dictation_segment(self.app, cut)
    }

    fn complete(
        &mut self,
        audio: slugtale_lib::CapturedAudio,
        position: slugtale_lib::DictationSegmentPosition,
    ) -> Result<slugtale_lib::DictationSegmentOutcome, Self::Error> {
        run_dictation_segment(self.app, audio, position)
    }

    fn record(&mut self, segment: slugtale_lib::CountedSegment) {
        self.app.state::<UsageRecorder>().record(UsageUpdate {
            date: slugtale_lib::today_local(),
            segment,
        });
    }
}

/// Start the single worker that transcribes and inserts Dictation Segments.
///
/// Segments are decoded one at a time on purpose. Whisper would happily be
/// asked for two at once, but then a short segment could overtake a long one and
/// the user's words would land out of order — so the queue is the ordering
/// guarantee, and the cost is that a slow segment delays the next.
fn start_dictation_segment_worker(app: &tauri::AppHandle) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel::<slugtale_lib::DictationSegmentJob>();
    {
        let segments = app.state::<DictationSegments>();
        let mut jobs = segments
            .jobs
            .lock()
            .map_err(|_| "dictation segment queue mutex poisoned".to_string())?;
        *jobs = Some(sender);
    }

    let app = app.clone();
    std::thread::Builder::new()
        .name("slugtale-dictation-segments".to_string())
        .spawn(move || {
            let mut worker = slugtale_lib::DictationSegmentWorker::default();

            while let Ok(job) = receiver.recv() {
                let last = job.is_last();
                let segments = app.state::<DictationSegments>();
                let mut execution = AppSegmentExecution { app: &app };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker.process(job, segments.control(), &mut execution)
                }));
                match result {
                    Ok(Ok(slugtale_lib::DictationSegmentJobResult::Completed {
                        inserted,
                        text_chars,
                        ..
                    })) => {
                        if inserted {
                            eprintln!("inserted dictation segment: {text_chars} chars");
                        } else {
                            eprintln!("dictation segment heard nothing; inserted nothing");
                        }
                    }
                    Ok(Ok(slugtale_lib::DictationSegmentJobResult::Skipped { .. })) => {}
                    Ok(Err(error)) => eprintln!("dictation workflow failed: {error}"),
                    Err(_) => eprintln!("dictation segment panicked; the queue stays open"),
                }
                if last {
                    hide_dictation_bar(&app);
                }
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn setup_configured_hotkey(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_current_settings(app.handle());

    let mut builder =
        tauri_plugin_global_shortcut::Builder::new().with_handler(move |app, shortcut, event| {
            let input = match event.state {
                tauri_plugin_global_shortcut::ShortcutState::Pressed => {
                    slugtale_lib::HotkeyInput::Pressed
                }
                tauri_plugin_global_shortcut::ShortcutState::Released => {
                    slugtale_lib::HotkeyInput::Released
                }
            };

            let state = app.state::<HotkeyRegistrationState>();
            let registration = state.0.lock();
            match registration {
                Ok(registration) => {
                    if let Some(commands) = registration.key_commands.as_ref() {
                        let key = if shortcut.key == tauri_plugin_global_shortcut::Code::Escape
                            && shortcut.mods.is_empty()
                        {
                            slugtale_lib::DictationKey::Escape
                        } else {
                            slugtale_lib::DictationKey::Hotkey
                        };
                        let _ = commands.send(GlobalKeyCommand::Input(key, input));
                    }
                }
                Err(_) => eprintln!("hotkey dictation adapter mutex poisoned"),
            }
        });

    if let Some(hotkey) = settings.hotkey.as_deref() {
        builder = builder.with_shortcut(hotkey)?;
    }

    app.handle().plugin(builder.build())?;
    set_hotkey_registration_state(app.handle(), &settings)?;
    start_global_key_worker(app.handle())?;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn request_escape_registration(registration: &HotkeyRegistration, should_register: bool) {
    if let Some(commands) = registration.key_commands.as_ref() {
        let _ = commands.send(GlobalKeyCommand::SyncEscape(should_register));
    }
}

/// Bare Escape must only be global while recording; otherwise Slugtale would
/// steal Escape from the user's current application. A dedicated worker first
/// registers Escape and only then starts recording, so there is no active but
/// uncancellable window. It also keeps registration outside the plugin callback,
/// which holds the plugin's key map while invoking us.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn start_global_key_worker(app: &tauri::AppHandle) -> Result<(), String> {
    let (commands, events) = std::sync::mpsc::channel::<GlobalKeyCommand>();
    {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration.key_commands = Some(commands);
    }

    let app = app.clone();
    std::thread::Builder::new()
        .name("dictation-global-keys".to_string())
        .spawn(move || {
            let mut escape_registered = false;
            for event in events {
                match event {
                    GlobalKeyCommand::SyncEscape(should_register) => {
                        if let Err(error) =
                            sync_escape_registration(&app, &mut escape_registered, should_register)
                        {
                            eprintln!("could not update global Escape key: {error}");
                        }
                    }
                    GlobalKeyCommand::Input(key, input) => {
                        // The Typing Challenge guard also lives inside
                        // begin_dictation; this early check keeps the release of
                        // a swallowed key from reaching the lifecycle at all.
                        if typing_challenge_is_open(&app) {
                            continue;
                        }

                        let is_dictating = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .map(|registration| registration.control.is_dictating())
                            .unwrap_or(false);

                        let pressed_start = matches!(
                            (key, input),
                            (
                                slugtale_lib::DictationKey::Hotkey,
                                slugtale_lib::HotkeyInput::Pressed
                            )
                        );

                        // A start goes through the shared readiness-gated begin
                        // sequence, identical to the Voice Activation path.
                        // One snapshot for this press: the readiness gate and
                        // the Start path share the same Settings value and
                        // permission probes (slugtale-g1o.6).
                        if pressed_start && !is_dictating {
                            let mut set_escape = |should_register: bool| {
                                sync_escape_registration(
                                    &app,
                                    &mut escape_registered,
                                    should_register,
                                )
                            };
                            if let Err(error) = begin_dictation(
                                &app,
                                slugtale_lib::DictationInput::Hotkey,
                                &mut set_escape,
                            ) {
                                eprintln!("dictation did not start: {error}");
                            }
                            continue;
                        }

                        let transition = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .and_then(|mut registration| {
                                let event = match (key, input) {
                                    (slugtale_lib::DictationKey::Hotkey, input) => {
                                        registration.control.on_hotkey(input)
                                    }
                                    (
                                        slugtale_lib::DictationKey::Escape,
                                        slugtale_lib::HotkeyInput::Pressed,
                                    ) => registration.control.cancel(),
                                    (
                                        slugtale_lib::DictationKey::Escape,
                                        slugtale_lib::HotkeyInput::Released,
                                    ) => None,
                                };
                                Some((event, registration.control.is_dictating()))
                            });
                        if let Some((event, should_register)) = transition {
                            // The shared registration mutex is no longer held:
                            // recording, transcription, and window work may block
                            // without preventing the main-thread shortcut handler
                            // from forwarding the next key transition (slugtale-pil).
                            if let Some(event) = event {
                                if let Err(error) = handle_dictation_event_with(&app, event, None) {
                                    eprintln!("dictation event failed: {error}");
                                }
                            }
                            if let Err(error) = sync_escape_registration(
                                &app,
                                &mut escape_registered,
                                should_register,
                            ) {
                                eprintln!("could not update global Escape key: {error}");
                            }
                        }
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn sync_escape_registration(
    app: &tauri::AppHandle,
    registered: &mut bool,
    should_register: bool,
) -> Result<(), String> {
    if should_register == *registered {
        return Ok(());
    }

    if should_register {
        app.global_shortcut()
            .register(DICTATION_ESCAPE_KEY)
            .map_err(|error| error.to_string())?;
    } else {
        app.global_shortcut()
            .unregister(DICTATION_ESCAPE_KEY)
            .map_err(|error| error.to_string())?;
    }
    *registered = should_register;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn set_hotkey_registration_state(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    let state = app.state::<HotkeyRegistrationState>();
    let mut registration = state
        .0
        .lock()
        .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
    registration.current_hotkey = settings.hotkey.clone();
    // The lifecycle belongs to dictation, not to the optional hotkey. Voice
    // Activation and Dictation Bar controls use it even with no hotkey set.
    registration.control = slugtale_lib::DictationControl::new(settings.activation_mode);
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn update_registered_hotkey(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    let previous = {
        let state = app.state::<HotkeyRegistrationState>();
        let registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration.current_hotkey.clone()
    };
    let next = settings.hotkey.clone();

    if previous != next {
        if let Some(hotkey) = next.as_deref() {
            app.global_shortcut()
                .register(hotkey)
                .map_err(|error| error.to_string())?;
        }

        if let Some(hotkey) = previous.as_deref() {
            if let Err(error) = app.global_shortcut().unregister(hotkey) {
                if let Some(new_hotkey) = next.as_deref() {
                    let _ = app.global_shortcut().unregister(new_hotkey);
                }
                return Err(error.to_string());
            }
        }
    }

    set_hotkey_registration_state(app, settings)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn update_registered_hotkey(
    _app: &tauri::AppHandle,
    _settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    Ok(())
}

/// The Dictation Bar's user-chosen appearance, pushed to the bar window so it can
/// paint its accent and align its orb to the edge it was sent to.
#[derive(Clone, serde::Serialize)]
struct DictationBarAppearance {
    position: slugtale_lib::BarPosition,
    accent: slugtale_lib::AccentColor,
}

impl DictationBarAppearance {
    fn from_settings(settings: &slugtale_lib::Settings) -> Self {
        Self {
            position: settings.bar_position,
            accent: settings.accent_color,
        }
    }
}

fn show_dictation_bar(
    app: &tauri::AppHandle,
    phase: DictationPhase,
    settings: &slugtale_lib::Settings,
) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let appearance = DictationBarAppearance::from_settings(settings);
        let bar_display = settings.bar_display.clone();
        // Tell the frontend which state to render before showing, so the bar never
        // flashes a stale "recording" pill when it reappears for transcription.
        let _ = window.emit("dictation-phase", phase.as_str());
        // Same reason for the appearance: a bar that appears in the old accent or
        // aligned to the old edge and then jumps is worse than one that never did.
        let _ = window.emit("dictation-appearance", appearance.clone());
        // The bar polls for the pointer only while it is on screen; the webview
        // stays alive between dictations and has no other way to know.
        let _ = window.emit("dictation-visibility", true);
        // Placing the bar reads monitor geometry, and those reads block until the
        // main thread answers them. The global-key worker calls this while holding
        // the hotkey registration lock, and the main thread takes that same lock on
        // the next key transition — doing the work inline deadlocks both threads and
        // freezes the tray. Hand the window work to the main thread instead of
        // waiting on it (slugtale-1n4).
        let _ = app.run_on_main_thread(move || {
            position_dictation_bar(&window, appearance.position, &bar_display);
            // Start click-through: at rest the orb covers a seventh of the window,
            // and the pointer is somewhere else entirely. The bar takes input back
            // only when the hit test says the pointer is genuinely over the paint.
            let _ = window.set_ignore_cursor_events(true);
            let _ = window.show();
            if slugtale_lib::dictation_bar_should_take_focus() {
                let _ = window.set_focus();
            }
        });
    }
}

fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let _ = window.hide();
        let _ = window.emit("dictation-visibility", false);
    }
}

/// Read the usable work area for the selected Dictation Bar display, in the
/// form the pure geometry wants. A disconnected named display falls back to the
/// main display so the bar never gets stranded off-screen.
fn dictation_bar_monitor(
    window: &tauri::WebviewWindow,
    display: &slugtale_lib::BarDisplay,
) -> Option<slugtale_lib::MonitorGeometry> {
    let primary = window.primary_monitor().ok().flatten();
    let monitor = match display {
        slugtale_lib::BarDisplay::Primary => primary,
        slugtale_lib::BarDisplay::Monitor(name) => window
            .available_monitors()
            .ok()
            .and_then(|monitors| {
                monitors
                    .into_iter()
                    .find(|monitor| monitor.name() == Some(name))
            })
            .or(primary),
    };

    let monitor = monitor?;

    let work_area = monitor.work_area();

    Some(slugtale_lib::MonitorGeometry {
        origin_x: work_area.position.x,
        origin_y: work_area.position.y,
        width: work_area.size.width,
        height: work_area.size.height,
        scale_factor: monitor.scale_factor(),
    })
}

/// Place the Dictation Bar along the bottom edge of the selected display's work
/// area, at the corner the user chose. The geometry itself lives in lib.rs;
/// this only supplies the live monitor and window reads.
fn position_dictation_bar(
    window: &tauri::WebviewWindow,
    position: slugtale_lib::BarPosition,
    display: &slugtale_lib::BarDisplay,
) {
    let Some(monitor) = dictation_bar_monitor(window, display) else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };

    let (x, y) = slugtale_lib::dictation_bar_origin(&monitor, size.width, size.height, position);
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Hand the pointer to whichever of Slugtale and the app underneath it is
/// actually over, and tell the bar which one that is.
///
/// The bar window is permanently sized for the expanded pill because a Tauri
/// window cannot grow on hover, so while collapsed most of it is transparent —
/// and a transparent window still swallows clicks. The frontend polls this while
/// the bar is visible: it cannot detect the pointer itself, because a window
/// ignoring cursor events receives no mouse events to detect it with.
#[tauri::command]
fn dictation_bar_pointer_over(app: tauri::AppHandle, expanded: bool) -> Result<bool, String> {
    let Some(window) = app.get_webview_window("dictation-bar") else {
        return Ok(false);
    };

    let position = load_current_settings(&app).bar_position;
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let origin = window.outer_position().map_err(|error| error.to_string())?;
    let pointer = app.cursor_position().map_err(|error| error.to_string())?;

    let over = slugtale_lib::pointer_is_over_dictation_bar(
        (pointer.x, pointer.y),
        (origin.x, origin.y),
        scale_factor,
        position,
        expanded,
    );
    window
        .set_ignore_cursor_events(!over)
        .map_err(|error| error.to_string())?;

    Ok(over)
}

fn current_settings_readiness(app: &tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    let settings = load_current_settings(app);
    let platform = CurrentPlatform::new();
    let local_model_ready = local_model_ready(app);
    let microphone_granted = slugtale_lib::PlatformReadiness::microphone_granted(&platform);
    let insertion_granted = slugtale_lib::PlatformReadiness::insertion_granted(&platform);
    let engines = current_engine_availability(app, &settings);
    let input = if voice_activation::supported() && settings.voice_activation_enabled {
        slugtale_lib::DictationInput::VoiceActivation
    } else {
        slugtale_lib::DictationInput::Hotkey
    };
    slugtale_lib::settings_readiness_report_checked_for_input(
        &settings,
        microphone_granted,
        insertion_granted,
        local_model_ready,
        &engines,
        input,
    )
}

/// Whether the Whisper ggml file — or a user-selected custom model — is on disk.
fn local_model_ready(app: &tauri::AppHandle) -> bool {
    model_manager(app)
        .map(|manager| manager.ready())
        .unwrap_or_else(|_| {
            load_current_settings(app)
                .model
                .as_ref()
                .is_some_and(|path| PathBuf::from(path).exists())
        })
}

fn build_activation_snapshot_for(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
) -> slugtale_lib::DictationActivation {
    let settings = load_current_settings(app);
    let platform = CurrentPlatform::new();
    let local_model_ready = local_model_ready(app);
    let engines = current_engine_availability(app, &settings);
    slugtale_lib::DictationActivation::build_for_input(
        settings,
        &platform,
        local_model_ready,
        engines,
        input,
    )
}

/// Engine availability for the readiness report, asked of the same providers the
/// dictation path uses so the two cannot disagree.
fn current_engine_availability(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Vec<(
    slugtale_lib::TranscriptionEngine,
    slugtale_lib::EngineAvailability,
)> {
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .availability(settings)
}

/// Tell the user which required items are missing and open Settings, where
/// they can act on each one.
fn report_not_ready(
    app: &tauri::AppHandle,
    report: &slugtale_lib::SettingsReadinessReport,
) -> bool {
    let missing = report
        .items
        .iter()
        .filter(|item| item.required && !item.ready)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        record_diagnostic_event(
            app,
            slugtale_lib::DiagnosticEvent::readiness_incomplete(&missing),
        );
        let labels = missing
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = slugtale_lib::notify(
            "Slugtale is not ready to dictate",
            &format!("Finish these items in Slugtale Settings: {labels}."),
        );
    }
    slugtale_lib::show_settings(app.clone());
    true
}

#[tauri::command]
fn get_settings_readiness(app: tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    let report = current_settings_readiness(&app);
    let local_model_ready = report
        .items
        .iter()
        .find(|item| item.id == "local_model")
        .is_some_and(|item| item.ready);
    if local_model_ready {
        warm_effective_primary_engine(&app);
    }
    if !report.dictation_available {
        let missing = report
            .items
            .iter()
            .filter(|item| item.required && !item.ready)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            record_diagnostic_event(
                &app,
                slugtale_lib::DiagnosticEvent::readiness_incomplete(&missing),
            );
        }
    }
    report
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> slugtale_lib::Settings {
    load_current_settings(&app)
}

/// One selectable display in the Settings UI. The stable monitor name is stored
/// in the Settings File; its label adds resolution so similarly named displays
/// remain distinguishable.
#[derive(serde::Serialize)]
struct DictationBarDisplayOption {
    value: slugtale_lib::BarDisplay,
    label: String,
}

/// Return the displays that can host the Dictation Bar right now. Displays with
/// no stable name cannot be selected safely across app launches, but the main
/// display is always available as the fallback choice.
#[tauri::command]
fn dictation_bar_displays(app: tauri::AppHandle) -> Vec<DictationBarDisplayOption> {
    let primary = app.primary_monitor().ok().flatten();
    let primary_label = primary
        .as_ref()
        .and_then(|monitor| monitor.name())
        .map(|name| format!("Main display ({name})"))
        .unwrap_or_else(|| "Main display".to_string());
    let mut displays = vec![DictationBarDisplayOption {
        value: slugtale_lib::BarDisplay::Primary,
        label: primary_label,
    }];

    let monitors = app.available_monitors().unwrap_or_default();
    for monitor in monitors {
        if primary.as_ref().is_some_and(|primary| {
            monitor.position() == primary.position() && monitor.size() == primary.size()
        }) {
            continue;
        }
        let Some(name) = monitor.name().cloned() else {
            continue;
        };
        let size = monitor.size();
        displays.push(DictationBarDisplayOption {
            value: slugtale_lib::BarDisplay::Monitor(name.clone()),
            label: format!("{name} ({} × {})", size.width, size.height),
        });
    }

    displays
}

#[tauri::command]
fn open_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::MacosMicrophonePermissionSetup,
        );
    }

    #[cfg(target_os = "windows")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::WindowsMicrophonePermissionSetup,
        );
    }

    #[cfg(target_os = "linux")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::LinuxMicrophonePermissionSetup,
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("microphone settings shortcut is not implemented for this platform".to_string())
    }
}

#[tauri::command]
fn open_text_insertion_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::MacosTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(target_os = "windows")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::WindowsTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(target_os = "linux")]
    {
        return slugtale_lib::run_text_insertion_permission_setup(
            &slugtale_lib::LinuxTextInsertionPermissionSetup,
        )
        .map(|_| ());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("text insertion settings shortcut is not implemented for this platform".to_string())
    }
}

#[tauri::command]
fn save_hotkey_settings(
    app: tauri::AppHandle,
    hotkey: Option<String>,
    activation_mode: slugtale_lib::ActivationMode,
) -> Result<slugtale_lib::Settings, String> {
    let previous = load_current_settings(&app);
    let mut settings = previous.clone();
    slugtale_lib::apply_hotkey_settings(&mut settings, hotkey, activation_mode);

    update_registered_hotkey(&app, &settings)?;
    if let Err(error) = save_current_settings(&app, &settings) {
        let _ = update_registered_hotkey(&app, &previous);
        return Err(error);
    }

    Ok(settings)
}

#[tauri::command]
fn save_transcription_settings(
    app: tauri::AppHandle,
    speed_profile: slugtale_lib::SpeedProfile,
) -> Result<slugtale_lib::Settings, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_transcription_settings(&mut settings, speed_profile);
    save_current_settings(&app, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn save_transcript_cleanup_settings(
    app: tauri::AppHandle,
    cleanup_mode: slugtale_lib::TranscriptCleanupMode,
) -> Result<slugtale_lib::Settings, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_transcript_cleanup_settings(&mut settings, cleanup_mode);
    save_current_settings(&app, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn voice_activation_supported() -> bool {
    voice_activation::supported()
}

/// Save the Voice Activation opt-in and bring the listener in line immediately.
/// Change the worker first, then persist. A failed worker must not leave a saved
/// "on" value while nothing is listening.
#[tauri::command]
fn save_voice_activation_settings(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    voice_activation::save_settings(&app, enabled)
}

#[tauri::command]
fn save_dictation_bar_settings(
    app: tauri::AppHandle,
    bar_position: slugtale_lib::BarPosition,
    accent_color: slugtale_lib::AccentColor,
    bar_display: slugtale_lib::BarDisplay,
) -> Result<slugtale_lib::Settings, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_dictation_bar_settings(
        &mut settings,
        bar_position,
        accent_color,
        bar_display,
    );
    save_current_settings(&app, &settings)?;
    apply_dictation_bar_appearance(&app, &settings);
    Ok(settings)
}

/// Push a saved appearance change to a bar that is already on screen, so the user
/// sees the choice they just made instead of waiting for the next dictation.
/// Does nothing visible when the bar is hidden — showing it re-sends both.
fn apply_dictation_bar_appearance(app: &tauri::AppHandle, settings: &slugtale_lib::Settings) {
    let Some(window) = app.get_webview_window("dictation-bar") else {
        return;
    };
    let appearance = DictationBarAppearance::from_settings(settings);
    let _ = window.emit("dictation-appearance", appearance.clone());

    if !window.is_visible().unwrap_or(false) {
        return;
    }
    // Repositioning reads monitor geometry, which blocks on the main thread;
    // hand it over rather than waiting on it from here (slugtale-1n4).
    let bar_display = settings.bar_display.clone();
    let _ = app.run_on_main_thread(move || {
        position_dictation_bar(&window, appearance.position, &bar_display);
    });
}

/// Register or unregister the app as an OS login item to match the desired state.
/// Backed by tauri-plugin-autostart (a macOS LaunchAgent), which keeps this off the
/// dictation hot path and gives the Windows port the same abstraction for free.
fn set_launch_at_login_state(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|error| error.to_string())
    } else {
        autolaunch.disable().map_err(|error| error.to_string())
    }
}

#[tauri::command]
fn save_launch_at_login(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    let previous = load_current_settings(&app);
    let mut settings = previous.clone();
    slugtale_lib::apply_launch_at_login_settings(&mut settings, enabled);

    set_launch_at_login_state(&app, enabled)?;
    if let Err(error) = save_current_settings(&app, &settings) {
        let _ = set_launch_at_login_state(&app, previous.launch_at_login);
        return Err(error);
    }

    Ok(settings)
}

/// What Settings renders for one app-update check (slugtale-9pr). `version` is
/// the newer build's version when one is available, and is what the user sees
/// before deciding to install.
#[derive(Debug, Clone, serde::Serialize)]
struct AppUpdateView {
    available: bool,
    version: Option<String>,
}

impl AppUpdateView {
    fn none() -> Self {
        Self {
            available: false,
            version: None,
        }
    }

    fn available(version: String) -> Self {
        Self {
            available: true,
            version: Some(version),
        }
    }
}

/// Ask GitHub Releases whether a newer signed build exists (ADR-0022). The
/// endpoint and public key live in tauri.conf.json; signature verification is
/// enforced by the plugin before anything is staged.
#[tauri::command]
async fn check_for_app_update(app: tauri::AppHandle) -> Result<AppUpdateView, String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater.check().await.map_err(|error| error.to_string())?;
    Ok(match update {
        Some(update) => AppUpdateView::available(update.version),
        None => AppUpdateView::none(),
    })
}

/// Download, verify, stage, and relaunch into a checked app update. The restart
/// never returns; the process replaces itself with the new build.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
async fn install_app_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no app update is available".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|error| error.to_string())?;
    app.restart();
}

/// What Settings needs to render one row of the Transcription Engines list
/// (slugtale-vjs.4): whether it is the current primary, its licence and
/// provenance from [`slugtale_lib::EngineMetadata`], whether it can run right
/// now, and how much of its assets are actually on disk.
///
/// This mirrors `EngineMetadata`/`EngineAvailability` rather than replacing
/// them — Settings renders the licence and attribution strings straight out of
/// `metadata` so the CC BY 4.0 wording is never retyped in the frontend.
#[derive(Debug, Clone, serde::Serialize)]
struct EngineView {
    id: &'static str,
    display_name: &'static str,
    is_primary: bool,
    metadata: slugtale_lib::EngineMetadata,
    availability: slugtale_lib::EngineAvailability,
    /// `availability`'s reason rendered through [`slugtale_lib::EngineUnavailable`]'s
    /// `Display`, so Settings shows the same wording the rest of Slugtale does
    /// rather than re-deriving copy per reason code in JavaScript. `None` when
    /// the engine is available.
    unavailable_reason: Option<String>,
    /// Whether Settings should offer an Install action right now. Mirrors
    /// [`slugtale_lib::EngineUnavailable::is_user_resolvable`]: only a missing-assets
    /// engine gets a button, never an unsupported OS or a build without the
    /// feature.
    installable: bool,
    assets: EngineAssetState,
}

/// Installed-asset accounting for one engine, kept separate from
/// [`slugtale_lib::EngineAvailability`] because an engine can be unavailable for
/// reasons that have nothing to do with assets (wrong OS, build without the
/// feature).
#[derive(Debug, Clone, serde::Serialize)]
struct EngineAssetState {
    /// Bytes on disk for assets Slugtale itself owns. `None` for Apple
    /// SpeechTranscriber, whose assets Slugtale never downloads or measures.
    installed_bytes: Option<u64>,
    /// Whether Slugtale's own copy of the assets is fully installed. `None` for
    /// system-managed engines; `availability` is the honest answer there.
    present: Option<bool>,
}

/// A [`slugtale_lib::TranscriptionProvider`] for the Whisper engine, built the
/// same way `complete_captured_dictation` builds one: from the cache keyed by
/// the currently configured model path. Constructing it does not load model
/// weights, so this is cheap enough to call every time Settings asks.
fn whisper_engine_provider(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<Arc<dyn TranscriptionProvider>, String> {
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .whisper_provider(settings)
        .ok_or_else(|| "could not resolve a local model directory for Whisper".to_string())
}

/// Build one engine's Settings row from its cached provider. Never re-probes:
/// every branch reads `metadata()`/`availability()` off a provider that was
/// already constructed (Whisper) or already registered at startup (Parakeet,
/// Apple SpeechTranscriber), matching how the dictation path itself asks these
/// questions.
fn build_engine_view(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
    engine: slugtale_lib::TranscriptionEngine,
) -> Result<EngineView, String> {
    let is_primary = settings.primary_engine == engine;

    let (metadata, availability, assets) = match engine {
        slugtale_lib::TranscriptionEngine::Whisper => {
            let provider = whisper_engine_provider(app, settings)?;
            let status = model_manager(app)?.status();
            (
                provider.metadata(),
                provider.availability(),
                EngineAssetState {
                    installed_bytes: status.bytes,
                    present: Some(status.present),
                },
            )
        }
        slugtale_lib::TranscriptionEngine::Parakeet => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            let status = provider.status();
            (
                provider.metadata(),
                provider.availability(),
                EngineAssetState {
                    installed_bytes: Some(status.installed_bytes),
                    present: Some(status.present),
                },
            )
        }
        slugtale_lib::TranscriptionEngine::AppleSpeech => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .apple_provider();
            (
                provider.metadata(),
                provider.availability(),
                // System-managed: Slugtale never downloads or measures these.
                EngineAssetState {
                    installed_bytes: None,
                    present: None,
                },
            )
        }
    };

    let (unavailable_reason, installable) = match &availability {
        slugtale_lib::EngineAvailability::Available => (None, false),
        slugtale_lib::EngineAvailability::Unavailable(reason) => {
            (Some(reason.to_string()), reason.is_user_resolvable())
        }
    };

    Ok(EngineView {
        id: engine.id(),
        display_name: engine.display_name(),
        is_primary,
        metadata,
        availability,
        unavailable_reason,
        installable,
        assets,
    })
}

/// Every Transcription Engine Settings can show, in [`slugtale_lib::TranscriptionEngine::ALL`]
/// order. Read-only and non-blocking: see [`build_engine_view`].
#[tauri::command]
fn transcription_engines(app: tauri::AppHandle) -> Result<Vec<EngineView>, String> {
    let settings = load_current_settings(&app);
    slugtale_lib::TranscriptionEngine::ALL
        .into_iter()
        .map(|engine| build_engine_view(&app, &settings, engine))
        .collect()
}

/// Persist the chosen primary engine and Second Opinion mode (slugtale-vjs.4).
/// Mirrors [`save_transcription_settings`]: no check that the chosen engine can
/// actually run, because availability can change after the choice is made and
/// is resolved fresh by `transcription_router` on the next dictation instead
/// (see [`slugtale_lib::apply_engine_settings`]).
#[tauri::command]
fn set_transcription_engines(
    app: tauri::AppHandle,
    primary_engine: slugtale_lib::TranscriptionEngine,
    second_opinion: slugtale_lib::SecondOpinionMode,
) -> Result<slugtale_lib::Settings, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_engine_settings(&mut settings, primary_engine, second_opinion);
    save_current_settings(&app, &settings)?;
    // Start warming the newly effective engine now so the first dictation
    // after the change does not pay for a cold model load.
    warm_effective_primary_engine(&app);
    Ok(settings)
}

/// Install one engine's assets as an explicit user action (slugtale-vjs.4).
///
/// Whisper and Parakeet both fetch pinned artefacts over HTTP and report
/// progress on `on_progress`, exactly like [`download_local_model`]. Apple
/// SpeechTranscriber has no download for Slugtale to drive — it asks macOS to
/// install its own system assets via
/// [`slugtale_lib::AppleSpeechProvider::request_asset_installation`], which
/// blocks for as long as that takes and reports no progress, so `on_progress`
/// is simply unused on that branch.
#[tauri::command]
async fn install_engine_assets(
    app: tauri::AppHandle,
    engine: slugtale_lib::TranscriptionEngine,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<EngineView, String> {
    match engine {
        slugtale_lib::TranscriptionEngine::Whisper => {
            let manager = model_manager(&app)?;
            let status = tauri::async_runtime::spawn_blocking(move || {
                // Throttle IPC traffic: the initial update, then one per ~1 MB,
                // plus the final update (slugtale-dtl).
                let mut forward = slugtale_lib::throttled_progress(move |progress| {
                    let _ = on_progress.send(progress);
                });
                manager
                    .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            if status.present {
                warm_effective_primary_engine(&app);
            }
        }
        slugtale_lib::TranscriptionEngine::Parakeet => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            let asset_dir = provider.asset_dir().to_path_buf();
            tauri::async_runtime::spawn_blocking(move || {
                // Throttle IPC traffic: the initial update, then one per ~1 MB,
                // plus the final update (slugtale-dtl).
                let mut forward = slugtale_lib::throttled_progress(move |progress| {
                    let _ = on_progress.send(progress);
                });
                slugtale_lib::install_parakeet_assets(
                    &asset_dir,
                    &slugtale_lib::HttpModelDownloader,
                    &mut forward,
                )
                .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            provider.refresh_availability();
        }
        slugtale_lib::TranscriptionEngine::AppleSpeech => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .apple_provider();
            tauri::async_runtime::spawn_blocking(move || provider.request_asset_installation())
                .await
                .map_err(|error| error.to_string())??;
        }
    }

    let settings = load_current_settings(&app);
    build_engine_view(&app, &settings, engine)
}

/// Remove one engine's installed assets as an explicit user action
/// (slugtale-vjs.4). Apple SpeechTranscriber's assets are macOS's, not
/// Slugtale's, so there is nothing here to delete — the branch refuses rather
/// than pretending to free space Slugtale never claimed.
#[tauri::command]
fn remove_engine_assets(
    app: tauri::AppHandle,
    engine: slugtale_lib::TranscriptionEngine,
) -> Result<EngineView, String> {
    match engine {
        slugtale_lib::TranscriptionEngine::Whisper => {
            model_manager(&app)?
                .delete_default()
                .map_err(|error| error.to_string())?;
        }
        slugtale_lib::TranscriptionEngine::Parakeet => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            slugtale_lib::delete_parakeet_assets(provider.asset_dir())
                .map_err(|error| error.to_string())?;
            provider.refresh_availability();
        }
        slugtale_lib::TranscriptionEngine::AppleSpeech => {
            return Err(
                "Apple SpeechTranscriber's assets are installed and managed by macOS; \
                 Slugtale cannot remove them."
                    .to_string(),
            );
        }
    }

    let settings = load_current_settings(&app);
    build_engine_view(&app, &settings, engine)
}

/// One span of the Usage pane — today, this week, or all time — with Time Saved
/// already computed and already worded.
///
/// Time Saved is sent as text rather than a number the frontend rounds, because
/// there is exactly one right way to say it (ADR-0025: prefix About, no
/// decimals) and duplicating that rule in JavaScript is how the two drift apart.
/// Speaking duration is deliberately not here: it is stored, but it is not a
/// number the pane shows.
#[derive(serde::Serialize)]
struct UsageSpan {
    dictations: u32,
    words: u32,
    /// `null` when there is no Typing Baseline, which is the hole the pane draws
    /// with a take-the-baseline action rather than an invented default WPM.
    time_saved: Option<String>,
}

fn usage_span(totals: &slugtale_lib::UsageTotals, words_per_minute: Option<u32>) -> UsageSpan {
    let seconds = slugtale_lib::time_saved_seconds(totals, words_per_minute);
    UsageSpan {
        dictations: totals.dictations,
        words: totals.words,
        time_saved: seconds.map(|seconds| slugtale_lib::format_time_saved(Some(seconds))),
    }
}

/// Everything the Usage pane draws, in one answer.
#[derive(serde::Serialize)]
struct UsageSummary {
    /// Whether Daily Usage Records are being written at all.
    store_usage: bool,
    today: UsageSpan,
    this_week: UsageSpan,
    all_time: UsageSpan,
    /// The measured Typing Baseline, or `null` until all three Typing Challenges
    /// are done.
    measured_wpm: Option<u32>,
    /// The user's typed stand-in, whether or not it is the one in use.
    typed_estimate: Option<u32>,
    /// How many of the three Typing Challenges are finished, for "2 of 3".
    completed_challenges: usize,
    challenge_count: usize,
}

#[tauri::command]
fn get_usage_summary(app: tauri::AppHandle) -> UsageSummary {
    let settings = load_current_settings(&app);
    let baseline = &settings.typing_baseline;
    let words_per_minute = baseline.effective_wpm();
    // With storing off there is no Usage File, so every span is zero — but the
    // Typing Baseline still reads, because the challenges work either way.
    let usage = if settings.store_usage {
        load_current_usage(&app)
    } else {
        slugtale_lib::UsageFile::default()
    };
    let today = slugtale_lib::today_local();
    let week_start = locale_week_start(&app);

    UsageSummary {
        store_usage: settings.store_usage,
        today: usage_span(
            &slugtale_lib::totals_for_day(&usage, today),
            words_per_minute,
        ),
        this_week: usage_span(
            &slugtale_lib::totals_for_week(&usage, today, week_start),
            words_per_minute,
        ),
        all_time: usage_span(&slugtale_lib::totals_all_time(&usage), words_per_minute),
        measured_wpm: baseline.measured_wpm(),
        typed_estimate: baseline.typed_estimate,
        completed_challenges: baseline.completed_challenges(),
        challenge_count: slugtale_lib::TYPING_CHALLENGE_COUNT,
    }
}

/// Turn storing Daily Usage Records on or off.
///
/// Turning it off deletes the Usage File outright rather than leaving it to rot
/// unread: "stop storing this" has to mean the stored thing is gone. The Typing
/// Baseline is in the Settings File and is untouched.
#[tauri::command]
fn set_usage_storing(app: tauri::AppHandle, enabled: bool) -> Result<UsageSummary, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_usage_settings(&mut settings, enabled);
    save_current_settings(&app, &settings)?;

    if !enabled {
        if let Some(path) = usage_path(&app) {
            slugtale_lib::delete_usage(&path).map_err(|error| error.to_string())?;
        }
    }

    Ok(get_usage_summary(app))
}

/// Set or clear the typed typing-speed estimate. Refused once the three Typing
/// Challenges have produced a measurement.
#[tauri::command]
fn set_typing_estimate(
    app: tauri::AppHandle,
    estimate: Option<u32>,
) -> Result<UsageSummary, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_typed_estimate(&mut settings.typing_baseline, estimate)
        .map_err(|error| error.to_string())?;
    save_current_settings(&app, &settings)?;

    Ok(get_usage_summary(app))
}

/// The state of the Typing Challenge window: which passage to show next and how
/// far through the three the user is.
#[derive(serde::Serialize)]
struct TypingChallengeState {
    /// The passage to type, or `null` when all three are done.
    passage: Option<String>,
    passage_index: Option<usize>,
    completed: usize,
    total: usize,
    seconds: u32,
    measured_wpm: Option<u32>,
}

fn typing_challenge_state(baseline: &slugtale_lib::TypingBaseline) -> TypingChallengeState {
    let passage_index = baseline.next_passage_index();
    TypingChallengeState {
        passage: passage_index.map(|index| {
            slugtale_lib::TYPING_CHALLENGE_PASSAGES[index]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }),
        passage_index,
        completed: baseline.completed_challenges(),
        total: slugtale_lib::TYPING_CHALLENGE_COUNT,
        seconds: slugtale_lib::TYPING_CHALLENGE_SECONDS,
        measured_wpm: baseline.measured_wpm(),
    }
}

#[tauri::command]
fn get_typing_challenge(app: tauri::AppHandle) -> TypingChallengeState {
    typing_challenge_state(&load_current_settings(&app).typing_baseline)
}

/// Score one finished Typing Challenge and store it.
///
/// The window sends the text as it finally stood, so backspacing is free — which
/// is how people type, and the point is to measure that.
#[tauri::command]
fn submit_typing_challenge(
    app: tauri::AppHandle,
    passage_index: usize,
    typed: String,
) -> Result<TypingChallengeState, String> {
    let passage = slugtale_lib::TYPING_CHALLENGE_PASSAGES
        .get(passage_index)
        .ok_or_else(|| format!("there is no typing challenge passage {passage_index}"))?;
    let words_per_minute = slugtale_lib::score_typing_challenge(
        passage,
        &typed,
        slugtale_lib::TYPING_CHALLENGE_SECONDS,
    );

    let mut settings = load_current_settings(&app);
    slugtale_lib::record_typing_challenge(
        &mut settings.typing_baseline,
        passage_index,
        words_per_minute,
    );
    save_current_settings(&app, &settings)?;

    notify_usage_changed(&app);
    Ok(typing_challenge_state(&settings.typing_baseline))
}

/// Clear all three challenge results so the user can sit them again. Historical
/// Time Saved moves with the new baseline, because it was never stored.
#[tauri::command]
fn redo_typing_challenges(app: tauri::AppHandle) -> Result<TypingChallengeState, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::redo_typing_challenges(&mut settings.typing_baseline);
    save_current_settings(&app, &settings)?;

    notify_usage_changed(&app);
    Ok(typing_challenge_state(&settings.typing_baseline))
}

/// Open the Typing Challenge window, creating it on first use.
///
/// It is its own window and larger than Settings on purpose: thirty seconds of
/// typing against a passage needs room to read, and the 480x520 settings frame
/// would put the passage and the typing box in a column too narrow to follow.
#[tauri::command]
fn open_typing_challenge(app: tauri::AppHandle) -> Result<(), String> {
    // Raised before the window exists, so the hotkey is already inert by the
    // time the webview can steal focus and the user can start typing.
    app.state::<TypingChallengeOpen>().set(true);

    if let Some(window) = app.get_webview_window(TYPING_CHALLENGE_WINDOW) {
        window.show().map_err(|error| error.to_string())?;
        window.set_focus().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let built = tauri::WebviewWindowBuilder::new(
        &app,
        TYPING_CHALLENGE_WINDOW,
        tauri::WebviewUrl::App("typing-challenge.html".into()),
    )
    .title("Slugtale Typing Challenge")
    .inner_size(760.0, 620.0)
    .resizable(false)
    .build();

    match built {
        Ok(_) => Ok(()),
        Err(error) => {
            // The window never appeared, so the hotkey must work again.
            app.state::<TypingChallengeOpen>().set(false);
            Err(error.to_string())
        }
    }
}

#[tauri::command]
fn close_typing_challenge(app: tauri::AppHandle) -> Result<(), String> {
    app.state::<TypingChallengeOpen>().set(false);
    if let Some(window) = app.get_webview_window(TYPING_CHALLENGE_WINDOW) {
        window.close().map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Tell an open Usage pane that its numbers moved. Redoing the challenges shifts
/// every Time Saved on screen, so the pane cannot be left showing the old ones.
fn notify_usage_changed(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.emit("usage-changed", ());
    }
}

/// Whether the Typing Challenge window is on screen right now.
///
/// While it is, the dictation Hotkey does nothing at all (ADR-0025): the user is
/// typing a passage, and their hotkey is very likely inside it. Doing nothing —
/// rather than starting a dictation, or refusing with a notification — is what
/// keeps the thirty seconds being a measurement of typing.
fn typing_challenge_is_open(app: &tauri::AppHandle) -> bool {
    app.state::<TypingChallengeOpen>().get()
}

#[tauri::command]
fn get_local_model_status(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    Ok(model_manager(&app)?.status())
}

#[tauri::command]
async fn download_local_model(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    let manager = model_manager(&app)?;
    let status = tauri::async_runtime::spawn_blocking(move || {
        // Throttle IPC traffic: the initial update, then one per ~1 MB, plus
        // the final update (slugtale-dtl).
        let mut forward = slugtale_lib::throttled_progress(move |progress| {
            let _ = on_progress.send(progress);
        });
        manager
            .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    if status.present {
        warm_effective_primary_engine(&app);
    }
    Ok(status)
}

#[tauri::command]
fn delete_local_model(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    model_manager(&app)?
        .delete_default()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reveal_model_location(app: tauri::AppHandle) -> Result<(), String> {
    model_manager(&app)?
        .open_in_file_manager()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn transcribe_captured_audio(
    app: tauri::AppHandle,
    sample_rate_hz: u32,
    samples: Vec<f32>,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let diagnostic_log = current_diagnostic_log(&app, &settings);
    // Routed like the hotkey path through the same assembled stack, so a
    // dictation driven from the frontend gets the same engine stack and the
    // same second opinion as one driven from the hotkey. Two transcription
    // paths that disagreed would be a bug the user could only find by noticing
    // that one of them was worse.
    let stack = app
        .state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .dictation_stack(&settings, diagnostic_log.clone())
        .map_err(|error| error.to_string())?;
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    tauri::async_runtime::spawn_blocking(move || {
        let runtime = stack.asr_runtime();
        slugtale_lib::transcribe_captured_audio(&runtime, audio).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(Default)]
struct CurrentPlatform;

impl CurrentPlatform {
    fn new() -> Self {
        Self
    }

    #[cfg(target_os = "macos")]
    fn macos_platform(&self) -> slugtale_lib::MacosPlatform {
        slugtale_lib::MacosPlatform::new()
    }

    #[cfg(target_os = "windows")]
    fn windows_platform(&self) -> slugtale_lib::WindowsPlatform {
        slugtale_lib::WindowsPlatform::new()
    }

    #[cfg(target_os = "linux")]
    fn linux_platform(&self) -> slugtale_lib::LinuxPlatform {
        slugtale_lib::LinuxPlatform::new()
    }
}

fn app_files(app: &tauri::AppHandle) -> AppFiles {
    app.state::<AppFiles>().inner().clone()
}

fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app_files(app).settings_path()
}

fn model_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_files(app).model_dir()
}

fn model_manager(app: &tauri::AppHandle) -> Result<slugtale_lib::LocalModelManager, String> {
    let settings_path =
        settings_path(app).ok_or_else(|| "could not resolve settings path".to_string())?;
    Ok(slugtale_lib::LocalModelManager::new(
        model_dir(app)?,
        settings_path,
    ))
}

/// The Usage File (CONTEXT.md): a sibling of the Settings File, so opting out
/// deletes one obvious file and nothing else.
fn usage_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app_files(app).usage_path()
}

fn load_current_usage(app: &tauri::AppHandle) -> slugtale_lib::UsageFile {
    app_files(app).usage()
}

/// Which week the Usage pane means by "this week", asked of the OS rather than
/// assumed (ADR-0021: locale is platform behaviour).
fn locale_week_start(_app: &tauri::AppHandle) -> slugtale_lib::WeekStart {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        slugtale_lib::locale_week_start()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        slugtale_lib::WeekStart::default()
    }
}

/// Start the single worker that writes Daily Usage Records.
///
/// It is a worker rather than an inline write for one reason: a Counted Segment
/// has already reached the user's document by the time it is counted, so nothing
/// here may be allowed to delay the next segment or fail the dictation. Every
/// failure below is therefore a skip, not an error.
fn start_usage_worker(app: &tauri::AppHandle) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel::<UsageUpdate>();
    {
        let recorder = app.state::<UsageRecorder>();
        let mut queue = recorder
            .0
            .lock()
            .map_err(|_| "usage queue mutex poisoned".to_string())?;
        *queue = Some(sender);
    }

    let app = app.clone();
    std::thread::Builder::new()
        .name("slugtale-usage".to_string())
        .spawn(move || {
            while let Ok(update) = receiver.recv() {
                // The opt-in is checked here, at the last possible moment, so a
                // segment that was in flight when the user turned storing off
                // does not land in a file they just asked to be deleted.
                if !load_current_settings(&app).store_usage {
                    continue;
                }
                let Some(path) = usage_path(&app) else {
                    continue;
                };
                if let Some(parent) = path.parent() {
                    if std::fs::create_dir_all(parent).is_err() {
                        continue;
                    }
                }

                let mut usage = slugtale_lib::load_usage(&path);
                slugtale_lib::record_counted_segment(&mut usage, update.date, update.segment);
                if let Err(error) = slugtale_lib::save_usage(&path, &usage) {
                    eprintln!("could not write the usage file: {error}");
                    continue;
                }

                // The Usage pane is the only surface that shows any of this, so
                // it is the only thing told. Nothing reaches the Pill, the tray,
                // or a notification (ADR-0025).
                if let Some(window) = app.get_webview_window("settings") {
                    let _ = window.emit("usage-changed", ());
                }
            }
        })
        .map_err(|error| error.to_string())?;

    Ok(())
}

fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    app_files(app).settings()
}

fn current_diagnostic_log(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> SharedDiagnosticLog<FileDiagnosticSink> {
    app_files(app).diagnostic_log(settings.diagnostic_logging)
}

fn record_diagnostic_event(app: &tauri::AppHandle, event: slugtale_lib::DiagnosticEvent) {
    let settings = load_current_settings(app);
    current_diagnostic_log(app, &settings).record(event);
}

fn save_current_settings(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    app_files(app).save_settings(settings)
}

impl slugtale_lib::PlatformReadiness for CurrentPlatform {
    fn microphone_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().microphone_granted();
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_platform().microphone_granted();
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_platform().microphone_granted();
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }

    fn insertion_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().insertion_granted();
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_platform().insertion_granted();
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_platform().insertion_granted();
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }
}

fn main() {
    let reauthorize_permissions =
        slugtale_lib::permission_reauthorization_requested(std::env::args());
    let app = tauri::Builder::default()
        .manage(slugtale_lib::TranscriptionEngineCatalogue::default())
        .manage(RecordingFeedbackState::default())
        .manage(FocusTargetState::default())
        .manage(AudioCaptureState::default())
        .manage(DictationSegments::default())
        .manage(HotkeyRegistrationState::default())
        .manage(UsageRecorder::default())
        .manage(TypingChallengeOpen::default())
        .manage(voice_activation::VoiceActivationState::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // Every local file path resolves through this one store, so it has
            // to exist before anything that reads or writes a file.
            app.manage(AppFiles::from_app(app.handle()));
            slugtale_lib::setup_tray(app)?;
            setup_configured_hotkey(app)?;
            // The Dictation Segment worker outlives every dictation: it is what
            // keeps segments landing in the order they were spoken.
            start_dictation_segment_worker(app.handle()).map_err(std::io::Error::other)?;
            // Usage writes happen off the Dictation Workflow path (ADR-0025), so
            // the queue that carries them has to exist before the first segment.
            start_usage_worker(app.handle()).map_err(std::io::Error::other)?;
            // Reconcile the OS login item with the stored preference so a moved or
            // rebuilt app (dev binaries change path) does not drift out of sync.
            let settings = load_current_settings(app.handle());
            let _ = set_launch_at_login_state(app.handle(), settings.launch_at_login);
            if let Ok(model_dir) = model_dir(app.handle()) {
                app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                    .set_model_dir(model_dir);
            }
            warm_effective_primary_engine(app.handle());
            // Prepare Audio Capture while idle so the first Hotkey does not pay
            // for device discovery and ring allocation (slugtale-g1o.3). Only
            // when the microphone permission is already granted: preparation
            // must never prompt, and a denied microphone stays on the normal
            // permission path.
            if slugtale_lib::PlatformReadiness::microphone_granted(&CurrentPlatform::new()) {
                if let Ok(mut capture) = app.state::<AudioCaptureState>().0.lock() {
                    let _ = slugtale_lib::AudioRecorder::prepare(capture.recorder_mut());
                }
            }
            // Voice Activation is opt-in: the always-on listener only starts
            // when a previously saved preference asks for it (slugtale-e95).
            if let Err(error) = voice_activation::sync_worker(
                app.handle(),
                load_current_settings(app.handle()).voice_activation_enabled,
            ) {
                eprintln!("voice activation worker did not start: {error}");
            }
            if reauthorize_permissions {
                slugtale_lib::show_settings(app.handle().clone());
                #[cfg(target_os = "macos")]
                slugtale_lib::request_microphone_access().map_err(std::io::Error::other)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if slugtale_lib::hides_on_close(window.label()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            // The Typing Challenge window can also be closed from its title bar,
            // which never reaches the close command. Either way, the hotkey has
            // to start working again the moment the window goes.
            if window.label() == TYPING_CHALLENGE_WINDOW
                && matches!(
                    event,
                    tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                )
            {
                window.state::<TypingChallengeOpen>().set(false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            show_settings,
            get_settings_readiness,
            get_settings,
            dictation_bar_displays,
            open_microphone_settings,
            open_text_insertion_settings,
            save_hotkey_settings,
            save_transcription_settings,
            save_transcript_cleanup_settings,
            voice_activation_supported,
            save_voice_activation_settings,
            save_dictation_bar_settings,
            dictation_bar_pointer_over,
            save_launch_at_login,
            check_for_app_update,
            install_app_update,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcription_engines,
            set_transcription_engines,
            install_engine_assets,
            remove_engine_assets,
            transcribe_captured_audio,
            dictation_event,
            get_usage_summary,
            set_usage_storing,
            set_typing_estimate,
            get_typing_challenge,
            submit_typing_challenge,
            redo_typing_challenges,
            open_typing_challenge,
            close_typing_challenge
        ])
        .build(tauri::generate_context!())
        .expect("error while building Slugtale");

    // `App::run` terminates with `process::exit`, which skips Rust destructors.
    // Use the returning event loop and explicitly quiesce/drop Whisper first so
    // ggml's C++ Metal globals never tear down around live resources (p1u).
    let exit_code = app.run_return(|app, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .shutdown();
        }
    });

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
