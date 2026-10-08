//! The dictation lifecycle host: everything that happens between an activation
//! input saying "start" and the Dictation Runtime receiving the captured audio.
//! It owns the recording-feedback state machine, the focus target, the audio
//! capture session, and the runtime handle, and it reaches the rest of the app
//! only through [`DictationSurface`] — one port implemented once by the Tauri
//! shell, and by a fake in tests.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// What the Dictation Bar is currently doing, sent to its frontend so it can show
/// the matching state. The bar stays on screen through transcription (slugtale-0t4).
#[derive(Clone, Copy)]
pub enum DictationPhase {
    Recording,
    Transcribing,
}

impl DictationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            DictationPhase::Recording => "recording",
            DictationPhase::Transcribing => "transcribing",
        }
    }
}

/// Everything one dictation pinned when it began: the session number its
/// Dictation Segments carry, the Settings they use, and the app their words
/// belong to.
///
/// A job carries only the session number, so pinning the other two here is what
/// stops a later Start — or a Settings change the user made while the model was
/// working — from reaching into a job that is already running. Before this, each
/// segment re-read both, so a slow decode could land its words in whichever app
/// the user had since switched to (slugtale-cbxb, slugtale-7lq0).
#[derive(Clone)]
struct DictationSession {
    session: u64,
    settings: crate::Settings,
    target_pid: Option<i32>,
}

/// Everything the dictation lifecycle needs from the rest of the app: Settings
/// reads, diagnostics, the Dictation Bar surface, and failure notifications.
/// Every method is named for a dictation effect, not for a transport detail,
/// so the implementation stays replaceable and the tests stay honest about
/// what the lifecycle actually asked for.
pub trait DictationSurface: Send + Sync {
    fn settings(&self) -> crate::Settings;
    fn record_diagnostic_event(&self, event: crate::DiagnosticEvent);
    fn show_dictation_bar(&self, phase: DictationPhase, settings: &crate::Settings);
    fn hide_dictation_bar(&self);
    fn emit_dictation_audio_level(&self, level: f32);
    fn notify_capture_failure(&self, error: &str);
    fn play_dictation_sound(&self, sound: crate::DictationSound);
    fn diagnostic_log(
        &self,
        settings: &crate::Settings,
    ) -> crate::SharedDiagnosticLog<crate::FileDiagnosticSink>;
    fn dictation_stack(
        &self,
        settings: &crate::Settings,
    ) -> Result<crate::DictationStack<crate::FileDiagnosticSink>, String>;
    /// The Text Insertion and Insertion Rescue for one Dictation Segment, aimed
    /// at `target_pid`. Focus restoration repeats for every Segment Pause
    /// (ADR-0015), so the pair is asked for per segment rather than once per
    /// dictation.
    fn prepared_insertion(
        &self,
        target_pid: Option<i32>,
    ) -> Result<crate::PreparedInsertion, String>;
}
/// The dictation lifecycle's one owner of state: recording feedback, the focus
/// target, the audio capture session, the pinned dictation session, and the
/// runtime handle. The locks are private so the ordering rules stay inside this
/// module. No method reaches a [`DictationSurface`] while holding one.
/// [`Self::prepare_capture`] is the one exception and it is deliberate: it calls
/// the operating system to discover the input device, and doing that once while
/// idle is cheaper than holding no lock and racing another caller onto the same
/// device.
pub struct DictationHost<R = crate::CpalAudioRecorder> {
    surface: Arc<dyn DictationSurface>,
    feedback: Mutex<crate::RecordingFeedback>,
    /// The operating-system call that names the app the user is typing into. A
    /// field rather than a direct call so the target policy is decided by tests
    /// with injected outcomes, and never by whatever happens to be in front on
    /// the machine running them (slugtale-7lq0).
    focus_target_source: Arc<dyn Fn() -> Option<i32> + Send + Sync>,
    capture: Mutex<crate::AudioCaptureSession<R>>,
    /// What the running dictation pinned at Start. Replaced by the next Start,
    /// which is why a job must be checked against it rather than assumed current.
    session: Mutex<Option<DictationSession>>,
    /// The capture ring's voiced-sample watermark, published by the recorder
    /// into a cell it shares with this host. Reading it takes no lock, which is
    /// what lets the level-emitter thread name a Pause Flush's cut while Stop or
    /// Cancel holds the capture lock and is joining that very thread
    /// (slugtale-xqzs).
    voice_watermark: Arc<std::sync::atomic::AtomicU64>,
    runtime_state: Mutex<Option<Arc<crate::DictationRuntime>>>,
    usage: Arc<crate::UsageQueue>,
}

impl<R> DictationHost<R>
where
    R: crate::DictationRecorder,
{
    pub fn new(surface: Arc<dyn DictationSurface>, usage: Arc<crate::UsageQueue>) -> Self
    where
        R: Default,
    {
        Self::with_recorder(surface, R::default(), usage)
    }

    pub fn with_recorder(
        surface: Arc<dyn DictationSurface>,
        recorder: R,
        usage: Arc<crate::UsageQueue>,
    ) -> Self {
        Self::with_recorder_and_focus_target_source(
            surface,
            recorder,
            usage,
            Arc::new(crate::capture_text_target),
        )
    }

    /// [`Self::with_recorder`] with the frontmost-application lookup injected.
    pub fn with_recorder_and_focus_target_source(
        surface: Arc<dyn DictationSurface>,
        recorder: R,
        usage: Arc<crate::UsageQueue>,
        focus_target_source: Arc<dyn Fn() -> Option<i32> + Send + Sync>,
    ) -> Self {
        let voice_watermark = recorder.voice_watermark_cell();
        Self {
            surface,
            feedback: Mutex::new(crate::RecordingFeedback::default()),
            focus_target_source,
            capture: Mutex::new(crate::AudioCaptureSession::new(recorder)),
            session: Mutex::new(None),
            voice_watermark,
            runtime_state: Mutex::new(None),
            usage,
        }
    }

    /// Install the runtime once setup has started it. Every lifecycle call
    /// before this would find nothing able to record, so setup orders this
    /// ahead of any activation input.
    pub fn set_runtime(&self, runtime: Arc<crate::DictationRuntime>) -> Result<(), String> {
        let mut guard = self
            .runtime_state
            .lock()
            .map_err(|_| "dictation runtime state mutex poisoned".to_string())?;
        *guard = Some(runtime);
        Ok(())
    }

    /// The capture ring's voiced-sample watermark, read when the Pause Flush is
    /// due — the microphone half of the watermark cut (ADR-0026).
    ///
    /// Deliberately lock-free: the level-emitter thread asks this at the instant a
    /// Segment Pause elapses, which can be while Stop or Cancel holds the capture
    /// lock and is joining that thread. A locked read there leaves the two waiting
    /// for each other and the app frozen (slugtale-xqzs).
    pub fn voice_watermark(&self) -> u64 {
        self.voice_watermark.load(Ordering::Acquire)
    }

    /// Prepare audio capture while idle so the first Hotkey does not pay for
    /// device discovery and ring allocation (slugtale-g1o.3). Preparation must
    /// never prompt, so callers gate this on an already-granted microphone.
    /// `settings` decides which microphone is prepared.
    pub fn prepare_capture(&self, settings: &crate::Settings) {
        if let Ok(mut guard) = self.capture.lock() {
            guard.set_prefer_built_in_microphone(settings.prefer_built_in_microphone);
            let _ = guard.prepare();
        }
    }

    /// The app's one Dictation Runtime.
    ///
    /// # Panics
    /// Before setup has started the runtime; nothing reaches this module
    /// before that.
    pub fn runtime(&self) -> Arc<crate::DictationRuntime> {
        self.runtime_state
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .expect("dictation runtime started")
    }

    pub fn handle_dictation_event(&self, event: crate::DictationEvent) -> Result<(), String> {
        self.handle_dictation_event_with(event, None)
    }

    /// `activation` is the snapshot a Hotkey press built for its readiness gate;
    /// Start consumes it so the rest of the activation reuses the same Settings
    /// value instead of reloading (slugtale-g1o.6). Callers without one — Cancel
    /// from the tray, tests — pass `None`.
    pub fn handle_dictation_event_with(
        &self,
        event: crate::DictationEvent,
        mut activation: Option<crate::DictationActivation>,
    ) -> Result<(), String> {
        self.surface
            .record_diagnostic_event(crate::DiagnosticEvent::hotkey_transition(event));

        match event {
            crate::DictationEvent::Start => {
                // Capture the app the user is dictating into before our own bar can
                // take focus, so insertion can re-target it later (slugtale-squ).
                let target_pid = self.frontmost_app();
                // Resolved before capture: Settings choose the microphone as
                // well as how the bar looks, and this snapshot is the one the
                // dictation pins below.
                let settings = match activation.take() {
                    Some(activation) => activation.settings,
                    None => self.surface.settings(),
                };
                // Open the dictation before capture starts: the level callback
                // installed below stamps every Pause Flush with this session, and
                // the detector is armed with this dictation's own Segment Pause,
                // from the same Settings snapshot pinned just below — so the
                // length it flushes at and the Settings its jobs read cannot
                // come from different points in time (slugtale-cbxb).
                let session = self.runtime().begin(crate::segment_pause_duration(&settings));
                // Pin everything this dictation's jobs will need, so a segment
                // decoding a second from now cannot pick up the next dictation's
                // target or the user's latest Settings (slugtale-cbxb).
                self.pin_session(DictationSession {
                    session,
                    settings: settings.clone(),
                    target_pid,
                });
                if let Ok(mut guard) = self.capture.lock() {
                    guard.set_prefer_built_in_microphone(settings.prefer_built_in_microphone);
                }
                // If the microphone cannot start, do not show a recording state.
                self.handle_audio_capture_event(event)?;
                self.apply_recording_feedback(event, Some(&settings))?;
            }
            // Stop plays its cue but leaves the bar on screen: the audio-capture step
            // switches it to a transcribing state and hides it once the workflow
            // finishes, so the user sees the model working (slugtale-0t4). Its bar
            // update is this Stop press's own activation, so read Settings once here.
            crate::DictationEvent::Stop => {
                self.advance_recording_feedback(event)?;
                let settings = self.surface.settings();
                self.handle_audio_capture_event_with_settings(event, Some(&settings))?;
            }
            // Cancel clears the bar immediately and discards the audio. It also
            // drops any Dictation Segment still queued, so nothing further is typed
            // after the user asks Slugtale to stop. Text inserted by an earlier
            // Segment Pause is not undone (ADR-0014). It reads no Settings at all.
            crate::DictationEvent::Cancel => {
                self.runtime().abandon();
                self.apply_recording_feedback(event, None)?;
                self.handle_audio_capture_event(event)?;
            }
        }

        Ok(())
    }

    /// Advance the recording-feedback state machine and play its audible cue without
    /// touching the Dictation Bar window. Callers that own the bar's visibility (Stop,
    /// which keeps it up for transcription) use this directly.
    fn advance_recording_feedback(
        &self,
        event: crate::DictationEvent,
    ) -> Result<crate::RecordingFeedbackEffect, String> {
        let effect = {
            let mut guard = self
                .feedback
                .lock()
                .map_err(|_| "recording feedback mutex poisoned".to_string())?;
            guard.on_event(event)
        };

        if let Some(sound) = effect.sound {
            self.surface.play_dictation_sound(sound);
        }

        Ok(effect)
    }

    fn apply_recording_feedback(
        &self,
        event: crate::DictationEvent,
        settings: Option<&crate::Settings>,
    ) -> Result<(), String> {
        let effect = self.advance_recording_feedback(event)?;

        if effect.bar_visible {
            // Only the visible branch needs Settings; Cancel passes `None` and
            // never pays for a read.
            let owned;
            let settings = match settings {
                Some(settings) => settings,
                None => {
                    owned = self.surface.settings();
                    &owned
                }
            };
            self.surface
                .show_dictation_bar(DictationPhase::Recording, settings);
        } else {
            self.surface.hide_dictation_bar();
        }

        Ok(())
    }

    /// The app that has focus right now, as the operating system reports it.
    fn frontmost_app(&self) -> Option<i32> {
        (self.focus_target_source)()
    }

    /// Record what this dictation pinned, replacing whatever the previous one
    /// pinned. A running job that still holds the older session's number is then
    /// visibly out of date instead of quietly adopting the new dictation's
    /// Settings and text target (slugtale-cbxb).
    fn pin_session(&self, pinned: DictationSession) {
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some(pinned);
        }
    }

    /// What `session` pinned when it began, or an error naming the session that
    /// no longer owns it.
    fn pinned_session(&self, session: u64) -> Result<DictationSession, String> {
        self.session
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .filter(|pinned| pinned.session == session)
            .ok_or_else(|| {
                format!("dictation segment belongs to session {session}, which is no longer running")
            })
    }

    fn handle_audio_capture_event(&self, event: crate::DictationEvent) -> Result<(), String> {
        self.handle_audio_capture_event_with_settings(event, None)
    }

    /// `bar_settings` is needed only when a Stop completes and the bar switches to
    /// its transcribing state; passing it in spares that path a Settings reload
    /// (slugtale-g1o.6).
    fn handle_audio_capture_event_with_settings(
        &self,
        event: crate::DictationEvent,
        bar_settings: Option<&crate::Settings>,
    ) -> Result<(), String> {
        // Built before the capture lock is taken: the level callback closes over
        // the runtime, and reading it under the capture lock would nest one
        // host lock inside another on the Dictation Bar's hottest path.
        let level_callback = matches!(event, crate::DictationEvent::Start)
            .then(|| self.dictation_audio_level_callback());
        let outcome = {
            let mut guard = self
                .capture
                .lock()
                .map_err(|_| "audio capture mutex poisoned".to_string())?;
            if let Some(level_callback) = level_callback {
                guard.set_level_callback(Some(level_callback));
            }
            guard.on_event(event)
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.clear_dictation_audio_level_callback();
                self.surface.hide_dictation_bar();
                self.surface
                    .record_diagnostic_event(crate::DiagnosticEvent::audio_capture_failed(&error));
                #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
                self.surface.notify_capture_failure(&error.to_string());
                return Err(error.to_string());
            }
        };

        match outcome {
            Some(crate::AudioCaptureOutcome::Completed(audio)) => {
                self.clear_dictation_audio_level_callback();
                eprintln!(
                    "captured dictation audio: {} samples at {} Hz",
                    audio.samples.len(),
                    audio.sample_rate_hz
                );
                // Keep the bar on screen in a transcribing state while the model runs,
                // then hide it once insertion completes (slugtale-0t4). The worker
                // hides it, so it stays up until every earlier Segment Pause has
                // landed too, not just this last one.
                let owned;
                let bar_settings = match bar_settings {
                    Some(settings) => settings,
                    // Only this path pays for a Settings reload (slugtale-g1o.6).
                    None => {
                        owned = self.surface.settings();
                        &owned
                    }
                };
                self.surface
                    .show_dictation_bar(DictationPhase::Transcribing, bar_settings);
                let queued = self.runtime().send_last(audio);
                if !queued {
                    eprintln!("dictation segment worker is unavailable; dropping final segment");
                    self.surface.hide_dictation_bar();
                }
            }
            Some(crate::AudioCaptureOutcome::Discarded) => {
                self.clear_dictation_audio_level_callback();
                eprintln!("discarded dictation audio");
                self.surface.hide_dictation_bar();
            }
            // No active session to drain. A terminal event still clears any bar left
            // on screen (e.g. Stop with nothing captured); Start has none to hide.
            None => {
                if matches!(event, crate::DictationEvent::Stop) {
                    self.surface.hide_dictation_bar();
                }
            }
        }

        Ok(())
    }

    /// The Segment Pause detector lives inside the Dictation Runtime, which
    /// re-arms it on every begin(), so each dictation starts unable to flush.
    fn dictation_audio_level_callback(&self) -> crate::AudioLevelCallback {
        let surface = self.surface.clone();
        let runtime = self.runtime();
        Arc::new(move |level| {
            surface.emit_dictation_audio_level(level);
            runtime.on_voice_level(level);
        })
    }

    fn clear_dictation_audio_level_callback(&self) {
        if let Ok(mut guard) = self.capture.lock() {
            guard.set_level_callback(None);
        }
        self.surface.emit_dictation_audio_level(0.0);
    }

    /// Transcribe and insert one Dictation Segment, start to finish.
    ///
    /// Runs synchronously on the Dictation Segment worker thread, as part of
    /// `session`. The Settings and the text target come from what that session
    /// pinned at Start, never from a re-read: a decode long enough for the user
    /// to change either must not land its words somewhere else (slugtale-cbxb).
    ///
    /// The session is asked about once more at the insertion boundary itself,
    /// through the prepared pair's gate. Checking only before the decode started
    /// is what let a cancelled dictation still type (slugtale-cbxb).
    pub fn run_dictation_segment(
        &self,
        session: u64,
        audio: crate::CapturedAudio,
        position: crate::DictationSegmentPosition,
    ) -> Result<crate::DictationSegmentOutcome, String> {
        let pinned = self.pinned_session(session)?;
        let settings = &pinned.settings;
        let stack = self.surface.dictation_stack(settings)?;

        let runtime = self.runtime();
        let mut prepared = self.surface.prepared_insertion(pinned.target_pid)?;
        // Scope every effect this segment would have on the user — the keystrokes,
        // the focus work before them, and the clipboard rescue if insertion fails
        // — to the session that owns them. The runtime decides each one at the
        // moment it happens rather than once here, because the decode that has
        // just finished can have been overtaken (slugtale-cbxb).
        prepared.guard_with_session(session, runtime.session_effects());

        let completed = crate::DictationWorkflow::new(
            &stack,
            &prepared.insertion,
            prepared.rescue.as_ref(),
            settings.transcript_cleanup,
        )
        .complete(audio, position);

        // A refused insertion reaches the Dictation Workflow as an adapter that
        // declined to type, which it reports as a plain success. Only this module
        // knows the refusal meant "the user cancelled", so the segment is put
        // back to what actually happened: nothing was typed, nothing was
        // rescued, and it must not count or take the dictation's first position.
        let completed = match completed {
            Ok(outcome) if prepared.insertion_was_refused() => {
                Ok(crate::DictationSegmentOutcome {
                    inserted: false,
                    rescued: false,
                    insertion_failure: None,
                    ..outcome
                })
            }
            other => other,
        };

        match completed {
            Ok(outcome) => {
                record_insertion_diagnostics(
                    self.surface.as_ref(),
                    outcome.insertion_failure.as_ref(),
                    outcome.rescued,
                );
                Ok(outcome)
            }
            Err(error) => {
                record_insertion_diagnostics(
                    self.surface.as_ref(),
                    error.insertion_failure(),
                    false,
                );
                Err(error.to_string())
            }
        }
    }

    /// Take the speech captured so far as a Dictation Segment, leaving the
    /// microphone running. Called only from the worker thread. `cut` is the sample
    /// watermark the Pause Flush was queued with: the segment ends there (plus a
    /// small acoustic guard), whatever else has arrived since.
    pub fn take_dictation_segment(
        &self,
        session: u64,
        cut: u64,
    ) -> Option<crate::CapturedAudio> {
        let flushed = self
            .capture
            .lock()
            .map_err(|_| "audio capture mutex poisoned".to_string())
            .and_then(|mut guard| {
                // The capture lock is held, so this is the last moment a newer
                // Start can be beginning a capture: if the ring is no longer this
                // session's, nothing may be drained from it (slugtale-cbxb).
                if !self.owns_capture(session) {
                    return Ok(None);
                }
                guard.cut_segment(cut).map_err(|error| error.to_string())
            });

        match flushed {
            Ok(audio) => audio,
            Err(error) => {
                eprintln!("could not take dictation segment: {error}");
                None
            }
        }
    }

    /// Whether `session` is the dictation the capture ring currently belongs to.
    ///
    /// The session pinned at Start is the capture's identity, which is what makes
    /// "is this audio still ours" answerable at all: audio alone carries no
    /// session, so a job that drained late would otherwise take the next
    /// recording's speech (slugtale-cbxb).
    fn owns_capture(&self, session: u64) -> bool {
        self.session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|pinned| pinned.session))
            == Some(session)
    }
}

/// Report how one segment's Text Insertion went. A rescued segment is the case
/// where insertion failed and the transcription was preserved anyway, so it is
/// the one that produces both lines. The Dictation Workflow has no Local
/// Diagnostic Log of its own, which is why this is asked of the surface here
/// rather than from inside it.
fn record_insertion_diagnostics(
    surface: &dyn DictationSurface,
    insertion_failure: Option<&crate::TextInsertionError>,
    rescued: bool,
) {
    if let Some(error) = insertion_failure {
        surface.record_diagnostic_event(crate::DiagnosticEvent::insertion_failed(error));
    }
    if rescued {
        surface.record_diagnostic_event(crate::DiagnosticEvent::insertion_rescued());
    }
}

/// The Dictation Host is the Dictation Runtime's host, so the Tauri shell needs
/// no adapter of its own: the microphone cut, the Dictation Workflow, the
/// Usage handoff, and the Dictation Bar hide that follows the final job are all
/// answers this module already had. The recorder is `Send` because the worker
/// thread reaches this host while the level-emitter thread and the app handle
/// still hold it.
impl<R> crate::DictationRuntimeHost for DictationHost<R>
where
    R: crate::DictationRecorder + Send,
{
    fn take_pause_segment(&self, session: u64, cut: u64) -> Option<crate::CapturedAudio> {
        self.take_dictation_segment(session, cut)
    }

    fn complete(
        &self,
        session: u64,
        audio: crate::CapturedAudio,
        position: crate::DictationSegmentPosition,
    ) -> Result<crate::DictationSegmentOutcome, String> {
        self.run_dictation_segment(session, audio, position)
    }

    fn record_counted_segment(&self, segment: crate::CountedSegment) {
        self.usage.record(segment);
    }

    /// `session`'s final job has settled, whatever way it settled. Only that
    /// dictation's own bar may be hidden: Cancel cleared its bar at the moment the
    /// user pressed Escape, and a dictation a newer Start replaced must leave the
    /// new bar alone (slugtale-cbxb).
    ///
    /// The runtime has already decided this, under the same lock that a newer
    /// Start takes, so this method only mutates the surface — the decision and the
    /// hide cannot come apart.
    fn last_job_settled(&self, _session: u64) {
        self.surface.hide_dictation_bar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioCaptureError, CapturedAudio, CountedSegment, DictationEvent, DictationRecorder,
        DictationRuntime, DictationRuntimeHost, DictationSegmentOutcome, DictationSegmentPosition,
        EngineAssetLifecycle, EngineAvailability, EngineConfidence, EngineMetadata,
        EngineTranscriber, EngineTranscription, FileDiagnosticSink, FinalTranscription,
        InsertionRescue, InsertionRescueError, PreparedInsertion, SettledTextInsertion,
        SharedDiagnosticLog, TextInsertion, TextInsertionError, TranscriptionProvider, VOICE_LEVEL,
    };
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// Every effect the lifecycle asked of its surface, in the order it asked.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Diagnostic(&'static str),
        PlaySound(&'static str),
        ShowBar(&'static str),
        HideBar,
        ClearAudioLevel,
        ReadSettings,
        NotifyCaptureFailure,
        /// The text target the Dictation Workflow aimed at for a segment, so a
        /// test can see which app a segment was pinned to.
        PreparedInsertion(Option<i32>),
    }

    #[derive(Clone)]
    struct FakeSurface {
        calls: Arc<std::sync::Mutex<Vec<Call>>>,
        /// What the fake Transcription Engine answers with, one entry per
        /// segment and the last one repeating, so a test names only the
        /// segments it reads.
        transcriptions: Arc<std::sync::Mutex<Vec<String>>>,
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        rescued: Arc<std::sync::Mutex<Vec<String>>>,
        /// Set by a test that needs Text Insertion to fail, so the Insertion
        /// Rescue is the path the segment takes.
        insertion_fails: Arc<std::sync::atomic::AtomicBool>,
        /// Held while a segment's Transcription runs, so a test can keep a decode
        /// in flight across a Cancel and a new Start.
        decoding: Option<Arc<DecodeGate>>,
        /// The Settings this surface answers with. Defaults to
        /// [`TEST_SEGMENT_PAUSE_SECS`], the shortest Segment Pause the Settings
        /// File accepts, so a Pause Flush test waits two real seconds rather
        /// than the product default's five; a test can change it mid-dictation
        /// to stand in for a save.
        settings: Arc<std::sync::Mutex<crate::Settings>>,
    }

    impl Default for FakeSurface {
        fn default() -> Self {
            Self {
                calls: Arc::default(),
                transcriptions: Arc::default(),
                inserted: Arc::default(),
                rescued: Arc::default(),
                insertion_fails: Arc::default(),
                decoding: None,
                settings: Arc::new(std::sync::Mutex::new(crate::Settings {
                    segment_pause_secs: TEST_SEGMENT_PAUSE_SECS,
                    ..crate::Settings::default()
                })),
            }
        }
    }

    /// The gate a blocking Transcription waits on: `entered` is closed when the
    /// decode starts, `release` is opened when the test lets it finish.
    #[derive(Default)]
    struct DecodeGate {
        entered: Arc<(Mutex<bool>, std::sync::Condvar)>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl DecodeGate {
        /// Wait until some segment's Transcription has actually started.
        fn wait_until_decoding(&self) {
            let (lock, condvar) = &*self.entered;
            let mut decoding = lock.lock().unwrap();
            while !*decoding {
                let (next, timeout) = condvar
                    .wait_timeout(decoding, Duration::from_secs(5))
                    .expect("decode gate mutex is usable");
                decoding = next;
                assert!(
                    !timeout.timed_out(),
                    "the segment worker never reached Transcription"
                );
            }
        }

        /// Let the blocked decode finish.
        fn release(&self) {
            let (lock, condvar) = &*self.release;
            *lock.lock().unwrap() = true;
            condvar.notify_all();
        }
    }

    impl FakeSurface {
        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn clear_calls(&self) {
            self.calls.lock().unwrap().clear();
        }

        fn transcribing_as(self: &Arc<Self>, texts: &[&str]) -> Arc<Self> {
            *self.transcriptions.lock().unwrap() =
                texts.iter().map(|text| text.to_string()).collect();
            self.clone()
        }

        fn next_transcription(&self) -> String {
            let mut texts = self.transcriptions.lock().unwrap();
            if texts.len() > 1 {
                texts.remove(0)
            } else {
                texts.first().cloned().unwrap_or_default()
            }
        }

        fn inserted(&self) -> Vec<String> {
            self.inserted.lock().unwrap().clone()
        }

        fn rescued(&self) -> Vec<String> {
            self.rescued.lock().unwrap().clone()
        }

        fn with_failing_insertion(self: &Arc<Self>) -> Arc<Self> {
            self.insertion_fails
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.clone()
        }

        /// Change what this surface's Settings answer with, the way a save
        /// does: the next dictation pins the new value and the one in progress
        /// keeps what it pinned at Start.
        fn set_segment_pause_secs(&self, secs: i64) {
            self.settings.lock().unwrap().segment_pause_secs = secs;
        }

        /// Hold every Transcription until [`DecodeGate::release`], so a test can
        /// cancel and restart while a decode is genuinely in flight.
        fn blocking_transcription(self: &Arc<Self>) -> (Arc<Self>, Arc<DecodeGate>) {
            let gate = Arc::new(DecodeGate::default());
            let mut surface = (**self).clone();
            surface.decoding = Some(Arc::clone(&gate));
            (Arc::new(surface), gate)
        }
    }

    impl DictationSurface for FakeSurface {
        fn settings(&self) -> crate::Settings {
            self.record(Call::ReadSettings);
            self.settings.lock().unwrap().clone()
        }

        fn record_diagnostic_event(&self, event: crate::DiagnosticEvent) {
            let tag = match event {
                crate::DiagnosticEvent::HotkeyTransition { .. } => "hotkey_transition",
                crate::DiagnosticEvent::AudioCaptureFailed { .. } => "audio_capture_failed",
                crate::DiagnosticEvent::InsertionFailed { .. } => "insertion_failed",
                crate::DiagnosticEvent::InsertionRescued => "insertion_rescued",
                _ => "other",
            };
            self.record(Call::Diagnostic(tag));
        }

        fn show_dictation_bar(&self, phase: DictationPhase, _settings: &crate::Settings) {
            self.record(Call::ShowBar(phase.as_str()));
        }

        fn hide_dictation_bar(&self) {
            self.record(Call::HideBar);
        }

        fn emit_dictation_audio_level(&self, level: f32) {
            if level == 0.0 {
                self.record(Call::ClearAudioLevel);
            }
        }

        fn notify_capture_failure(&self, _error: &str) {
            self.record(Call::NotifyCaptureFailure);
        }

        fn play_dictation_sound(&self, sound: crate::DictationSound) {
            let name = match sound {
                crate::DictationSound::Start => "start",
                crate::DictationSound::Stop => "stop",
            };
            self.record(Call::PlaySound(name));
        }

        fn diagnostic_log(
            &self,
            _settings: &crate::Settings,
        ) -> SharedDiagnosticLog<FileDiagnosticSink> {
            SharedDiagnosticLog::new(false, FileDiagnosticSink::unavailable())
        }

        fn dictation_stack(
            &self,
            _settings: &crate::Settings,
        ) -> Result<crate::DictationStack<FileDiagnosticSink>, String> {
            let gate = self.decoding.clone();
            let engine: Arc<dyn TranscriptionProvider> = Arc::new(FakeEngine {
                text: self.next_transcription(),
                gate,
            });
            let router = crate::SecondOpinionRouter::single(engine);
            Ok(crate::DictationStack::new(
                router,
                SharedDiagnosticLog::new(false, FileDiagnosticSink::unavailable()),
            ))
        }

        fn prepared_insertion(
            &self,
            target_pid: Option<i32>,
        ) -> Result<PreparedInsertion, String> {
            self.record(Call::PreparedInsertion(target_pid));
            Ok(PreparedInsertion::new(
                SettledTextInsertion::new(
                    Box::new(RecordingInsertion {
                        inserted: self.inserted.clone(),
                        fails: self
                            .insertion_fails
                            .load(std::sync::atomic::Ordering::SeqCst),
                    }),
                    None,
                ),
                Box::new(RecordingRescue {
                    rescued: self.rescued.clone(),
                }),
            ))
        }
    }

    /// A Transcription Engine that answers with the text it was handed, so the
    /// Dictation Workflow downstream of it is the real one. With a gate, the
    /// decode blocks inside `transcribe`, which is the window a Cancel and a new
    /// Start used to slip through.
    struct FakeEngine {
        text: String,
        gate: Option<Arc<DecodeGate>>,
    }

    impl EngineTranscriber for FakeEngine {
        fn engine(&self) -> crate::TranscriptionEngine {
            crate::TranscriptionEngine::Whisper
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: crate::TranscriptionEngine::Whisper,
                model_id: "test",
                capability: "test",
                revision: "test",
                approximate_bytes: None,
                source_url: None,
                license: "test",
                license_url: "https://example.test",
                attribution: None,
                modifications: None,
                system_managed: false,
                supported_platforms: "test",
            }
        }

        fn availability(&self) -> EngineAvailability {
            EngineAvailability::Available
        }

        fn transcribe(
            &self,
            _audio: &CapturedAudio,
        ) -> Result<EngineTranscription, crate::AsrError> {
            if let Some(gate) = self.gate.as_ref() {
                let (lock, condvar) = &*gate.entered;
                *lock.lock().unwrap() = true;
                condvar.notify_all();
                let (lock, condvar) = &*gate.release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = condvar
                        .wait(released)
                        .expect("decode gate mutex is usable");
                }
            }
            Ok(EngineTranscription {
                engine: crate::TranscriptionEngine::Whisper,
                transcription: FinalTranscription::plain(self.text.clone()),
                alternatives: Vec::new(),
                confidence: EngineConfidence::unreported(),
                latency: Duration::ZERO,
            })
        }
    }

    impl EngineAssetLifecycle for FakeEngine {
        fn assets(&self) -> crate::EngineAssets {
            crate::EngineAssets {
                installed_bytes: None,
                present: Some(true),
            }
        }
    }

    struct RecordingInsertion {
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        fails: bool,
    }

    impl TextInsertion for RecordingInsertion {
        fn insert(&self, transcription: &FinalTranscription) -> Result<(), TextInsertionError> {
            self.inserted
                .lock()
                .unwrap()
                .push(transcription.text.clone());
            if self.fails {
                return Err(TextInsertionError::new("fake insertion failure"));
            }
            Ok(())
        }
    }

    struct RecordingRescue {
        rescued: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl InsertionRescue for RecordingRescue {
        fn rescue(&self, transcription: &FinalTranscription) -> Result<(), InsertionRescueError> {
            self.rescued
                .lock()
                .unwrap()
                .push(transcription.text.clone());
            Ok(())
        }
    }

    /// The microphone a test speaks into. `level` hands back the level publisher
    /// the host installed, so a test reaches the Segment Pause detector through
    /// the same callback the audio emitter thread calls.
    #[derive(Clone, Default)]
    struct FakeMicrophone {
        /// Every cut the recorder was asked for, so a test can prove the
        /// microphone was never touched rather than inferring it from what came
        /// back.
        cuts: Arc<std::sync::Mutex<Vec<u64>>>,
        level: Arc<std::sync::Mutex<Option<crate::AudioLevelCallback>>>,
        /// The ring position of the last voiced sample, moved forward whenever
        /// speech is heard so a Pause Flush has a cut worth queueing.
        watermark: Arc<std::sync::atomic::AtomicU64>,
        /// Set once a recorder has joined the level thread it drove, so a test can
        /// say the pairing completed rather than infer it from a Stop returning.
        joined_the_level_thread: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeMicrophone {
        /// A level the Dictation Bar treats as speech, which both flexes the
        /// waveform and holds a Segment Pause open.
        fn speaking() -> f32 {
            VOICE_LEVEL + 0.2
        }

        /// Speak, then stay quiet long enough for the Segment Pause to elapse.
        /// The pause is a real clock, so the fixture's Settings ask for the
        /// shortest length the Settings File accepts and the test waits that
        /// plus scheduler slop.
        fn speak_then_pause(&self) {
            self.voice(Self::speaking());
            std::thread::sleep(TEST_PAUSE + PAUSE_SLOP);
            self.voice(0.0);
        }

        fn voice(&self, level: f32) {
            if crate::is_voice_level(level) {
                self.watermark
                    .fetch_add(8_000, std::sync::atomic::Ordering::Relaxed);
            }
            let callback = self.level.lock().unwrap().clone();
            if let Some(callback) = callback {
                callback(level);
            }
        }
    }

    /// A recorder that never touches a device. `fail_start` simulates a
    /// microphone that cannot open, `silent_stop` the digital silence of a
    /// denied macOS microphone, and `fail_cut` a capture that breaks exactly
    /// when a Pause Flush asks it for a segment. `join_the_level_thread_on_stop`
    /// reproduces the production order that used to freeze the app: the level
    /// thread is joined while the Dictation Host still holds the capture lock.
    struct FakeRecorder {
        fail_start: bool,
        silent_stop: bool,
        fail_cut: bool,
        microphone: FakeMicrophone,
        /// Every cut a Pause Flush asked the microphone for. A channel rather
        /// than a shared log so a test can wait for the cut instead of racing
        /// the Stop that would end the dictation first.
        cuts: mpsc::Sender<u64>,
        /// The microphone preference in force when `start` last ran.
        started_preferring_built_in: Arc<Mutex<Option<bool>>>,
        prefer_built_in: bool,
        /// Whether `stop` drives a level callback to completion itself, the way a
        /// real recorder joins its level emitter while the caller holds the
        /// capture lock.
        join_the_level_thread_on_stop: bool,
    }

    impl FakeRecorder {
        fn healthy() -> Self {
            Self {
                fail_start: false,
                silent_stop: false,
                fail_cut: false,
                microphone: FakeMicrophone::default(),
                cuts: mpsc::channel().0,
                started_preferring_built_in: Arc::default(),
                prefer_built_in: false,
                join_the_level_thread_on_stop: false,
            }
        }

        fn joining_the_level_thread_on_stop(mut self) -> Self {
            self.join_the_level_thread_on_stop = true;
            self
        }

        /// Stand in for the level emitter's own thread while `stop` runs: the
        /// callback is driven on another thread and waited for here, which is
        /// exactly the pairing that froze the app (slugtale-xqzs).
        fn join_the_level_thread(&self) {
            if !self.join_the_level_thread_on_stop {
                return;
            }
            let Some(callback) = self.microphone.level.lock().unwrap().clone() else {
                return;
            };
            std::thread::spawn(move || {
                callback(FakeMicrophone::speaking());
                // Long enough for the armed Segment Pause to elapse while Stop
                // waits: the watermark probe at that moment is the path that
                // used to want the same capture lock Stop held.
                std::thread::sleep(TEST_PAUSE + PAUSE_SLOP);
                callback(0.0);
            })
            .join()
            .expect("the level thread does not panic");
            self.microphone
                .joined_the_level_thread
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl Default for FakeRecorder {
        fn default() -> Self {
            Self::healthy()
        }
    }

    impl DictationRecorder for FakeRecorder {
        fn prepare(&mut self) -> Result<(), AudioCaptureError> {
            Ok(())
        }

        fn start(&mut self) -> Result<(), AudioCaptureError> {
            *self.started_preferring_built_in.lock().unwrap() = Some(self.prefer_built_in);
            if self.fail_start {
                return Err(AudioCaptureError::new("fake start failure"));
            }
            Ok(())
        }

        fn set_prefer_built_in_microphone(&mut self, prefer: bool) {
            self.prefer_built_in = prefer;
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            self.join_the_level_thread();
            Ok(self.captured())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.join_the_level_thread();
            Ok(())
        }

        fn cut_segment(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            let _ = self.cuts.send(cut);
            self.microphone.cuts.lock().unwrap().push(cut);
            if self.fail_cut {
                return Err(AudioCaptureError::new("fake cut failure"));
            }
            Ok(self.captured())
        }

        fn voice_watermark_cell(&self) -> Arc<std::sync::atomic::AtomicU64> {
            Arc::clone(&self.microphone.watermark)
        }

        fn set_level_callback(&mut self, callback: Option<crate::AudioLevelCallback>) {
            *self.microphone.level.lock().unwrap() = callback;
        }
    }

    impl FakeRecorder {
        fn captured(&self) -> CapturedAudio {
            CapturedAudio {
                sample_rate_hz: 16_000,
                samples: if self.silent_stop {
                    vec![0.0; 160]
                } else {
                    vec![0.4; 480]
                },
            }
        }
    }

    /// A Segment Pause short enough for a test to sit through. The rule under it
    /// is the one the five-second default drives.
    /// The shortest Segment Pause the Settings File accepts. The fixture's
    /// Settings ask for it, so a Pause Flush test waits two real seconds rather
    /// than the product default's five.
    const TEST_SEGMENT_PAUSE_SECS: i64 = 2;
    const TEST_PAUSE: Duration = Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64);
    /// Scheduler slop past the armed pause before the quiet level that ends a
    /// segment, so a loaded machine cannot race the flush.
    const PAUSE_SLOP: Duration = Duration::from_millis(300);

    /// The app a test dictates into, reported by the injected focus lookup
    /// rather than by whatever is in front on the machine running the tests.
    fn focus_target_source() -> Arc<dyn Fn() -> Option<i32> + Send + Sync> {
        Arc::new(|| Some(TEST_TARGET_PID))
    }

    const TEST_TARGET_PID: i32 = 4_242;

    /// A runtime host that reaches nothing, for the tests that only exercise the
    /// lifecycle events and never reach a Dictation Segment.
    struct UnreachableRuntimeHost;

    impl DictationRuntimeHost for UnreachableRuntimeHost {
        fn take_pause_segment(&self, _session: u64, _cut: u64) -> Option<CapturedAudio> {
            None
        }

        fn complete(
            &self,
            _session: u64,
            _audio: CapturedAudio,
            _position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            Err("test host never transcribes".to_string())
        }

        fn record_counted_segment(&self, _segment: CountedSegment) {}

        fn last_job_settled(&self, _session: u64) {}
    }

    /// The Usage handoff these tests never read: a writer thread that discards
    /// every Counted Segment.
    fn discarding_usage_queue() -> Arc<crate::UsageQueue> {
        crate::UsageQueue::start(Arc::new(|_: crate::LocalDate, _: CountedSegment| {}))
            .expect("the usage writer starts")
    }

    fn started_runtime() -> Arc<DictationRuntime> {
        Arc::new(
            DictationRuntime::start(Arc::new(UnreachableRuntimeHost), || 0)
                .expect("test runtime starts"),
        )
    }

    /// A dictation whose Dictation Segments are real: the worker runs, the
    /// segment path reaches the host, and Counted Segments come back to
    /// `counted` so a test can wait for one.
    struct Dictating {
        host: Arc<DictationHost<FakeRecorder>>,
        surface: Arc<FakeSurface>,
        microphone: FakeMicrophone,
        cuts: mpsc::Receiver<u64>,
        counted: mpsc::Receiver<CountedSegment>,
    }

    impl Dictating {
        fn recording(surface: &Arc<FakeSurface>, mut recorder: FakeRecorder) -> Self {
            let microphone = recorder.microphone.clone();
            let (cut_sender, cuts) = mpsc::channel();
            recorder.cuts = cut_sender;
            let (counted_tx, counted) = mpsc::channel();
            let host = Arc::new(DictationHost::with_recorder_and_focus_target_source(
                surface.clone(),
                recorder,
                crate::UsageQueue::start(Arc::new(
                    move |_: crate::LocalDate, segment: CountedSegment| {
                        let _ = counted_tx.send(segment);
                    },
                ))
                .expect("the usage writer starts"),
                focus_target_source(),
            ));
            // The watermark the runtime probes is the host's own read of the
            // microphone, so a queued flush carries the position the capture
            // session reported rather than a number the test made up.
            let runtime_host: Arc<dyn DictationRuntimeHost> = host.clone();
            let watermark_host = Arc::clone(&host);
            let runtime = DictationRuntime::start(runtime_host, move || {
                watermark_host.voice_watermark()
            })
            .expect("test runtime starts");
            host.set_runtime(Arc::new(runtime)).unwrap();

            host.handle_dictation_event(DictationEvent::Start).unwrap();
            Self {
                host,
                surface: surface.clone(),
                microphone,
                cuts,
                counted,
            }
        }

        /// Speak, pause, and wait until that Dictation Segment has been inserted
        /// or rescued.
        fn pause_flush(&self) -> CountedSegment {
            self.microphone.speak_then_pause();
            self.counted
                .recv_timeout(Duration::from_secs(5))
                .expect("the Pause Flush settles")
        }

        fn stop(&self) -> Vec<Call> {
            self.host
                .handle_dictation_event(DictationEvent::Stop)
                .expect("stop succeeds");
            self.settle()
        }

        fn cancel(&self) {
            self.host
                .handle_dictation_event(DictationEvent::Cancel)
                .expect("cancel succeeds");
        }

        fn start_another_dictation(&self) {
            self.host
                .handle_dictation_event(DictationEvent::Start)
                .expect("start succeeds");
        }

        /// Every cut the microphone was asked for, whether or not a test read it off
        /// the channel.
        fn drain_cuts(&self) -> Vec<u64> {
            self.microphone.cuts.lock().unwrap().clone()
        }

        /// Whether Stop's own join of the level thread ran to the end. Stop
        /// returning at all proves it, so this says the same thing more plainly.
        fn joined_the_level_thread(&self) -> bool {
            self.microphone
                .joined_the_level_thread
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Wait for the worker to reach the end of the queue. Every job is
        /// ordered, so once the Dictation Bar has been hidden the earlier
        /// segments have been dealt with, whichever way they turned out.
        fn settle(&self) -> Vec<Call> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let calls = self.surface.calls();
                if calls.contains(&Call::HideBar) {
                    return calls;
                }
                assert!(
                    Instant::now() < deadline,
                    "the segment worker never settled"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Every cut not yet read back by `cut_reached_the_microphone`, in order.
        fn cuts(&self) -> Vec<u64> {
            self.cuts.try_iter().collect()
        }

        /// Wait until the worker has asked the microphone for its segment. Stop
        /// ends the dictation, so a test that has to see a cut happen first
        /// cannot afford to guess whether the worker got there in time.
        fn cut_reached_the_microphone(&self) -> u64 {
            self.cuts
                .recv_timeout(Duration::from_secs(5))
                .expect("the Pause Flush cuts the microphone")
        }
    }

    fn host_with(
        surface: &Arc<FakeSurface>,
        recorder: FakeRecorder,
    ) -> DictationHost<FakeRecorder> {
        let host = DictationHost::with_recorder_and_focus_target_source(
            surface.clone(),
            recorder,
            discarding_usage_queue(),
            focus_target_source(),
        );
        host.set_runtime(started_runtime()).unwrap();
        host
    }

    /// The session the host has pinned, which is what a Dictation Segment job for
    /// the running dictation would carry.
    fn running_session(host: &DictationHost<FakeRecorder>) -> u64 {
        host.session
            .lock()
            .unwrap()
            .as_ref()
            .expect("a dictation is running")
            .session
    }

    #[test]
    fn a_rescued_segment_records_both_the_failure_and_the_rescue() {
        // Text Insertion failed and the Insertion Rescue preserved the
        // transcription anyway. Both facts belong in the Local Diagnostic Log,
        // because "insertion failed" alone reads as lost text and "rescued"
        // alone hides why the path was taken at all (ADR-0019).
        let secret = "do not log these dictated words";
        let surface = Arc::new(FakeSurface::default()).transcribing_as(&[secret]);
        let surface = surface.with_failing_insertion();
        let host = host_with(&surface, FakeRecorder::healthy());
        host.handle_dictation_event(DictationEvent::Start).unwrap();
        let session = running_session(&host);
        surface.clear_calls();

        let outcome = host
            .run_dictation_segment(
                session,
                CapturedAudio::mono_16khz(vec![0.0, 0.25]),
                DictationSegmentPosition::First,
            )
            .expect("the rescue preserved the transcription");

        assert!(outcome.rescued);
        assert!(outcome.insertion_failure.is_some());
        assert_eq!(
            surface.calls(),
            vec![
                Call::PreparedInsertion(Some(TEST_TARGET_PID)),
                Call::Diagnostic("insertion_failed"),
                Call::Diagnostic("insertion_rescued"),
            ]
        );
        // The recorded events are reasons, never the text they were about.
        for event in [
            crate::DiagnosticEvent::InsertionFailed {
                reason: "text insertion failed: fake insertion failure".to_string(),
            },
            crate::DiagnosticEvent::InsertionRescued,
        ] {
            assert!(
                !format!("{event:?}").contains(secret),
                "leaked transcript text: {event:?}"
            );
        }
    }

    #[test]
    fn a_segment_that_inserts_records_nothing_about_insertion() {
        let surface = Arc::new(FakeSurface::default()).transcribing_as(&["typed straight in"]);
        let host = host_with(&surface, FakeRecorder::healthy());
        host.handle_dictation_event(DictationEvent::Start).unwrap();
        let session = running_session(&host);
        surface.clear_calls();

        host.run_dictation_segment(
            session,
            CapturedAudio::mono_16khz(vec![0.0, 0.25]),
            DictationSegmentPosition::First,
        )
        .expect("insertion succeeded");

        // No Settings read and no diagnostic: the segment used what the dictation
        // pinned, and insertion worked.
        assert_eq!(
            surface.calls(),
            vec![Call::PreparedInsertion(Some(TEST_TARGET_PID))]
        );
        assert_eq!(surface.inserted(), vec!["Typed straight in"]);
    }

    #[test]
    fn cancelling_from_a_hidden_bar_reads_no_settings_and_replays_nothing() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(&surface, FakeRecorder::healthy());

        host.handle_dictation_event(DictationEvent::Cancel).unwrap();

        // Cancel reads no Settings at all (CONTEXT.md: Cancel discards), and a
        // hidden bar means no sound and exactly one hide.
        assert_eq!(
            surface.calls(),
            vec![Call::Diagnostic("hotkey_transition"), Call::HideBar,]
        );
    }

    #[test]
    fn a_stray_stop_from_a_hidden_bar_replays_nothing() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(&surface, FakeRecorder::healthy());

        host.handle_dictation_event(DictationEvent::Stop).unwrap();

        // A hold-mode key release arriving after the bar went down must not
        // replay the stop sound or re-end the session (ADR-0014). The Stop
        // path still reads Settings before discovering nothing was active —
        // pinned here as the price of the shared event handler.
        assert_eq!(
            surface.calls(),
            vec![
                Call::Diagnostic("hotkey_transition"),
                Call::ReadSettings,
                Call::HideBar,
            ]
        );
    }

    #[test]
    fn start_plays_its_cue_then_shows_the_recording_bar_after_capture_opens() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(&surface, FakeRecorder::healthy());

        host.handle_dictation_event(DictationEvent::Start).unwrap();

        let calls = surface.calls();
        assert_eq!(calls[0], Call::Diagnostic("hotkey_transition"));
        // If the microphone cannot start, no recording state is shown — so the
        // cue must come after capture opened, immediately before the bar.
        let sound = calls
            .iter()
            .position(|call| *call == Call::PlaySound("start"))
            .expect("start plays its cue");
        assert_eq!(calls[sound + 1], Call::ShowBar("recording"));
        assert!(!calls.contains(&Call::HideBar));
        assert_eq!(
            calls.iter().filter(|c| **c == Call::ReadSettings).count(),
            1
        );
    }

    #[test]
    fn start_opens_the_microphone_the_settings_prefer() {
        // The press's own Settings decide the microphone, before it opens.
        let surface = Arc::new(FakeSurface::default());
        let recorder = FakeRecorder::healthy();
        let started = recorder.started_preferring_built_in.clone();
        let host = host_with(&surface, recorder);

        host.handle_dictation_event(DictationEvent::Start).unwrap();

        assert_eq!(
            *started.lock().unwrap(),
            Some(crate::Settings::default().prefer_built_in_microphone)
        );
    }

    #[test]
    fn stop_switches_the_bar_to_transcribing_and_keeps_it_up() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(&surface, FakeRecorder::healthy());
        host.handle_dictation_event(DictationEvent::Start).unwrap();
        let recording = surface
            .calls()
            .iter()
            .position(|call| *call == Call::ShowBar("recording"))
            .unwrap();

        host.handle_dictation_event(DictationEvent::Stop).unwrap();

        let calls = surface.calls();
        // The bar stays on screen for transcription (slugtale-0t4): shown again
        // as transcribing, never hidden between the two shows.
        let transcribing = calls
            .iter()
            .position(|call| *call == Call::ShowBar("transcribing"))
            .expect("stop shows the transcribing state");
        assert!(!calls[recording..transcribing].contains(&Call::HideBar));
        assert!(calls.contains(&Call::PlaySound("stop")));
        // Start reads Settings once (no activation snapshot) and Stop reads it
        // once more; nothing else pays for a read.
        assert_eq!(
            calls.iter().filter(|c| **c == Call::ReadSettings).count(),
            2
        );
    }

    #[test]
    fn a_failed_capture_hides_the_bar_and_reports_instead_of_showing_recording() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(
            &surface,
            FakeRecorder {
                fail_start: true,
                silent_stop: true,
                ..FakeRecorder::healthy()
            },
        );

        let result = host.handle_dictation_event(DictationEvent::Start);

        assert!(result.is_err());
        assert_eq!(
            surface.calls(),
            vec![
                Call::Diagnostic("hotkey_transition"),
                Call::ReadSettings,
                Call::ClearAudioLevel,
                Call::HideBar,
                Call::Diagnostic("audio_capture_failed"),
                Call::NotifyCaptureFailure,
            ]
        );
    }

    #[test]
    fn a_silent_stop_reports_a_denied_microphone_instead_of_transcribing() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(
            &surface,
            FakeRecorder {
                silent_stop: true,
                ..FakeRecorder::healthy()
            },
        );
        host.handle_dictation_event(DictationEvent::Start).unwrap();

        let result = host.handle_dictation_event(DictationEvent::Stop);

        // Digital silence is how a denied macOS microphone fails (slugtale-d3k):
        // the bar hides and the user is told, rather than a "You" transcription.
        assert!(result.is_err());
        let calls = surface.calls();
        assert!(calls.contains(&Call::ShowBar("recording")));
        assert!(calls.contains(&Call::NotifyCaptureFailure));
        assert!(!calls.contains(&Call::ShowBar("transcribing")));
    }

    #[test]
    fn cancelling_mid_dictation_discards_the_capture_and_never_plays_a_sound() {
        let surface = Arc::new(FakeSurface::default());
        let host = host_with(&surface, FakeRecorder::healthy());
        host.handle_dictation_event(DictationEvent::Start).unwrap();

        host.handle_dictation_event(DictationEvent::Cancel).unwrap();

        // Cancel clears the bar immediately and discards the audio (CONTEXT.md:
        // Cancel discards): no stop cue, no transcribing state, and the capture
        // session's Discarded outcome hides the bar a second time after the
        // feedback state machine already dropped it.
        assert_eq!(
            surface.calls(),
            vec![
                Call::Diagnostic("hotkey_transition"),
                Call::ReadSettings,
                Call::PlaySound("start"),
                Call::ShowBar("recording"),
                Call::Diagnostic("hotkey_transition"),
                Call::HideBar,
                Call::ClearAudioLevel,
                Call::HideBar,
            ]
        );
    }

    // ---- Pause Flush, from a voice level to inserted text (ADR-0026) ----

    #[test]
    fn a_segment_pause_reaches_the_text_target_while_the_dictation_runs_on() {
        // The whole middle of the chain, over the real interfaces: a level from
        // the microphone, the Segment Pause, the worker, the capture session cut,
        // and the Dictation Workflow's Immediate Insertion.
        let surface = Arc::new(FakeSurface::default());
        let surface = surface.transcribing_as(&["first words", "last words"]);
        let dictation = Dictating::recording(
            &surface,
            FakeRecorder {
                silent_stop: true,
                ..FakeRecorder::healthy()
            },
        );

        let segment = dictation.pause_flush();
        assert!(segment.starts_dictation, "the first words of the dictation");

        // The segment ends at the watermark the flush was queued with, read off
        // the microphone through the capture session.
        assert_eq!(dictation.cuts(), [8_000]);

        // The dictation carried on and ended on digital silence, which the
        // flushed segment is what allows: a user who pauses and then presses
        // Stop says nothing in between.
        let calls = dictation.stop();

        // Both segments were inserted, in the order they were spoken: the
        // paused one first, and the one the user stopped with appended after it,
        // which is what a continuation carries.
        assert_eq!(surface.inserted(), ["First words", " last words"]);
        assert!(calls.contains(&Call::ShowBar("transcribing")));
    }

    #[test]
    fn a_capture_failure_while_cutting_a_segment_takes_no_segment() {
        // Capture can break at the cut rather than at Start. The dictation must
        // carry on to its Stop instead of losing the user's speech to a panic or
        // to a segment that silently never arrives.
        let surface = Arc::new(FakeSurface::default());
        let surface = surface.transcribing_as(&["last words"]);
        let dictation = Dictating::recording(
            &surface,
            FakeRecorder {
                fail_cut: true,
                ..FakeRecorder::healthy()
            },
        );

        dictation.microphone.speak_then_pause();
        assert_eq!(dictation.cut_reached_the_microphone(), 8_000);
        let calls = dictation.stop();

        // The cut failed at the queued watermark, so the segment it would have
        // carried was never inserted, and nothing else was asked for one. The
        // failed capture is not reported as a microphone the user has to fix.
        assert!(dictation.cuts().is_empty());
        assert_eq!(surface.inserted(), ["Last words"]);
        assert!(!calls.contains(&Call::NotifyCaptureFailure));
    }

    // ---- one dictation, one session, checked at the boundaries ----

    #[test]
    fn stop_joins_the_level_thread_without_freezing_on_its_watermark_probe() {
        // The lock order that froze the app (slugtale-xqzs): Stop holds the
        // capture lock and joins the voice-level thread, while that thread asks
        // for the capture ring's watermark the moment a Segment Pause elapses.
        // Both used to want the same lock, so each waited for the other and Stop
        // never returned.
        //
        // The recorder here joins the level thread inside `stop`, exactly where a
        // real recorder joins its level emitter, so the old order is forced
        // rather than raced for. Stop runs on its own thread and the test bounds
        // it, so a regression fails as a timeout rather than as a hung suite.
        let surface = Arc::new(FakeSurface::default()).transcribing_as(&["words"]);
        let recorder = FakeRecorder::healthy().joining_the_level_thread_on_stop();
        let dictating = Dictating::recording(&surface, recorder);
        let host = Arc::clone(&dictating.host);

        let (done, settled) = mpsc::channel();
        let stopping = std::thread::spawn(move || {
            host.handle_dictation_event(DictationEvent::Stop)
                .expect("stop succeeds");
            let _ = done.send(());
        });

        assert!(
            settled.recv_timeout(Duration::from_secs(10)).is_ok(),
            "Stop never returned: the level thread and Stop are each waiting for the capture lock"
        );
        stopping.join().expect("the stopping thread finishes");
        assert!(
            dictating.joined_the_level_thread(),
            "the level thread must run to the end while Stop waits for it"
        );
    }

    #[test]
    fn a_segment_keeps_the_target_and_settings_its_dictation_pinned() {
        // A job reads the Settings and the text target its own dictation pinned.
        // Re-reading them per segment is what let a decode that outlived the user's
        // switch land in a different app (slugtale-cbxb, slugtale-7lq0).
        let surface = Arc::new(FakeSurface::default()).transcribing_as(&["first dictation"]);
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());
        surface.clear_calls();

        dictating.microphone.speak_then_pause();
        let counted = dictating
            .counted
            .recv_timeout(Duration::from_secs(5))
            .expect("the Pause Flush settles");
        assert_eq!(counted.words, 2);

        assert_eq!(
            surface
                .calls()
                .into_iter()
                .filter(|call| matches!(call, Call::ReadSettings))
                .count(),
            0,
            "a queued segment must not re-read Settings"
        );
        assert_eq!(
            surface
                .calls()
                .into_iter()
                .filter_map(|call| match call {
                    Call::PreparedInsertion(target) => Some(target),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [Some(TEST_TARGET_PID)],
            "the segment is aimed at the app its dictation began in"
        );
    }

    #[test]
    fn each_dictation_arms_the_pause_its_own_settings_snapshot_holds() {
        // The Segment Pause is part of what a dictation pins at Start: the
        // length is derived from the same Settings snapshot its jobs read, so a
        // save landing while it runs cannot retarget it, and the next dictation
        // arms from the newest saved length. A save used to publish
        // asynchronously to the runtime after persisting, where a delayed older
        // save could overwrite a newer one and the next dictation would flush
        // at a length the Settings File no longer held (slugtale-cbxb).
        let surface = Arc::new(FakeSurface::default());
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        // The fixture's Settings ask for the shortest accepted pause.
        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64)
        );

        // Two overlapping saves land while this dictation runs; the newest is
        // the one the Settings store serialised last.
        surface.set_segment_pause_secs(5);
        surface.set_segment_pause_secs(8);
        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64),
            "the dictation in progress keeps the pause it armed with at Start"
        );

        // The next dictation pins the length the Settings File now holds.
        dictating.start_another_dictation();
        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(8),
            "the next dictation arms from the Settings snapshot it pins"
        );
    }

    #[test]
    fn an_old_pause_flush_drains_no_audio_from_a_newer_dictations_capture() {
        // The interleaving that used to take the new recording's speech: the
        // worker's liveness check passed, and Cancel plus a newer Start landed
        // before the drain. The session travels to the capture lock, which is the
        // lock a concurrent Start holds while it begins capturing, so the ring is
        // asked whether it is still this job's before anything is taken from it.
        let surface = Arc::new(FakeSurface::default()).transcribing_as(&["words"]);
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());
        let stale = running_session(&dictating.host);

        dictating.cancel();
        dictating.start_another_dictation();
        assert_ne!(
            running_session(&dictating.host),
            stale,
            "the replacement dictation now owns the capture"
        );
        assert!(
            dictating.drain_cuts().is_empty(),
            "nothing has been cut yet"
        );

        // The old job reaching the drain boundary now.
        let taken = dictating.host.take_dictation_segment(stale, 8_000);

        assert!(
            taken.is_none(),
            "a stale flush must not take the new recording's audio"
        );
        assert!(
            dictating.drain_cuts().is_empty(),
            "the microphone was never asked to cut anything"
        );

        // And the live session can still drain its own audio, so the guard refuses
        // by session rather than refusing everything.
        assert!(
            dictating
                .host
                .take_dictation_segment(running_session(&dictating.host), 8_000)
                .is_some()
        );
        assert_eq!(dictating.drain_cuts(), [8_000]);
    }

    #[test]
    fn an_old_dictations_completion_never_hides_the_bar_a_new_dictation_showed() {
        // Forced interleaving, the one the review names: the old dictation's final
        // job settles exactly while the replacement dictation is showing its own
        // bar. Deciding to hide and hiding are one operation under the runtime's
        // effects lock, which every lifecycle event also takes, so the hide cannot
        // slip into the gap between the check and the mutation (slugtale-cbxb).
        let (surface, gate) = Arc::new(FakeSurface::default())
            .transcribing_as(&["words from the replaced dictation"])
            .blocking_transcription();
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        // Stop queues the final segment and its decode blocks in the model.
        dictating
            .host
            .handle_dictation_event(DictationEvent::Stop)
            .expect("stop succeeds");
        gate.wait_until_decoding();

        // The user starts dictating again before that decode returns.
        dictating.start_another_dictation();
        surface.clear_calls();
        gate.release();

        // The replacement dictation's own segment settles after the old one,
        // because the worker is single and ordered.
        dictating.microphone.speak_then_pause();
        dictating
            .counted
            .recv_timeout(Duration::from_secs(5))
            .expect("the replacement dictation's flush settles");

        assert!(
            !surface.calls().contains(&Call::HideBar),
            "an old dictation's completion must not hide the new bar: {:?}",
            surface.calls()
        );
    }

    #[test]
    fn a_cancelled_dictation_that_a_new_one_replaced_inserts_nothing_at_all() {
        // The whole of slugtale-cbxb in one run: the dictation's final segment is
        // decoding when the user presses Escape and starts dictating again, the
        // decode finishes afterwards, and nothing it would have done may happen —
        // no words, no clipboard rescue, no Usage count, and no bar hide that
        // would take the new dictation's bar down with it.
        let (surface, gate) = Arc::new(FakeSurface::default())
            .transcribing_as(&[
                "words from the cancelled dictation",
                "words from the replacement dictation",
            ])
            .blocking_transcription();
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        // Stop queues the final segment and the decode goes into the model.
        dictating
            .host
            .handle_dictation_event(DictationEvent::Stop)
            .expect("stop succeeds");
        gate.wait_until_decoding();

        dictating.cancel();
        dictating.start_another_dictation();
        gate.release();

        // The replacement dictation's own segment settles after the old one,
        // because the worker is single and ordered — so by the time it arrives the
        // cancelled dictation's job has run to its end.
        dictating.microphone.speak_then_pause();
        let counted = dictating
            .counted
            .recv_timeout(Duration::from_secs(5))
            .expect("the replacement dictation's flush settles");

        assert_eq!(
            surface.inserted(),
            ["Words from the replacement dictation"],
            "the cancelled dictation's words must never be typed"
        );
        assert!(
            surface.rescued().is_empty(),
            "a cancelled dictation must not rescue either: {:?}",
            surface.rescued()
        );
        let more: Vec<CountedSegment> = dictating.counted.try_iter().collect();
        assert!(
            more.is_empty(),
            "the cancelled dictation must not count a second time: {more:?}"
        );
        assert_eq!(counted.words, 5, "only the replacement dictation counts");
        let hides = surface
            .calls()
            .into_iter()
            .filter(|call| *call == Call::HideBar)
            .count();
        assert_eq!(
            hides, 1,
            "only Cancel clears the bar; the old job's settle must not hide the new one"
        );
    }
}
