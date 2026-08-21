#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

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

/// One unit of work for the Dictation Segment pipeline.
///
/// A Segment Pause sends a *request* rather than the audio itself, and that is
/// deliberate. The request is raised from the recorder's own level-emitter
/// thread, which `CpalAudioRecorder::stop` joins while holding the
/// [`AudioCaptureState`] lock — so if that thread ever blocked on the same lock
/// to drain the ring, pressing Stop mid-flush would deadlock the app. Draining
/// on the worker instead keeps the emitter thread free of that lock entirely.
enum DictationSegmentJob {
    /// A Segment Pause elapsed: take whatever has been captured so far.
    PauseFlush { dictation: u64 },
    /// The dictation ended. Carries the audio left over after the last Segment
    /// Pause, already drained by the Stop path.
    Last {
        dictation: u64,
        audio: slugtale_lib::CapturedAudio,
    },
}

/// The ordered pipeline that turns Dictation Segments into inserted text.
///
/// A single worker thread drains the queue, and that is the whole of the
/// ordering guarantee: however long any one segment takes to decode, the text
/// lands in the order it was spoken.
#[derive(Default)]
struct DictationSegments {
    /// `None` until the worker starts. Held behind a mutex because an mpsc
    /// `Sender` is `Send` but not `Sync`, and Tauri state must be both.
    jobs: Mutex<Option<std::sync::mpsc::Sender<DictationSegmentJob>>>,
    /// Incremented on every Start, so each dictation's segments are
    /// distinguishable from the previous dictation's still-decoding tail.
    dictation: std::sync::atomic::AtomicU64,
    /// Every dictation at or below this number has been cancelled. Escape
    /// discards the remainder of a dictation, including segments already queued
    /// but not yet inserted; text that has already landed stays where it is,
    /// because Slugtale cannot un-type it (ADR-0014).
    cancelled_through: std::sync::atomic::AtomicU64,
    /// Set when a segment fell back to the Insertion Rescue. Further Segment
    /// Pauses are held back for the rest of that dictation: without this, a
    /// machine that has not granted Accessibility would clobber the clipboard
    /// and raise a notification once every five seconds.
    rescued: std::sync::atomic::AtomicBool,
}

impl DictationSegments {
    fn current(&self) -> u64 {
        self.dictation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Open a new dictation and return its number.
    fn begin(&self) -> u64 {
        self.rescued
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.dictation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// Abandon the active dictation's un-inserted remainder.
    fn abandon(&self) {
        self.cancelled_through
            .store(self.current(), std::sync::atomic::Ordering::SeqCst);
    }

    fn is_cancelled(&self, dictation: u64) -> bool {
        dictation
            <= self
                .cancelled_through
                .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether `dictation` is still the one being recorded. A Pause Flush is
    /// only honoured while this holds: draining the ring for a dictation that
    /// has already ended would take the *next* dictation's opening words.
    fn is_recording(&self, dictation: u64) -> bool {
        self.current() == dictation && !self.is_cancelled(dictation)
    }

    fn suspend_pause_flushes(&self) {
        self.rescued
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn pause_flushes_suspended(&self) -> bool {
        self.rescued.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Queue a job, reporting whether the worker accepted it.
    fn send(&self, job: DictationSegmentJob) -> bool {
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
    DiagnosticAsrRuntime, DiagnosticInsertionRescue, DiagnosticTextInsertion, FileDiagnosticSink,
    SharedDiagnosticLog, TranscriptionProvider,
};

/// The Transcription Engines that outlive a single dictation.
///
/// Whisper is deliberately absent: its runtime is keyed by the active model
/// path, which the user can change from Settings, so it comes from
/// [`slugtale_lib::WhisperRuntimeCache`] per dictation instead. Parakeet and
/// Apple SpeechTranscriber own no such per-dictation state, and building them
/// once is what lets them answer [`TranscriptionProvider::availability`] from a
/// cached probe rather than a filesystem or OS query on every dictation.
struct TranscriptionEngines {
    parakeet: Arc<slugtale_lib::ParakeetProvider>,
}

impl TranscriptionEngines {
    fn new(model_dir: &std::path::Path) -> Self {
        Self {
            parakeet: Arc::new(slugtale_lib::ParakeetProvider::new(
                slugtale_lib::parakeet_asset_dir(model_dir),
            )),
        }
    }

    /// The provider for one engine, or `None` for Whisper, which the caller
    /// supplies from the model-path-keyed cache.
    fn provider(
        &self,
        engine: slugtale_lib::TranscriptionEngine,
    ) -> Option<Arc<dyn TranscriptionProvider>> {
        match engine {
            slugtale_lib::TranscriptionEngine::Whisper => None,
            slugtale_lib::TranscriptionEngine::Parakeet => Some(self.parakeet.clone()),
            // Apple SpeechTranscriber lives in AppleSpeechEngineState, which is
            // managed unconditionally because it needs no models directory.
            slugtale_lib::TranscriptionEngine::AppleSpeech => None,
        }
    }
}

/// The Apple SpeechTranscriber provider, shared by Settings and the dictation
/// path.
///
/// Held separately from [`TranscriptionEngines`] because it needs no models
/// directory: Apple's assets belong to macOS, so this provider is available even
/// on a machine where Slugtale could not resolve its own app data directory.
///
/// There is exactly one of these, and that is the point. The provider caches its
/// own availability probe, so a second instance would mean Settings installing
/// the system assets while the router went on reading a stale "not installed"
/// from its own copy.
struct AppleSpeechEngineState(Arc<slugtale_lib::AppleSpeechProvider>);

impl Default for AppleSpeechEngineState {
    fn default() -> Self {
        Self(Arc::new(slugtale_lib::AppleSpeechProvider::new()))
    }
}

/// Resolve every Transcription Engine to the one managed instance of its
/// provider, so that installing assets from Settings is visible immediately.
/// Two instances would each cache their own availability probe, and the router
/// would keep reading "not installed" after the user had just installed it.
///
/// Whisper is passed in rather than looked up because its runtime is keyed by
/// the active model path (see [`TranscriptionEngines`]); `None` there means the
/// models directory could not be resolved, which is itself a Whisper that cannot
/// run. Returning `None` for any engine means this build registered no provider
/// for it at all.
fn engine_resolver(
    app: &tauri::AppHandle,
    whisper_provider: Option<Arc<dyn TranscriptionProvider>>,
) -> impl Fn(slugtale_lib::TranscriptionEngine) -> Option<Arc<dyn TranscriptionProvider>> + '_ {
    let engines = app.try_state::<TranscriptionEngines>();
    let apple = app.try_state::<AppleSpeechEngineState>();

    move |engine| match engine {
        slugtale_lib::TranscriptionEngine::Whisper => whisper_provider.clone(),
        slugtale_lib::TranscriptionEngine::Parakeet => engines
            .as_ref()
            .and_then(|engines| engines.provider(engine)),
        slugtale_lib::TranscriptionEngine::AppleSpeech => apple
            .as_ref()
            .map(|apple| apple.0.clone() as Arc<dyn TranscriptionProvider>),
    }
}

/// What every Transcription Engine reports about itself right now, in
/// [`slugtale_lib::TranscriptionEngine::ALL`] order.
///
/// Both the readiness report and the dictation router read availability through
/// here so they cannot disagree: Settings saying "ready" while the router picks
/// an engine that fails at transcription is exactly the bug this closes
/// (slugtale-bre). Cheap enough for the hotkey path — every provider answers
/// from a cached probe rather than re-examining the machine.
fn engine_availability(
    resolve: &impl Fn(slugtale_lib::TranscriptionEngine) -> Option<Arc<dyn TranscriptionProvider>>,
) -> Vec<(
    slugtale_lib::TranscriptionEngine,
    slugtale_lib::EngineAvailability,
)> {
    slugtale_lib::TranscriptionEngine::ALL
        .into_iter()
        .filter_map(|engine| resolve(engine).map(|provider| (engine, provider.availability())))
        .collect()
}

/// Assemble the engine stack for one dictation from the Settings File.
///
/// Two fallbacks here are worth stating plainly, because both trade the user's
/// stated preference for finishing the dictation:
///
/// - A primary engine whose assets were deleted since it was chosen falls back
///   to whichever engine can actually run
///   ([`slugtale_lib::engine_that_can_run`]). Refusing to transcribe would
///   punish the user for a setting they may not remember making — but falling
///   back to Whisper unconditionally was worse, because a build without
///   `local-whisper-runtime` has no Whisper to fall back to (slugtale-bre).
/// - The second opinion is whichever *available* engine is not the primary, in
///   the fixed [`slugtale_lib::TranscriptionEngine::ALL`] order. There is no
///   setting for it because benchmark slugtale-9dv has not yet established
///   which pairing is worth offering.
fn transcription_router(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
    whisper: Arc<slugtale_lib::LocalWhisperRuntime>,
    diagnostic_log: SharedDiagnosticLog<FileDiagnosticSink>,
) -> slugtale_lib::SecondOpinionRouter {
    let whisper_provider: Arc<dyn TranscriptionProvider> =
        Arc::new(slugtale_lib::WhisperTranscriptionProvider::new(whisper));
    let resolve = engine_resolver(app, Some(whisper_provider.clone()));

    let primary =
        slugtale_lib::engine_that_can_run(settings.primary_engine, &engine_availability(&resolve))
            .and_then(&resolve)
            // Nothing on this machine can transcribe. Keep Whisper so the
            // dictation fails with the reason readiness already showed the user,
            // rather than leaving the router with no engine at all.
            .unwrap_or_else(|| whisper_provider.clone());

    let router = match settings.second_opinion {
        slugtale_lib::SecondOpinionMode::Off => slugtale_lib::SecondOpinionRouter::single(primary),
        slugtale_lib::SecondOpinionMode::Automatic => {
            let second = slugtale_lib::TranscriptionEngine::ALL
                .into_iter()
                .filter(|engine| *engine != primary.engine())
                .filter_map(resolve)
                .find(|provider| provider.availability().is_available());

            match second {
                Some(second) => slugtale_lib::SecondOpinionRouter::new(
                    primary,
                    second,
                    slugtale_lib::SecondOpinionMode::Automatic,
                ),
                None => slugtale_lib::SecondOpinionRouter::single(primary),
            }
        }
    };

    router.observing(move |routing| {
        diagnostic_log.record(slugtale_lib::DiagnosticEvent::routing_decision(routing))
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistrationState(Mutex<HotkeyRegistration>);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistration {
    current_hotkey: Option<String>,
    lifecycle: Option<slugtale_lib::DictationLifecycle>,
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
        "stop" => slugtale_lib::DictationEvent::Stop,
        "cancel" => return cancel_active_dictation(&app),
        other => return Err(format!("unknown dictation event: {other}")),
    };

    handle_dictation_event(&app, event)
}

/// Cancel through the same lifecycle bridge used by the global Escape handler
/// so a click on the Dictation Bar cannot leave toggle/hold state believing a
/// discarded dictation is still active.
fn cancel_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let event = {
            let state = app.state::<HotkeyRegistrationState>();
            let mut registration = state
                .0
                .lock()
                .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
            let event = registration
                .lifecycle
                .as_mut()
                .and_then(slugtale_lib::DictationLifecycle::cancel);
            if event.is_some() {
                request_escape_registration(&registration, false);
            }
            event
        };
        if let Some(event) = event {
            return handle_dictation_event(app, event);
        }
    }

    handle_dictation_event(app, slugtale_lib::DictationEvent::Cancel)
}

fn handle_dictation_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
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
            apply_recording_feedback(app, event)?;
        }
        // Stop plays its cue but leaves the bar on screen: the audio-capture step
        // switches it to a transcribing state and hides it once the workflow
        // finishes, so the user sees the model working (slugtale-0t4).
        slugtale_lib::DictationEvent::Stop => {
            advance_recording_feedback(app, event)?;
            handle_audio_capture_event(app, event)?;
        }
        // Cancel clears the bar immediately and discards the audio. It also
        // drops any Dictation Segment still queued, so nothing further is typed
        // after the user asks Slugtale to stop. Text inserted by an earlier
        // Segment Pause is not undone (ADR-0014).
        slugtale_lib::DictationEvent::Cancel => {
            app.state::<DictationSegments>().abandon();
            apply_recording_feedback(app, event)?;
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
) -> Result<(), String> {
    let effect = advance_recording_feedback(app, event)?;

    if effect.bar_visible {
        show_dictation_bar(app, DictationPhase::Recording);
    } else {
        hide_dictation_bar(app);
    }

    Ok(())
}

fn capture_focus_target(app: &tauri::AppHandle) {
    let _ = app;
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let pid = slugtale_lib::frontmost_app_pid();
        if let Ok(mut guard) = app.state::<FocusTargetState>().0.lock() {
            *guard = pid;
        }
    }
}

fn handle_audio_capture_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
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
            show_dictation_bar(app, DictationPhase::Transcribing);
            let segments = app.state::<DictationSegments>();
            let queued = segments.send(DictationSegmentJob::Last {
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
/// takes only its own detector lock and hands the queue a request rather than
/// touching the audio session.
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

    segments.send(DictationSegmentJob::PauseFlush {
        dictation: segments.current(),
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

fn warm_ready_local_whisper_runtime(app: &tauri::AppHandle) -> Result<(), String> {
    let settings = load_current_settings(app);
    let model_path = model_manager(app)?.active_model_path(&settings);
    warm_local_whisper_runtime(app, &model_path);
    Ok(())
}

fn warm_local_whisper_runtime(app: &tauri::AppHandle, model_path: &std::path::Path) {
    let cache = app.state::<slugtale_lib::WhisperRuntimeCache>();
    if let Some(runtime) = cache.begin_warming_existing_model(model_path) {
        tauri::async_runtime::spawn_blocking(move || {
            let _ = runtime.warm_up();
        });
    }
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
    let model_path = settings
        .model
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(app)?));
    let whisper = app
        .state::<slugtale_lib::WhisperRuntimeCache>()
        .runtime_for(&model_path);
    // Apply the current Transcription Speed Profile before decoding so the user's
    // accuracy/speed choice takes effect without reloading the model.
    whisper.set_speed_profile(settings.speed_profile);
    let runtime = transcription_router(app, &settings, whisper, diagnostic_log.clone());
    let target_pid = app
        .state::<FocusTargetState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| *guard);

    {
        // Bring the user's app back to the front so synthesized keystrokes land
        // in its focused field rather than wherever focus drifted (slugtale-squ).
        // This repeats for every segment, which is what makes a Pause Flush
        // behave exactly like the single insertion it replaces.
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        if let Some(pid) = target_pid {
            if slugtale_lib::activate_app(pid) {
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }

        #[cfg(target_os = "macos")]
        {
            // Without Accessibility trust every synthesized event is silently
            // dropped; insertion falls back to the clipboard rescue, so tell the
            // user how to fix it permanently (slugtale-avo).
            if !slugtale_lib::accessibility_trusted() {
                let _ = slugtale_lib::notify(
                    "Slugtale needs Accessibility access",
                    "Turn on Slugtale under System Settings \u{2192} Privacy & Security \u{2192} \
                     Accessibility so it can type into other apps. Until then your transcription \
                     is copied to the clipboard \u{2014} paste it with Cmd+V.",
                );
            }

            let insertion = slugtale_lib::MacosTextInsertion::new();
            let rescue = slugtale_lib::MacosInsertionRescue::new();
            let runtime = DiagnosticAsrRuntime::new(&runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(
                &runtime,
                &insertion,
                &rescue,
                settings.transcript_cleanup,
            );
            workflow
                .complete(audio, position)
                .map_err(|error| error.to_string())
        }

        #[cfg(target_os = "windows")]
        {
            let insertion = slugtale_lib::WindowsTextInsertion::new();
            let rescue = slugtale_lib::WindowsInsertionRescue::new();
            let runtime = DiagnosticAsrRuntime::new(&runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(
                &runtime,
                &insertion,
                &rescue,
                settings.transcript_cleanup,
            );
            workflow
                .complete(audio, position)
                .map_err(|error| error.to_string())
        }

        #[cfg(target_os = "linux")]
        {
            // On a Wayland session synthesized input is not yet supported; the
            // insertion still runs and falls through to the clipboard rescue,
            // but tell the user why so their transcription is not silently lost
            // (mirrors the macOS Accessibility notice above).
            if !slugtale_lib::detect_session().is_supported() {
                let _ = slugtale_lib::notify(
                    "Slugtale needs an X11 session",
                    "Slugtale currently types into other apps only on an X11 session. Until you \
                     switch to X11 your transcription is copied to the clipboard \u{2014} paste it \
                     with Ctrl+V.",
                );
            }

            let insertion = slugtale_lib::LinuxTextInsertion::new();
            let rescue = slugtale_lib::LinuxInsertionRescue::new();
            let runtime = DiagnosticAsrRuntime::new(&runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(
                &runtime,
                &insertion,
                &rescue,
                settings.transcript_cleanup,
            );
            workflow
                .complete(audio, position)
                .map_err(|error| error.to_string())
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = (runtime, audio, target_pid, position);
            Err("text insertion is not implemented for this platform".to_string())
        }
    }
}

/// Take the speech captured so far as a Dictation Segment, leaving the
/// microphone running. Called only from the worker thread.
fn take_dictation_segment(app: &tauri::AppHandle) -> Option<slugtale_lib::CapturedAudio> {
    let capture = app.state::<AudioCaptureState>();
    let flushed = capture
        .0
        .lock()
        .map_err(|_| "audio capture mutex poisoned".to_string())
        .and_then(|mut guard| guard.flush_segment().map_err(|error| error.to_string()));

    match flushed {
        Ok(audio) => audio,
        Err(error) => {
            eprintln!("could not take dictation segment: {error}");
            None
        }
    }
}

/// Start the single worker that transcribes and inserts Dictation Segments.
///
/// Segments are decoded one at a time on purpose. Whisper would happily be
/// asked for two at once, but then a short segment could overtake a long one and
/// the user's words would land out of order — so the queue is the ordering
/// guarantee, and the cost is that a slow segment delays the next.
fn start_dictation_segment_worker(app: &tauri::AppHandle) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::channel::<DictationSegmentJob>();
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
            // Which dictation the worker is part-way through, and whether it has
            // put anything into the text target yet. The second answers only one
            // question — whether the next segment opens the text or appends to
            // it — and it has to be the worker's, because only the worker knows
            // that an earlier segment transcribed to nothing.
            let mut dictation = 0u64;
            let mut inserted_any = false;

            while let Ok(job) = receiver.recv() {
                let segments = app.state::<DictationSegments>();
                let (number, last) = match &job {
                    DictationSegmentJob::PauseFlush { dictation } => (*dictation, false),
                    DictationSegmentJob::Last { dictation, .. } => (*dictation, true),
                };

                if number != dictation {
                    dictation = number;
                    inserted_any = false;
                }

                let audio = match job {
                    // A Pause Flush is honoured only while its dictation is
                    // still recording. If Stop already drained the ring, this
                    // finds nothing and skips — nothing is lost, because Stop
                    // took the same audio into the last segment.
                    DictationSegmentJob::PauseFlush { .. } => segments
                        .is_recording(number)
                        .then(|| take_dictation_segment(&app))
                        .flatten(),
                    DictationSegmentJob::Last { audio, .. } => {
                        (!segments.is_cancelled(number)).then_some(audio)
                    }
                };

                if let Some(audio) = audio {
                    let position = if inserted_any {
                        slugtale_lib::DictationSegmentPosition::Continuation
                    } else {
                        slugtale_lib::DictationSegmentPosition::First
                    };
                    // Read the speaking duration before the audio is handed to
                    // the workflow, which consumes it.
                    let speaking_seconds = if audio.sample_rate_hz > 0 {
                        audio.samples.len() as f64 / f64::from(audio.sample_rate_hz)
                    } else {
                        0.0
                    };
                    // Whether this segment opens the dictation is the same
                    // question `position` already answered, and it is what makes
                    // Usage count dictations rather than Pause Flushes.
                    let starts_dictation = !inserted_any;
                    // One worker serves every dictation for the life of the app,
                    // so a panic here would silently disable insertion from now
                    // on rather than spoiling a single dictation. Contain it.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_dictation_segment(&app, audio, position)
                    }));
                    match outcome {
                        Ok(Ok(outcome)) => {
                            if outcome.inserted {
                                eprintln!(
                                    "inserted dictation segment: {} chars",
                                    outcome.transcription.text.chars().count()
                                );
                                // A Counted Segment is one that reached the text
                                // target, whether by insertion or by the rescue
                                // (ADR-0025) — `inserted` is true for both. A
                                // segment that heard nothing, and a segment whose
                                // rescue also failed, never get here, which is
                                // exactly the rule the design asked for.
                                app.state::<UsageRecorder>().record(UsageUpdate {
                                    date: slugtale_lib::today_local(),
                                    segment: slugtale_lib::CountedSegment {
                                        words: slugtale_lib::count_words(
                                            &outcome.transcription.text,
                                        ),
                                        speaking_seconds,
                                        starts_dictation,
                                    },
                                });
                            } else {
                                eprintln!("dictation segment heard nothing; inserted nothing");
                            }
                            inserted_any |= outcome.inserted;
                            if outcome.rescued {
                                segments.suspend_pause_flushes();
                            }
                        }
                        Ok(Err(error)) => eprintln!("dictation workflow failed: {error}"),
                        Err(_) => {
                            eprintln!("dictation segment panicked; the queue stays open")
                        }
                    }
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
                        if let Err(error) = sync_escape_registration(
                            &app,
                            &mut escape_registered,
                            should_register,
                        ) {
                            eprintln!("could not update global Escape key: {error}");
                        }
                    }
                    GlobalKeyCommand::Input(key, input) => {
                        // The Typing Challenge measures how fast the user types,
                        // so their hotkey has to be plain text for those thirty
                        // seconds. Swallow it here, before any lifecycle state
                        // moves, so releasing it later cannot resume anything.
                        if typing_challenge_is_open(&app) {
                            continue;
                        }

                        let starts_dictation = matches!(
                            (key, input),
                            (
                                slugtale_lib::DictationKey::Hotkey,
                                slugtale_lib::HotkeyInput::Pressed
                            )
                        ) && app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .and_then(|registration| {
                                registration
                                    .lifecycle
                                    .as_ref()
                                    .map(|lifecycle| !lifecycle.is_dictating())
                            })
                            .unwrap_or(false);

                        if starts_dictation && !hotkey_dictation_is_ready(&app) {
                            continue;
                        }

                        if starts_dictation
                            && sync_escape_registration(&app, &mut escape_registered, true)
                                .is_err()
                        {
                            eprintln!(
                                "dictation did not start because global Escape could not be registered"
                            );
                            continue;
                        }

                        let transition = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .and_then(|mut registration| {
                                registration.lifecycle.as_mut().map(|lifecycle| {
                                    let event = match (key, input) {
                                        (slugtale_lib::DictationKey::Hotkey, input) => {
                                            lifecycle.on_hotkey(input)
                                        }
                                        (
                                            slugtale_lib::DictationKey::Escape,
                                            slugtale_lib::HotkeyInput::Pressed,
                                        ) => lifecycle.cancel(),
                                        (
                                            slugtale_lib::DictationKey::Escape,
                                            slugtale_lib::HotkeyInput::Released,
                                        ) => None,
                                    };
                                    (event, lifecycle.is_dictating())
                                })
                            });
                        if let Some((event, should_register)) = transition {
                            // The shared registration mutex is no longer held:
                            // recording, transcription, and window work may block
                            // without preventing the main-thread shortcut handler
                            // from forwarding the next key transition (slugtale-pil).
                            if let Some(event) = event {
                                if let Err(error) = handle_dictation_event(&app, event) {
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
    registration.lifecycle = settings
        .hotkey
        .as_ref()
        .map(|_| slugtale_lib::DictationLifecycle::new(settings.activation_mode));
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

fn show_dictation_bar(app: &tauri::AppHandle, phase: DictationPhase) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let settings = load_current_settings(app);
        let appearance = DictationBarAppearance::from_settings(&settings);
        let bar_display = settings.bar_display;
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
    let local_model_ready = model_manager(app)
        .map(|manager| manager.ready())
        .unwrap_or_else(|_| {
            settings
                .model
                .as_ref()
                .is_some_and(|path| PathBuf::from(path).exists())
        });
    slugtale_lib::settings_readiness_report(
        &settings,
        &platform,
        local_model_ready,
        &current_engine_availability(app, &settings),
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
    let whisper = whisper_engine_provider(app, settings)
        .ok()
        .map(|provider| Arc::new(provider) as Arc<dyn TranscriptionProvider>);

    engine_availability(&engine_resolver(app, whisper))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn hotkey_dictation_is_ready(app: &tauri::AppHandle) -> bool {
    let report = current_settings_readiness(app);
    if report.dictation_available {
        return true;
    }

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
    false
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
        let _ = warm_ready_local_whisper_runtime(&app);
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
) -> Result<slugtale_lib::WhisperTranscriptionProvider, String> {
    let model_path = model_manager(app)?.active_model_path(settings);
    let runtime = app
        .state::<slugtale_lib::WhisperRuntimeCache>()
        .runtime_for(&model_path);
    Ok(slugtale_lib::WhisperTranscriptionProvider::new(runtime))
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
            let engines = app
                .try_state::<TranscriptionEngines>()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            let provider = &engines.parakeet;
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
            let provider = app.state::<AppleSpeechEngineState>().0.clone();
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
                let mut last_sent = 0u64;
                let mut forward = move |progress: slugtale_lib::DownloadProgress| {
                    let complete = progress
                        .total
                        .is_some_and(|total| progress.downloaded >= total);
                    if progress.downloaded == 0
                        || complete
                        || progress.downloaded - last_sent >= 1_048_576
                    {
                        last_sent = progress.downloaded;
                        let _ = on_progress.send(progress);
                    }
                };
                manager
                    .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            if status.present {
                warm_local_whisper_runtime(&app, &status.path);
            }
        }
        slugtale_lib::TranscriptionEngine::Parakeet => {
            let engines = app
                .try_state::<TranscriptionEngines>()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            let provider = engines.parakeet.clone();
            let asset_dir = provider.asset_dir().to_path_buf();
            tauri::async_runtime::spawn_blocking(move || {
                let mut last_sent = 0u64;
                let mut forward = move |progress: slugtale_lib::DownloadProgress| {
                    let complete = progress
                        .total
                        .is_some_and(|total| progress.downloaded >= total);
                    if progress.downloaded == 0
                        || complete
                        || progress.downloaded - last_sent >= 1_048_576
                    {
                        last_sent = progress.downloaded;
                        let _ = on_progress.send(progress);
                    }
                };
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
            let provider = app.state::<AppleSpeechEngineState>().0.clone();
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
            let engines = app
                .try_state::<TranscriptionEngines>()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            slugtale_lib::delete_parakeet_assets(engines.parakeet.asset_dir())
                .map_err(|error| error.to_string())?;
            engines.parakeet.refresh_availability();
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
        today: usage_span(&slugtale_lib::totals_for_day(&usage, today), words_per_minute),
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
fn set_typing_estimate(app: tauri::AppHandle, estimate: Option<u32>) -> Result<UsageSummary, String> {
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
        // Throttle IPC traffic: forward progress after ~1 MB of new data, plus
        // the initial and final updates, so the bar stays smooth without
        // flooding the channel with thousands of tiny messages.
        let mut last_sent = 0u64;
        let mut forward = move |progress: slugtale_lib::DownloadProgress| {
            let complete = progress
                .total
                .is_some_and(|total| progress.downloaded >= total);
            if progress.downloaded == 0 || complete || progress.downloaded - last_sent >= 1_048_576
            {
                last_sent = progress.downloaded;
                let _ = on_progress.send(progress);
            }
        };
        manager
            .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    if status.present {
        warm_local_whisper_runtime(&app, &status.path);
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
    cache: tauri::State<'_, slugtale_lib::WhisperRuntimeCache>,
    sample_rate_hz: u32,
    samples: Vec<f32>,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let model_path = model_manager(&app)?.active_model_path(&settings);
    let whisper = cache.runtime_for(&model_path);
    whisper.set_speed_profile(settings.speed_profile);
    let diagnostic_log = current_diagnostic_log(&app, &settings);
    // Routed like the hotkey path, so a dictation driven from the frontend gets
    // the same engine stack and the same second opinion as one driven from the
    // hotkey. Two transcription paths that disagreed would be a bug the user
    // could only find by noticing that one of them was worse.
    let runtime = transcription_router(&app, &settings, whisper, diagnostic_log.clone());
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    tauri::async_runtime::spawn_blocking(move || {
        let runtime = DiagnosticAsrRuntime::new(&runtime, diagnostic_log);
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

fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

fn model_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("models"))
        .map_err(|error| error.to_string())
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
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("usage.json"))
}

fn load_current_usage(app: &tauri::AppHandle) -> slugtale_lib::UsageFile {
    usage_path(app)
        .map(|path| slugtale_lib::load_usage(&path))
        .unwrap_or_default()
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

fn diagnostic_log_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("diagnostics.log"))
}

fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    settings_path(app)
        .map(|path| slugtale_lib::load_settings(&path))
        .unwrap_or_default()
}

fn current_diagnostic_log(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> SharedDiagnosticLog<FileDiagnosticSink> {
    let sink = diagnostic_log_path(app)
        .map(FileDiagnosticSink::new)
        .unwrap_or_else(FileDiagnosticSink::unavailable);
    SharedDiagnosticLog::new(settings.diagnostic_logging, sink)
}

fn record_diagnostic_event(app: &tauri::AppHandle, event: slugtale_lib::DiagnosticEvent) {
    let settings = load_current_settings(app);
    current_diagnostic_log(app, &settings).record(event);
}

fn save_current_settings(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    let path = settings_path(app).ok_or_else(|| "could not resolve settings path".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    slugtale_lib::save_settings(&path, settings).map_err(|error| error.to_string())
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
        .manage(slugtale_lib::WhisperRuntimeCache::default())
        .manage(RecordingFeedbackState::default())
        .manage(FocusTargetState::default())
        .manage(AudioCaptureState::default())
        .manage(DictationSegments::default())
        .manage(HotkeyRegistrationState::default())
        .manage(AppleSpeechEngineState::default())
        .manage(UsageRecorder::default())
        .manage(TypingChallengeOpen::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
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
            // Register the long-lived Transcription Engines once, so their
            // availability is a cached answer rather than a filesystem probe on
            // every dictation.
            if let Ok(model_dir) = model_dir(app.handle()) {
                app.manage(TranscriptionEngines::new(&model_dir));
            }
            let _ = warm_ready_local_whisper_runtime(app.handle());
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
            save_dictation_bar_settings,
            dictation_bar_pointer_over,
            save_launch_at_login,
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
            app.state::<slugtale_lib::WhisperRuntimeCache>().shutdown();
        }
    });

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
