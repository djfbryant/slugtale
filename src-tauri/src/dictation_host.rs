
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

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

#[derive(Clone)]
struct DictationSession {
    session: u64,
    settings: crate::Settings,
    target_pid: Option<i32>,
}

pub trait DictationEffects: Send + Sync {
    fn settings(&self) -> crate::Settings;
    fn record_diagnostic_event(&self, event: crate::DiagnosticEvent);
    fn show_dictation_bar(&self, phase: DictationPhase, settings: &crate::Settings);
    fn hide_dictation_bar(&self);
    fn emit_dictation_audio_level(&self, level: f32);
    fn notify_capture_failure(&self, error: &str);
    fn play_dictation_sound(&self, sound: crate::DictationSound);
}

pub trait DictationParts: Send + Sync {
    fn diagnostic_log(&self, settings: &crate::Settings) -> crate::ErasedDiagnosticLog;

    fn dictation_stack(
        &self,
        settings: &crate::Settings,
    ) -> Result<crate::DictationStack<crate::BoxedDiagnosticSink>, String>;

    fn prepared_insertion(
        &self,
        target_pid: Option<i32>,
    ) -> Result<crate::PreparedInsertion, String>;
}
pub struct DictationHost<R = crate::CpalAudioRecorder> {
    effects: Arc<dyn DictationEffects>,
    parts: Arc<dyn DictationParts>,
    feedback: Mutex<crate::RecordingFeedback>,
    focus_target_source: Arc<dyn Fn() -> Option<i32> + Send + Sync>,
    capture: Mutex<crate::AudioCaptureSession<R>>,
    session: Mutex<Option<DictationSession>>,
    voice_watermark: Arc<std::sync::atomic::AtomicU64>,
    runtime_state: Mutex<Option<Arc<crate::DictationRuntime>>>,
    usage: Arc<crate::UsageQueue>,
}

impl<R> DictationHost<R>
where
    R: crate::DictationRecorder,
{
    pub fn new(
        effects: Arc<dyn DictationEffects>,
        parts: Arc<dyn DictationParts>,
        usage: Arc<crate::UsageQueue>,
    ) -> Self
    where
        R: Default,
    {
        Self::with_recorder(effects, parts, R::default(), usage)
    }

    pub fn with_recorder(
        effects: Arc<dyn DictationEffects>,
        parts: Arc<dyn DictationParts>,
        recorder: R,
        usage: Arc<crate::UsageQueue>,
    ) -> Self {
        Self::with_recorder_and_focus_target_source(
            effects,
            parts,
            recorder,
            usage,
            Arc::new(crate::capture_text_target),
        )
    }

    pub fn with_recorder_and_focus_target_source(
        effects: Arc<dyn DictationEffects>,
        parts: Arc<dyn DictationParts>,
        recorder: R,
        usage: Arc<crate::UsageQueue>,
        focus_target_source: Arc<dyn Fn() -> Option<i32> + Send + Sync>,
    ) -> Self {
        let voice_watermark = recorder.voice_watermark_cell();
        Self {
            effects,
            parts,
            feedback: Mutex::new(crate::RecordingFeedback::default()),
            focus_target_source,
            capture: Mutex::new(crate::AudioCaptureSession::new(recorder)),
            session: Mutex::new(None),
            voice_watermark,
            runtime_state: Mutex::new(None),
            usage,
        }
    }

    pub fn set_runtime(&self, runtime: Arc<crate::DictationRuntime>) -> Result<(), String> {
        let mut guard = self
            .runtime_state
            .lock()
            .map_err(|_| "dictation runtime state mutex poisoned".to_string())?;
        *guard = Some(runtime);
        Ok(())
    }

    pub fn voice_watermark(&self) -> u64 {
        self.voice_watermark.load(Ordering::Acquire)
    }

    pub fn prepare_capture(&self, settings: &crate::Settings) {
        if let Ok(mut guard) = self.capture.lock() {
            guard.set_prefer_built_in_microphone(settings.prefer_built_in_microphone);
            let _ = guard.prepare();
        }
    }

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

    pub fn handle_dictation_event_with(
        &self,
        event: crate::DictationEvent,
        mut activation: Option<crate::DictationActivation>,
    ) -> Result<(), String> {
        self.effects
            .record_diagnostic_event(crate::DiagnosticEvent::hotkey_transition(event));

        match event {
            crate::DictationEvent::Start => {
                let target_pid = self.frontmost_app();
                let settings = match activation.take() {
                    Some(activation) => activation.settings,
                    None => self.effects.settings(),
                };
                let session = self.runtime().begin(crate::segment_pause_duration(&settings));
                self.pin_session(DictationSession {
                    session,
                    settings: settings.clone(),
                    target_pid,
                });
                if let Ok(mut guard) = self.capture.lock() {
                    guard.set_prefer_built_in_microphone(settings.prefer_built_in_microphone);
                }
                self.handle_audio_capture_event(event)?;
                self.apply_recording_feedback(event, Some(&settings))?;
            }
            crate::DictationEvent::Stop => {
                self.advance_recording_feedback(event)?;
                let settings = self.effects.settings();
                self.handle_audio_capture_event_with_settings(event, Some(&settings))?;
            }
            crate::DictationEvent::Cancel => {
                self.runtime().abandon();
                self.apply_recording_feedback(event, None)?;
                self.handle_audio_capture_event(event)?;
            }
        }

        Ok(())
    }

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
            self.effects.play_dictation_sound(sound);
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
            let owned;
            let settings = match settings {
                Some(settings) => settings,
                None => {
                    owned = self.effects.settings();
                    &owned
                }
            };
            self.effects
                .show_dictation_bar(DictationPhase::Recording, settings);
        } else {
            self.effects.hide_dictation_bar();
        }

        Ok(())
    }

    fn frontmost_app(&self) -> Option<i32> {
        (self.focus_target_source)()
    }

    fn pin_session(&self, pinned: DictationSession) {
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some(pinned);
        }
    }

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

    fn handle_audio_capture_event_with_settings(
        &self,
        event: crate::DictationEvent,
        bar_settings: Option<&crate::Settings>,
    ) -> Result<(), String> {
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
                self.effects.hide_dictation_bar();
                self.effects
                    .record_diagnostic_event(crate::DiagnosticEvent::audio_capture_failed(&error));
                #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
                self.effects.notify_capture_failure(&error.to_string());
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
                let owned;
                let bar_settings = match bar_settings {
                    Some(settings) => settings,
                    None => {
                        owned = self.effects.settings();
                        &owned
                    }
                };
                self.effects
                    .show_dictation_bar(DictationPhase::Transcribing, bar_settings);
                let queued = self.runtime().send_last(audio);
                if !queued {
                    eprintln!("dictation segment worker is unavailable; dropping final segment");
                    self.effects.hide_dictation_bar();
                }
            }
            Some(crate::AudioCaptureOutcome::Discarded) => {
                self.clear_dictation_audio_level_callback();
                eprintln!("discarded dictation audio");
                self.effects.hide_dictation_bar();
            }
            None => {
                if matches!(event, crate::DictationEvent::Stop) {
                    self.effects.hide_dictation_bar();
                }
            }
        }

        Ok(())
    }

    fn dictation_audio_level_callback(&self) -> crate::AudioLevelCallback {
        let effects = self.effects.clone();
        let runtime = self.runtime();
        Arc::new(move |level| {
            effects.emit_dictation_audio_level(level);
            runtime.on_voice_level(level);
        })
    }

    fn clear_dictation_audio_level_callback(&self) {
        if let Ok(mut guard) = self.capture.lock() {
            guard.set_level_callback(None);
        }
        self.effects.emit_dictation_audio_level(0.0);
    }

    pub fn run_dictation_segment(
        &self,
        session: u64,
        audio: crate::CapturedAudio,
        position: crate::DictationSegmentPosition,
    ) -> Result<crate::DictationSegmentOutcome, String> {
        let pinned = self.pinned_session(session)?;
        let settings = &pinned.settings;
        let stack = self.parts.dictation_stack(settings)?;

        let runtime = self.runtime();
        let mut prepared = self.parts.prepared_insertion(pinned.target_pid)?;
        prepared.guard_with_session(session, runtime.session_effects());

        let completed = crate::DictationWorkflow::new(
            &stack,
            &prepared.insertion,
            prepared.rescue.as_ref(),
            settings.transcript_cleanup,
        )
        .complete(audio, position);

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
                    self.effects.as_ref(),
                    outcome.insertion_failure.as_ref(),
                    outcome.rescued,
                );
                Ok(outcome)
            }
            Err(error) => {
                record_insertion_diagnostics(
                    self.effects.as_ref(),
                    error.insertion_failure(),
                    false,
                );
                Err(error.to_string())
            }
        }
    }

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

    fn owns_capture(&self, session: u64) -> bool {
        self.session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|pinned| pinned.session))
            == Some(session)
    }
}

fn record_insertion_diagnostics(
    effects: &dyn DictationEffects,
    insertion_failure: Option<&crate::TextInsertionError>,
    rescued: bool,
) {
    if let Some(error) = insertion_failure {
        effects.record_diagnostic_event(crate::DiagnosticEvent::insertion_failed(error));
    }
    if rescued {
        effects.record_diagnostic_event(crate::DiagnosticEvent::insertion_rescued());
    }
}

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

    fn last_job_settled(&self, _session: u64) {
        self.effects.hide_dictation_bar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioCaptureError, BoxedDiagnosticSink, CapturedAudio, CountedSegment, DictationEvent,
        DictationRecorder, DictationRuntime, DictationRuntimeHost, DictationSegmentOutcome,
        DictationSegmentPosition, EngineAssetLifecycle, EngineAvailability, EngineConfidence,
        EngineMetadata, EngineTranscriber, EngineTranscription, FinalTranscription,
        InsertionRescue, InsertionRescueError, PreparedInsertion, SettledTextInsertion,
        TextInsertion, TextInsertionError, TranscriptionProvider, VOICE_LEVEL,
    };
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Diagnostic(&'static str),
        PlaySound(&'static str),
        ShowBar(&'static str),
        HideBar,
        ClearAudioLevel,
        ReadSettings,
        NotifyCaptureFailure,
        PreparedInsertion(Option<i32>),
    }

    #[derive(Clone)]
    struct FakeSurface {
        calls: Arc<std::sync::Mutex<Vec<Call>>>,
        transcriptions: Arc<std::sync::Mutex<Vec<String>>>,
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        rescued: Arc<std::sync::Mutex<Vec<String>>>,
        insertion_fails: Arc<std::sync::atomic::AtomicBool>,
        decoding: Option<Arc<DecodeGate>>,
        settings: Arc<std::sync::Mutex<crate::Settings>>,
        log_lines: Arc<std::sync::Mutex<Vec<String>>>,
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
                log_lines: Arc::default(),
            }
        }
    }

    #[derive(Default)]
    struct DecodeGate {
        entered: Arc<(Mutex<bool>, std::sync::Condvar)>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl DecodeGate {
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

        fn set_segment_pause_secs(&self, secs: i64) {
            self.settings.lock().unwrap().segment_pause_secs = secs;
        }

        fn blocking_transcription(self: &Arc<Self>) -> (Arc<Self>, Arc<DecodeGate>) {
            let gate = Arc::new(DecodeGate::default());
            let mut surface = (**self).clone();
            surface.decoding = Some(Arc::clone(&gate));
            (Arc::new(surface), gate)
        }

        fn log_lines(&self) -> Vec<String> {
            self.log_lines.lock().unwrap().clone()
        }

        fn with_diagnostic_logging(self: &Arc<Self>) -> Arc<Self> {
            self.settings.lock().unwrap().diagnostic_logging = true;
            self.clone()
        }
    }

    impl DictationEffects for FakeSurface {
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
    }

    impl DictationParts for FakeSurface {
        fn diagnostic_log(&self, settings: &crate::Settings) -> crate::ErasedDiagnosticLog {
            let lines = Arc::clone(&self.log_lines);
            crate::ErasedDiagnosticLog::new(
                settings.diagnostic_logging,
                BoxedDiagnosticSink::new(move |line: &str| {
                    lines.lock().unwrap().push(line.to_string());
                }),
            )
        }

        fn dictation_stack(
            &self,
            settings: &crate::Settings,
        ) -> Result<crate::DictationStack<BoxedDiagnosticSink>, String> {
            let gate = self.decoding.clone();
            let engine: Arc<dyn TranscriptionProvider> = Arc::new(FakeEngine {
                text: self.next_transcription(),
                gate,
            });
            let router = crate::SecondOpinionRouter::single(engine);
            Ok(crate::DictationStack::new(
                router,
                self.diagnostic_log(settings),
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

    #[derive(Clone, Default)]
    struct FakeMicrophone {
        cuts: Arc<std::sync::Mutex<Vec<u64>>>,
        level: Arc<std::sync::Mutex<Option<crate::AudioLevelCallback>>>,
        watermark: Arc<std::sync::atomic::AtomicU64>,
        joined_the_level_thread: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeMicrophone {
        fn speaking() -> f32 {
            VOICE_LEVEL + 0.2
        }

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

    struct FakeRecorder {
        fail_start: bool,
        silent_stop: bool,
        fail_cut: bool,
        microphone: FakeMicrophone,
        cuts: mpsc::Sender<u64>,
        started_preferring_built_in: Arc<Mutex<Option<bool>>>,
        prefer_built_in: bool,
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

        fn join_the_level_thread(&self) {
            if !self.join_the_level_thread_on_stop {
                return;
            }
            let Some(callback) = self.microphone.level.lock().unwrap().clone() else {
                return;
            };
            std::thread::spawn(move || {
                callback(FakeMicrophone::speaking());
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

    const TEST_SEGMENT_PAUSE_SECS: i64 = 2;
    const TEST_PAUSE: Duration = Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64);
    const PAUSE_SLOP: Duration = Duration::from_millis(300);

    fn focus_target_source() -> Arc<dyn Fn() -> Option<i32> + Send + Sync> {
        Arc::new(|| Some(TEST_TARGET_PID))
    }

    const TEST_TARGET_PID: i32 = 4_242;

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
            let effects: Arc<dyn DictationEffects> = surface.clone();
            let parts: Arc<dyn DictationParts> = surface.clone();
            let host = Arc::new(DictationHost::with_recorder_and_focus_target_source(
                effects,
                parts,
                recorder,
                crate::UsageQueue::start(Arc::new(
                    move |_: crate::LocalDate, segment: CountedSegment| {
                        let _ = counted_tx.send(segment);
                    },
                ))
                .expect("the usage writer starts"),
                focus_target_source(),
            ));
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

        fn drain_cuts(&self) -> Vec<u64> {
            self.microphone.cuts.lock().unwrap().clone()
        }

        fn joined_the_level_thread(&self) -> bool {
            self.microphone
                .joined_the_level_thread
                .load(std::sync::atomic::Ordering::SeqCst)
        }

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

        fn cuts(&self) -> Vec<u64> {
            self.cuts.try_iter().collect()
        }

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
        let effects: Arc<dyn DictationEffects> = surface.clone();
        let parts: Arc<dyn DictationParts> = surface.clone();
        let host = DictationHost::with_recorder_and_focus_target_source(
            effects,
            parts,
            recorder,
            discarding_usage_queue(),
            focus_target_source(),
        );
        host.set_runtime(started_runtime()).unwrap();
        host
    }

    fn running_session(host: &DictationHost<FakeRecorder>) -> u64 {
        host.session
            .lock()
            .unwrap()
            .as_ref()
            .expect("a dictation is running")
            .session
    }

    #[test]
    fn a_segment_logs_through_whatever_sink_the_parts_role_supplies() {
        let secret = "do not log these dictated words";
        let surface = Arc::new(FakeSurface::default())
            .transcribing_as(&[secret])
            .with_diagnostic_logging();
        let host = host_with(&surface, FakeRecorder::healthy());
        host.handle_dictation_event(DictationEvent::Start).unwrap();
        let session = running_session(&host);

        host.run_dictation_segment(
            session,
            CapturedAudio::mono_16khz(vec![0.0, 0.25]),
            DictationSegmentPosition::First,
        )
        .expect("insertion succeeded");

        let lines = surface.log_lines();
        assert!(
            lines.iter().any(|line| line.contains("asr:")),
            "the segment's diagnostics went nowhere: {lines:?}"
        );
        for line in &lines {
            assert!(!line.contains(secret), "leaked transcript text: {line}");
        }
    }

    #[test]
    fn a_rescued_segment_records_both_the_failure_and_the_rescue() {
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
        let transcribing = calls
            .iter()
            .position(|call| *call == Call::ShowBar("transcribing"))
            .expect("stop shows the transcribing state");
        assert!(!calls[recording..transcribing].contains(&Call::HideBar));
        assert!(calls.contains(&Call::PlaySound("stop")));
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


    #[test]
    fn a_segment_pause_reaches_the_text_target_while_the_dictation_runs_on() {
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

        assert_eq!(dictation.cuts(), [8_000]);

        let calls = dictation.stop();

        assert_eq!(surface.inserted(), ["First words", " last words"]);
        assert!(calls.contains(&Call::ShowBar("transcribing")));
    }

    #[test]
    fn a_capture_failure_while_cutting_a_segment_takes_no_segment() {
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

        assert!(dictation.cuts().is_empty());
        assert_eq!(surface.inserted(), ["Last words"]);
        assert!(!calls.contains(&Call::NotifyCaptureFailure));
    }


    #[test]
    fn stop_joins_the_level_thread_without_freezing_on_its_watermark_probe() {
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
        let surface = Arc::new(FakeSurface::default());
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64)
        );

        surface.set_segment_pause_secs(5);
        surface.set_segment_pause_secs(8);
        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(TEST_SEGMENT_PAUSE_SECS as u64),
            "the dictation in progress keeps the pause it armed with at Start"
        );

        dictating.start_another_dictation();
        assert_eq!(
            dictating.host.runtime().armed_pause(),
            Duration::from_secs(8),
            "the next dictation arms from the Settings snapshot it pins"
        );
    }

    #[test]
    fn an_old_pause_flush_drains_no_audio_from_a_newer_dictations_capture() {
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

        let taken = dictating.host.take_dictation_segment(stale, 8_000);

        assert!(
            taken.is_none(),
            "a stale flush must not take the new recording's audio"
        );
        assert!(
            dictating.drain_cuts().is_empty(),
            "the microphone was never asked to cut anything"
        );

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
        let (surface, gate) = Arc::new(FakeSurface::default())
            .transcribing_as(&["words from the replaced dictation"])
            .blocking_transcription();
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        dictating
            .host
            .handle_dictation_event(DictationEvent::Stop)
            .expect("stop succeeds");
        gate.wait_until_decoding();

        dictating.start_another_dictation();
        surface.clear_calls();
        gate.release();

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
        let (surface, gate) = Arc::new(FakeSurface::default())
            .transcribing_as(&[
                "words from the cancelled dictation",
                "words from the replacement dictation",
            ])
            .blocking_transcription();
        let dictating = Dictating::recording(&surface, FakeRecorder::healthy());

        dictating
            .host
            .handle_dictation_event(DictationEvent::Stop)
            .expect("stop succeeds");
        gate.wait_until_decoding();

        dictating.cancel();
        dictating.start_another_dictation();
        gate.release();

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
