//! The dictation lifecycle host: everything that happens between an activation
//! input saying "start" and the Dictation Runtime receiving the captured audio.
//! It owns the recording-feedback state machine, the focus target, the audio
//! capture session, and the runtime handle, and it reaches the rest of the app
//! only through [`DictationSurface`] — one port implemented once by the Tauri
//! shell, and by a fake in tests.

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
/// target, the audio capture session, and the runtime handle. The locks are
/// private so the ordering rules stay inside this module; every method holds a
/// lock no longer than the state move itself, and never reaches a surface or
/// the operating system while holding one.
pub struct DictationHost<R = crate::CpalAudioRecorder> {
    surface: Arc<dyn DictationSurface>,
    feedback: Mutex<crate::RecordingFeedback>,
    /// The process id of the app the user was dictating into, captured when
    /// recording starts so insertion can re-target it after transcription
    /// (slugtale-squ).
    focus_target: Mutex<Option<i32>>,
    capture: Mutex<crate::AudioCaptureSession<R>>,
    runtime_state: Mutex<Option<Arc<crate::DictationRuntime>>>,
}

impl<R> DictationHost<R>
where
    R: crate::DictationRecorder,
{
    pub fn new(surface: Arc<dyn DictationSurface>) -> Self
    where
        R: Default,
    {
        Self::with_recorder(surface, R::default())
    }

    pub fn with_recorder(surface: Arc<dyn DictationSurface>, recorder: R) -> Self {
        Self {
            surface,
            feedback: Mutex::new(crate::RecordingFeedback::default()),
            focus_target: Mutex::new(None),
            capture: Mutex::new(crate::AudioCaptureSession::new(recorder)),
            runtime_state: Mutex::new(None),
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
    pub fn voice_watermark(&self) -> u64 {
        self.capture
            .lock()
            .map(|guard| guard.voice_watermark())
            .unwrap_or(0)
    }

    /// Prepare audio capture while idle so the first Hotkey does not pay for
    /// device discovery and ring allocation (slugtale-g1o.3). Preparation must
    /// never prompt, so callers gate this on an already-granted microphone.
    pub fn prepare_capture(&self) {
        if let Ok(mut guard) = self.capture.lock() {
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

    pub fn handle_dictation_event(
        &self,
        event: crate::DictationEvent,
    ) -> Result<(), String> {
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
                self.capture_focus_target();
                // Open the dictation before capture starts: the level callback
                // installed below stamps every Pause Flush with this number.
                self.runtime().begin();
                // If the microphone cannot start, do not show a recording state.
                self.handle_audio_capture_event(event)?;
                let settings = match activation.take() {
                    Some(activation) => activation.settings,
                    None => self.surface.settings(),
                };
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

    fn capture_focus_target(&self) {
        // Read from the operating system first, so the lock covers the state
        // move and nothing else.
        let target = crate::capture_text_target();
        if let Ok(mut guard) = self.focus_target.lock() {
            *guard = target;
        }
    }

    fn handle_audio_capture_event(
        &self,
        event: crate::DictationEvent,
    ) -> Result<(), String> {
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
                self.surface.record_diagnostic_event(
                    crate::DiagnosticEvent::audio_capture_failed(&error),
                );
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
    /// Runs synchronously on the Dictation Segment worker thread. Everything it
    /// touches is resolved per segment rather than per dictation, so a Settings
    /// change part-way through a long dictation takes effect at the next Segment
    /// Pause instead of being pinned at Start.
    pub fn run_dictation_segment(
        &self,
        audio: crate::CapturedAudio,
        position: crate::DictationSegmentPosition,
    ) -> Result<crate::DictationSegmentOutcome, String> {
        let settings = self.surface.settings();
        let diagnostic_log = self.surface.diagnostic_log(&settings);
        let stack = self.surface.dictation_stack(&settings)?;
        let target_pid = self.focus_target.lock().ok().and_then(|guard| *guard);

        let prepared = self.surface.prepared_insertion(target_pid)?;
        let runtime = stack.asr_runtime();
        let insertion =
            crate::DiagnosticTextInsertion::new(&prepared.insertion, diagnostic_log.clone());
        let rescue =
            crate::DiagnosticInsertionRescue::new(prepared.rescue.as_ref(), diagnostic_log);
        crate::DictationWorkflow::new(
            &runtime,
            &insertion,
            &rescue,
            settings.transcript_cleanup,
        )
        .complete(audio, position)
        .map_err(|error| error.to_string())
    }

    /// Take the speech captured so far as a Dictation Segment, leaving the
    /// microphone running. Called only from the worker thread. `cut` is the sample
    /// watermark the Pause Flush was queued with: the segment ends there (plus a
    /// small acoustic guard), whatever else has arrived since.
    pub fn take_dictation_segment(&self, cut: u64) -> Option<crate::CapturedAudio> {
        let flushed = self
            .capture
            .lock()
            .map_err(|_| "audio capture mutex poisoned".to_string())
            .and_then(|mut guard| guard.cut_segment(cut).map_err(|error| error.to_string()));

        match flushed {
            Ok(audio) => audio,
            Err(error) => {
                eprintln!("could not take dictation segment: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioCaptureError, CapturedAudio, CountedSegment, DictationEvent, DictationRecorder,
        DictationRuntime, DictationRuntimeHost, DictationSegmentOutcome, DictationSegmentPosition,
        EngineAvailability, EngineConfidence, EngineMetadata, EngineTranscription,
        FileDiagnosticSink, FinalTranscription, InsertionRescue, InsertionRescueError,
        PreparedInsertion, SettledTextInsertion, SharedDiagnosticLog, TextInsertion,
        TextInsertionError, TranscriptionProvider, SEGMENT_VOICE_LEVEL,
    };
    use std::sync::{mpsc, Weak};
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
    }

    #[derive(Default, Clone)]
    struct FakeSurface {
        calls: Arc<std::sync::Mutex<Vec<Call>>>,
        /// What the fake Transcription Engine answers with, one entry per
        /// segment and the last one repeating, so a test names only the
        /// segments it reads.
        transcriptions: Arc<std::sync::Mutex<Vec<String>>>,
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        rescued: Arc<std::sync::Mutex<Vec<String>>>,
        /// Spendable, so a test can fail one segment's insertion without failing
        /// every later one too.
        fail_insertion: Arc<std::sync::Mutex<bool>>,
    }

    impl FakeSurface {
        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
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

        fn fail_insertion(&self) {
            *self.fail_insertion.lock().unwrap() = true;
        }
    }

    impl DictationSurface for FakeSurface {
        fn settings(&self) -> crate::Settings {
            self.record(Call::ReadSettings);
            crate::Settings::default()
        }

        fn record_diagnostic_event(&self, event: crate::DiagnosticEvent) {
            let tag = match event {
                crate::DiagnosticEvent::HotkeyTransition { .. } => "hotkey_transition",
                crate::DiagnosticEvent::AudioCaptureFailed { .. } => "audio_capture_failed",
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
            let engine: Arc<dyn TranscriptionProvider> =
                Arc::new(FakeEngine(self.next_transcription()));
            let router = crate::SecondOpinionRouter::single(engine);
            Ok(crate::DictationStack::new(
                router,
                SharedDiagnosticLog::new(false, FileDiagnosticSink::unavailable()),
            ))
        }

        fn prepared_insertion(
            &self,
            _target_pid: Option<i32>,
        ) -> Result<PreparedInsertion, String> {
            Ok(PreparedInsertion {
                insertion: SettledTextInsertion::new(
                    Box::new(RecordingInsertion {
                        inserted: self.inserted.clone(),
                        fails: self.fail_insertion.clone(),
                    }),
                    None,
                ),
                rescue: Box::new(RecordingRescue {
                    rescued: self.rescued.clone(),
                }),
            })
        }
    }

    /// A Transcription Engine that answers with the text it was handed, so the
    /// Dictation Workflow downstream of it is the real one.
    struct FakeEngine(String);

    impl TranscriptionProvider for FakeEngine {
        fn engine(&self) -> crate::TranscriptionEngine {
            crate::TranscriptionEngine::Whisper
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: crate::TranscriptionEngine::Whisper,
                model_id: "test",
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
            Ok(EngineTranscription {
                engine: crate::TranscriptionEngine::Whisper,
                transcription: FinalTranscription::plain(self.0.clone()),
                alternatives: Vec::new(),
                confidence: EngineConfidence::unreported(),
                latency: Duration::ZERO,
            })
        }
    }

    struct RecordingInsertion {
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        fails: Arc<std::sync::Mutex<bool>>,
    }

    impl TextInsertion for RecordingInsertion {
        fn insert(&self, transcription: &FinalTranscription) -> Result<(), TextInsertionError> {
            self.inserted
                .lock()
                .unwrap()
                .push(transcription.text.clone());
            if std::mem::take(&mut *self.fails.lock().unwrap()) {
                Err(TextInsertionError::new("fake insertion failure"))
            } else {
                Ok(())
            }
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
    /// the host installed, so a test drives the Dictation Bar and the Segment
    /// Pause detector through the same callback the audio emitter thread calls.
    #[derive(Clone, Default)]
    struct FakeMicrophone {
        level: Arc<std::sync::Mutex<Option<crate::AudioLevelCallback>>>,
        /// The ring position of the last voiced sample, moved forward whenever
        /// speech is heard so a Pause Flush has a cut worth queueing.
        watermark: Arc<std::sync::atomic::AtomicU64>,
    }

    impl FakeMicrophone {
        /// A level the Dictation Bar treats as speech, which both flexes the
        /// waveform and holds a Segment Pause open.
        fn speaking() -> f32 {
            SEGMENT_VOICE_LEVEL + 0.2
        }

        /// Speak, then stay quiet long enough for the Segment Pause to elapse.
        /// The pause is a real clock the test would otherwise wait five seconds
        /// on, so the runtime is started with a short one.
        fn speak_then_pause(&self) {
            self.voice(Self::speaking());
            std::thread::sleep(TEST_PAUSE * 4);
            self.voice(0.0);
        }

        fn voice(&self, level: f32) {
            if level > SEGMENT_VOICE_LEVEL {
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
    /// when a Pause Flush asks it for a segment.
    struct FakeRecorder {
        fail_start: bool,
        silent_stop: bool,
        fail_cut: bool,
        microphone: FakeMicrophone,
        /// Every cut a Pause Flush asked the microphone for. A channel rather
        /// than a shared log so a test can wait for the cut instead of racing
        /// the Stop that would end the dictation first.
        cuts: mpsc::Sender<u64>,
    }

    impl FakeRecorder {
        fn healthy() -> Self {
            Self {
                fail_start: false,
                silent_stop: false,
                fail_cut: false,
                microphone: FakeMicrophone::default(),
                cuts: mpsc::channel().0,
            }
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
            if self.fail_start {
                return Err(AudioCaptureError::new("fake start failure"));
            }
            Ok(())
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(self.captured())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            Ok(())
        }

        fn cut_segment(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            let _ = self.cuts.send(cut);
            if self.fail_cut {
                return Err(AudioCaptureError::new("fake cut failure"));
            }
            Ok(self.captured())
        }

        fn voice_watermark(&self) -> u64 {
            self.microphone
                .watermark
                .load(std::sync::atomic::Ordering::Relaxed)
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

    /// The Dictation Runtime's host half, wired to the very host the level
    /// callback talks to. This is the delegation `AppHost` performs in the app,
    /// so a Pause Flush crosses the same two interfaces it crosses there.
    struct PausingRuntimeHost {
        host: Weak<DictationHost<FakeRecorder>>,
        surface: Arc<FakeSurface>,
    }

    impl PausingRuntimeHost {
        fn host(&self) -> Option<Arc<DictationHost<FakeRecorder>>> {
            self.host.upgrade()
        }
    }

    impl DictationRuntimeHost for PausingRuntimeHost {
        fn take_pause_segment(&mut self, cut: u64) -> Option<CapturedAudio> {
            self.host()?.take_dictation_segment(cut)
        }

        fn complete(
            &mut self,
            audio: CapturedAudio,
            position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            self.host()
                .ok_or_else(|| "the test host is gone".to_string())?
                .run_dictation_segment(audio, position)
        }

        fn last_job_settled(&mut self) {
            self.surface.hide_dictation_bar();
        }
    }

    /// A Segment Pause short enough for a test to sit through. The rule under it
    /// is the one the five-second default drives.
    const TEST_PAUSE: Duration = Duration::from_millis(30);

    /// A runtime host that reaches nothing, for the tests that only exercise the
    /// lifecycle events and never reach a Dictation Segment.
    struct UnreachableRuntimeHost;

    impl DictationRuntimeHost for UnreachableRuntimeHost {
        fn take_pause_segment(&mut self, _cut: u64) -> Option<CapturedAudio> {
            None
        }

        fn complete(
            &mut self,
            _audio: CapturedAudio,
            _position: DictationSegmentPosition,
        ) -> Result<DictationSegmentOutcome, String> {
            Err("test host never transcribes".to_string())
        }

        fn last_job_settled(&mut self) {}
    }

    fn started_runtime() -> Arc<DictationRuntime> {
        Arc::new(
            DictationRuntime::start(
                UnreachableRuntimeHost,
                || 0,
                Arc::new(|_: crate::LocalDate, _: CountedSegment| {}),
            )
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
            let host = Arc::new(DictationHost::with_recorder(surface.clone(), recorder));
            let (counted_tx, counted) = mpsc::channel();
            // The watermark the runtime probes is the host's own read of the
            // microphone, so a queued flush carries the position the capture
            // session reported rather than a number the test made up.
            let watermark = Arc::downgrade(&host);
            let runtime = DictationRuntime::start_with_pause(
                PausingRuntimeHost {
                    host: Arc::downgrade(&host),
                    surface: surface.clone(),
                },
                Arc::new(move || {
                    watermark
                        .upgrade()
                        .map(|host| host.voice_watermark())
                        .unwrap_or(0)
                }),
                Arc::new(move |_: crate::LocalDate, segment: CountedSegment| {
                    let _ = counted_tx.send(segment);
                }),
                TEST_PAUSE,
            )
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

        /// Every cut a Pause Flush has asked for so far, in order.
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
        let host = DictationHost::with_recorder(surface.clone(), recorder);
        host.set_runtime(started_runtime()).unwrap();
        host
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
    fn a_second_segment_pause_after_an_insertion_rescue_takes_nothing() {
        // A rescue means the text did not reach the text target, so the next
        // pause must not bury the rescued words under more of them.
        let surface = Arc::new(FakeSurface::default());
        let surface = surface.transcribing_as(&["rescued words", "after the rescue"]);
        surface.fail_insertion();
        let dictation = Dictating::recording(&surface, FakeRecorder::healthy());

        let segment = dictation.pause_flush();
        assert!(segment.starts_dictation);
        assert_eq!(surface.rescued(), ["Rescued words"]);

        dictation.microphone.speak_then_pause();
        dictation.stop();

        // One cut only: the flush after the rescue was not even queued, so the
        // microphone was never asked for a segment. The dictation's own last
        // segment is unaffected — that is what ADR-0026 promises.
        assert_eq!(dictation.cuts(), [8_000]);
        assert_eq!(surface.inserted(), ["Rescued words", " after the rescue"]);
        assert_eq!(surface.rescued(), ["Rescued words"]);
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
}
