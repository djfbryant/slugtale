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
    fn settings(&self) -> slugtale_lib::Settings;
    fn record_diagnostic_event(&self, event: slugtale_lib::DiagnosticEvent);
    fn show_dictation_bar(&self, phase: DictationPhase, settings: &slugtale_lib::Settings);
    fn hide_dictation_bar(&self);
    fn emit_dictation_audio_level(&self, level: f32);
    fn notify_capture_failure(&self, error: &str);
    fn play_dictation_sound(&self, sound: slugtale_lib::DictationSound);
    fn diagnostic_log(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> slugtale_lib::SharedDiagnosticLog<slugtale_lib::FileDiagnosticSink>;
    fn dictation_stack(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Result<slugtale_lib::DictationStack<slugtale_lib::FileDiagnosticSink>, String>;
}
/// The dictation lifecycle's one owner of state: recording feedback, the focus
/// target, the audio capture session, and the runtime handle. The locks are
/// private so the ordering rules stay inside this module; every method holds a
/// lock no longer than the state move itself and never across a surface call.
pub struct DictationHost<R = slugtale_lib::CpalAudioRecorder> {
    surface: Arc<dyn DictationSurface>,
    feedback: Mutex<slugtale_lib::RecordingFeedback>,
    /// The process id of the app the user was dictating into, captured when
    /// recording starts so insertion can re-target it after transcription
    /// (slugtale-squ).
    focus_target: Mutex<Option<i32>>,
    capture: Mutex<slugtale_lib::AudioCaptureSession<R>>,
    runtime_state: Mutex<Option<Arc<slugtale_lib::DictationRuntime>>>,
}

impl<R> DictationHost<R>
where
    R: slugtale_lib::AudioRecorder,
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
            feedback: Mutex::new(slugtale_lib::RecordingFeedback::default()),
            focus_target: Mutex::new(None),
            capture: Mutex::new(slugtale_lib::AudioCaptureSession::new(recorder)),
            runtime_state: Mutex::new(None),
        }
    }

    /// Install the runtime once setup has started it. Every lifecycle call
    /// before this would find nothing able to record, so setup orders this
    /// ahead of any activation input.
    pub fn set_runtime(&self, runtime: Arc<slugtale_lib::DictationRuntime>) -> Result<(), String> {
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
            .ok()
            .map(|guard| slugtale_lib::AudioRecorder::voice_watermark(guard.recorder()))
            .unwrap_or(0)
    }

    /// Prepare audio capture while idle so the first Hotkey does not pay for
    /// device discovery and ring allocation (slugtale-g1o.3). Preparation must
    /// never prompt, so callers gate this on an already-granted microphone.
    pub fn prepare_capture(&self) {
        if let Ok(mut guard) = self.capture.lock() {
            let _ = slugtale_lib::AudioRecorder::prepare(guard.recorder_mut());
        }
    }

    /// The app's one Dictation Runtime.
    ///
    /// # Panics
    /// Before setup has started the runtime; nothing reaches this module
    /// before that.
    pub fn runtime(&self) -> Arc<slugtale_lib::DictationRuntime> {
        self.runtime_state
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .expect("dictation runtime started")
    }

    pub fn handle_dictation_event(
        &self,
        event: slugtale_lib::DictationEvent,
    ) -> Result<(), String> {
        self.handle_dictation_event_with(event, None)
    }

    /// `activation` is the snapshot a Hotkey press built for its readiness gate;
    /// Start consumes it so the rest of the activation reuses the same Settings
    /// value instead of reloading (slugtale-g1o.6). Callers without one — Cancel
    /// from the tray, tests — pass `None`.
    pub fn handle_dictation_event_with(
        &self,
        event: slugtale_lib::DictationEvent,
        mut activation: Option<slugtale_lib::DictationActivation>,
    ) -> Result<(), String> {
        self.surface
            .record_diagnostic_event(slugtale_lib::DiagnosticEvent::hotkey_transition(event));

        match event {
            slugtale_lib::DictationEvent::Start => {
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
            slugtale_lib::DictationEvent::Stop => {
                self.advance_recording_feedback(event)?;
                let settings = self.surface.settings();
                self.handle_audio_capture_event_with_settings(event, Some(&settings))?;
            }
            // Cancel clears the bar immediately and discards the audio. It also
            // drops any Dictation Segment still queued, so nothing further is typed
            // after the user asks Slugtale to stop. Text inserted by an earlier
            // Segment Pause is not undone (ADR-0014). It reads no Settings at all.
            slugtale_lib::DictationEvent::Cancel => {
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
        event: slugtale_lib::DictationEvent,
    ) -> Result<slugtale_lib::RecordingFeedbackEffect, String> {
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
        event: slugtale_lib::DictationEvent,
        settings: Option<&slugtale_lib::Settings>,
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
        if let Ok(mut guard) = self.focus_target.lock() {
            *guard = slugtale_lib::capture_text_target();
        }
    }

    fn handle_audio_capture_event(
        &self,
        event: slugtale_lib::DictationEvent,
    ) -> Result<(), String> {
        self.handle_audio_capture_event_with_settings(event, None)
    }

    /// `bar_settings` is needed only when a Stop completes and the bar switches to
    /// its transcribing state; passing it in spares that path a Settings reload
    /// (slugtale-g1o.6).
    fn handle_audio_capture_event_with_settings(
        &self,
        event: slugtale_lib::DictationEvent,
        bar_settings: Option<&slugtale_lib::Settings>,
    ) -> Result<(), String> {
        let outcome = {
            let mut guard = self
                .capture
                .lock()
                .map_err(|_| "audio capture mutex poisoned".to_string())?;
            if matches!(event, slugtale_lib::DictationEvent::Start) {
                guard
                    .recorder_mut()
                    .set_level_callback(Some(self.dictation_audio_level_callback()));
            }
            guard.on_event(event)
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.clear_dictation_audio_level_callback();
                self.surface.hide_dictation_bar();
                self.surface.record_diagnostic_event(
                    slugtale_lib::DiagnosticEvent::audio_capture_failed(&error),
                );
                #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
                self.surface.notify_capture_failure(&error.to_string());
                return Err(error.to_string());
            }
        };

        match outcome {
            Some(slugtale_lib::AudioCaptureOutcome::Completed(audio)) => {
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
            Some(slugtale_lib::AudioCaptureOutcome::Discarded) => {
                self.clear_dictation_audio_level_callback();
                eprintln!("discarded dictation audio");
                self.surface.hide_dictation_bar();
            }
            // No active session to drain. A terminal event still clears any bar left
            // on screen (e.g. Stop with nothing captured); Start has none to hide.
            None => {
                if matches!(event, slugtale_lib::DictationEvent::Stop) {
                    self.surface.hide_dictation_bar();
                }
            }
        }

        Ok(())
    }

    /// The Segment Pause detector lives inside the Dictation Runtime, which
    /// re-arms it on every begin(), so each dictation starts unable to flush.
    fn dictation_audio_level_callback(&self) -> slugtale_lib::AudioLevelCallback {
        let surface = self.surface.clone();
        let runtime = self.runtime();
        Arc::new(move |level| {
            surface.emit_dictation_audio_level(level);
            runtime.on_voice_level(level);
        })
    }

    fn clear_dictation_audio_level_callback(&self) {
        if let Ok(mut guard) = self.capture.lock() {
            guard.recorder_mut().set_level_callback(None);
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
        audio: slugtale_lib::CapturedAudio,
        position: slugtale_lib::DictationSegmentPosition,
    ) -> Result<slugtale_lib::DictationSegmentOutcome, String> {
        let settings = self.surface.settings();
        let diagnostic_log = self.surface.diagnostic_log(&settings);
        let stack = self.surface.dictation_stack(&settings)?;
        let target_pid = self.focus_target.lock().ok().and_then(|guard| *guard);

        let prepared = slugtale_lib::prepare_text_insertion(target_pid)?;
        let runtime = stack.asr_runtime();
        let insertion =
            slugtale_lib::DiagnosticTextInsertion::new(&prepared.insertion, diagnostic_log.clone());
        let rescue =
            slugtale_lib::DiagnosticInsertionRescue::new(prepared.rescue.as_ref(), diagnostic_log);
        slugtale_lib::DictationWorkflow::new(
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
    pub fn take_dictation_segment(&self, cut: u64) -> Option<slugtale_lib::CapturedAudio> {
        let flushed = self
            .capture
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use slugtale_lib::{
        AudioCaptureError, AudioRecorder, CapturedAudio, CountedSegment, DictationEvent,
        DictationRuntime, DictationRuntimeHost, DictationSegmentOutcome, DictationSegmentPosition,
        FileDiagnosticSink, SharedDiagnosticLog,
    };

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
    }

    impl FakeSurface {
        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl DictationSurface for FakeSurface {
        fn settings(&self) -> slugtale_lib::Settings {
            self.record(Call::ReadSettings);
            slugtale_lib::Settings::default()
        }

        fn record_diagnostic_event(&self, event: slugtale_lib::DiagnosticEvent) {
            let tag = match event {
                slugtale_lib::DiagnosticEvent::HotkeyTransition { .. } => "hotkey_transition",
                slugtale_lib::DiagnosticEvent::AudioCaptureFailed { .. } => "audio_capture_failed",
                _ => "other",
            };
            self.record(Call::Diagnostic(tag));
        }

        fn show_dictation_bar(&self, phase: DictationPhase, _settings: &slugtale_lib::Settings) {
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

        fn play_dictation_sound(&self, sound: slugtale_lib::DictationSound) {
            let name = match sound {
                slugtale_lib::DictationSound::Start => "start",
                slugtale_lib::DictationSound::Stop => "stop",
            };
            self.record(Call::PlaySound(name));
        }

        fn diagnostic_log(
            &self,
            _settings: &slugtale_lib::Settings,
        ) -> SharedDiagnosticLog<FileDiagnosticSink> {
            SharedDiagnosticLog::new(false, FileDiagnosticSink::unavailable())
        }

        fn dictation_stack(
            &self,
            _settings: &slugtale_lib::Settings,
        ) -> Result<slugtale_lib::DictationStack<FileDiagnosticSink>, String> {
            unreachable!("the tested events never reach the segment worker path")
        }
    }

    /// A recorder that never touches a device. `fail_start` simulates a
    /// microphone that cannot open; `silent_stop` produces the digital silence
    /// of a denied macOS microphone.
    struct FakeRecorder {
        fail_start: bool,
        silent_stop: bool,
    }

    impl FakeRecorder {
        fn healthy() -> Self {
            Self {
                fail_start: false,
                silent_stop: false,
            }
        }
    }

    impl Default for FakeRecorder {
        fn default() -> Self {
            Self::healthy()
        }
    }

    impl AudioRecorder for FakeRecorder {
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

        fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(self.captured())
        }

        fn take_segment_through(&mut self, _cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(self.captured())
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

    /// A runtime host that answers nothing: the worker stays idle because no
    /// test drives Pause Flushes, and a stray settle touches no surface.
    struct IdleRuntimeHost;

    impl DictationRuntimeHost for IdleRuntimeHost {
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
                IdleRuntimeHost,
                || 0,
                Arc::new(|_: slugtale_lib::LocalDate, _: CountedSegment| {}),
            )
            .expect("test runtime starts"),
        )
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
                fail_start: false,
                silent_stop: true,
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
}
