use std::sync::Arc;

use tauri::{Emitter, Manager};

use slugtale_lib::{
    BoxedDiagnosticSink, DictationEffects, DictationEvent, DictationHost, DictationParts,
    DictationPhase, WindowLabel,
};

use crate::dictation_bar_window::{hide_dictation_bar, show_dictation_bar};
use crate::hotkey_registration::{request_escape_registration, HotkeyRegistrationState};

use super::settings::{build_activation_snapshot_for, report_not_ready};
use super::{app_files, dictation_input_is_inert, load_current_settings, record_diagnostic_event};

pub(crate) fn route_dictation_ui_event(event: &str) -> Result<DictationEvent, String> {
    slugtale_lib::parse_dictation_ui_event(event)
}

pub(crate) fn resolve_end_event(
    control_event: Option<DictationEvent>,
    fallback: DictationEvent,
) -> DictationEvent {
    control_event.unwrap_or(fallback)
}

#[tauri::command]
pub(crate) fn dictation_event(app: tauri::AppHandle, event: String) -> Result<(), String> {
    match event.as_str() {
        "start" => dictation_host(&app).handle_dictation_event(DictationEvent::Start),
        "stop" => stop_active_dictation(&app),
        "cancel" => cancel_active_dictation(&app),
        other => Err(slugtale_lib::parse_dictation_ui_event(other).unwrap_err()),
    }
}

fn stop_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.stop(),
        slugtale_lib::DictationEvent::Stop,
    )
}

fn cancel_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.cancel(),
        slugtale_lib::DictationEvent::Cancel,
    )
}

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
            let _ = request_escape_registration(&registration, false);
        }
        event
    };

    dictation_host(app).handle_dictation_event(resolve_end_event(event, fallback))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn begin_dictation(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
    arm_escape: &mut dyn FnMut(bool) -> Result<(), String>,
) -> Result<(), String> {
    slugtale_lib::begin_activation(&mut TauriBeginHost {
        app,
        input,
        arm_escape,
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct TauriBeginHost<'a> {
    app: &'a tauri::AppHandle,
    input: slugtale_lib::DictationInput,
    arm_escape: &'a mut dyn FnMut(bool) -> Result<(), String>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl slugtale_lib::BeginHost for TauriBeginHost<'_> {
    fn typing_challenge_is_open(&self) -> bool {
        dictation_input_is_inert(self.app)
    }

    fn readiness_snapshot(&mut self) -> slugtale_lib::DictationActivation {
        let activation = build_activation_snapshot_for(self.app, self.input);
        if !activation.dictation_available() {
            report_not_ready(self.app, &activation.report);
        }
        activation
    }

    fn with_control<R>(
        &mut self,
        step: &mut dyn FnMut(&mut slugtale_lib::DictationControl) -> R,
    ) -> Option<R> {
        let state = self.app.state::<HotkeyRegistrationState>();
        let mut registration = state.0.lock().ok()?;
        Some(step(&mut registration.control))
    }

    fn set_escape(&mut self, armed: bool) -> Result<(), String> {
        (self.arm_escape)(armed)
    }

    fn start(
        &mut self,
        event: slugtale_lib::DictationEvent,
        activation: slugtale_lib::DictationActivation,
    ) -> Result<(), String> {
        dictation_host(self.app).handle_dictation_event_with(event, Some(activation))
    }
}

pub(crate) struct TauriSurface {
    pub(crate) app: tauri::AppHandle,
}

impl DictationEffects for TauriSurface {
    fn settings(&self) -> slugtale_lib::Settings {
        load_current_settings(&self.app)
    }

    fn record_diagnostic_event(&self, event: slugtale_lib::DiagnosticEvent) {
        record_diagnostic_event(&self.app, event);
    }

    fn show_dictation_bar(&self, phase: DictationPhase, settings: &slugtale_lib::Settings) {
        show_dictation_bar(&self.app, phase, settings);
    }

    fn hide_dictation_bar(&self) {
        hide_dictation_bar(&self.app);
    }

    fn emit_dictation_audio_level(&self, level: f32) {
        if let Some(window) = WindowLabel::DictationBar.window(&self.app) {
            let _ = window.emit("dictation-audio-level", level.clamp(0.0, 1.0));
        }
    }

    fn notify_capture_failure(&self, error: &str) {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        let _ = slugtale_lib::notify("Slugtale could not capture audio", error);
    }

    fn play_dictation_sound(&self, sound: slugtale_lib::DictationSound) {
        let _ = slugtale_lib::play_dictation_sound(sound);
    }

    fn record_stage_timing(&self, timing: &slugtale_lib::StageTiming) {
        super::record_stage_timing(&self.app, timing);
    }
}

impl DictationParts for TauriSurface {
    fn diagnostic_log(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> slugtale_lib::ErasedDiagnosticLog {
        app_files(&self.app)
            .diagnostic_log(settings.diagnostic_logging)
            .erased()
    }

    fn dictation_stack(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Result<slugtale_lib::DictationStack<BoxedDiagnosticSink>, String> {
        let diagnostic_log = self.diagnostic_log(settings);
        let timing_log = app_files(&self.app).transcription_timing_log();
        self.app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .dictation_stack(settings, diagnostic_log, timing_log)
            .map_err(|error| error.to_string())
    }

    fn prepared_insertion(
        &self,
        target_pid: Option<i32>,
    ) -> Result<slugtale_lib::PreparedInsertion, String> {
        slugtale_lib::prepare_text_insertion(target_pid)
    }
}

pub(crate) fn dictation_host(app: &tauri::AppHandle) -> Arc<DictationHost> {
    app.state::<Arc<DictationHost>>().inner().clone()
}

/// The bar window is permanently sized for the expanded pill because a Tauri
/// window cannot grow on hover, so while collapsed most of it is transparent —
/// and a transparent window still swallows clicks. The frontend polls this while
/// the bar is visible: it cannot detect the pointer itself, because a window
/// ignoring cursor events receives no mouse events to detect it with.
#[tauri::command]
pub(crate) fn dictation_bar_pointer_over(
    app: tauri::AppHandle,
    expanded: bool,
) -> Result<bool, String> {
    let Some(window) = WindowLabel::DictationBar.window(&app) else {
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

#[derive(serde::Serialize)]
pub(crate) struct DictationBarDisplayOption {
    value: slugtale_lib::BarDisplay,
    label: String,
}

/// Return the displays that can host the Dictation Bar right now. Displays with
/// no stable name cannot be selected safely across app launches, but the main
/// display is always available as the fallback choice.
#[tauri::command]
pub(crate) fn dictation_bar_displays(app: tauri::AppHandle) -> Vec<DictationBarDisplayOption> {
    let primary = app.primary_monitor().ok().flatten();
    let primary_label = primary
        .as_ref()
        .and_then(|monitor| monitor.name())
        .map(|name| slugtale_lib::primary_display_label(Some(name)))
        .unwrap_or_else(|| slugtale_lib::primary_display_label(None));
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
            label: slugtale_lib::secondary_display_label(&name, size.width, size.height),
        });
    }

    displays
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct TestSurface {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
        lines: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl TestSurface {
        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.to_string());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl DictationEffects for TestSurface {
        fn settings(&self) -> slugtale_lib::Settings {
            slugtale_lib::Settings {
                segment_pause_secs: 2,
                ..slugtale_lib::Settings::default()
            }
        }

        fn record_diagnostic_event(&self, event: slugtale_lib::DiagnosticEvent) {
            let tag = match event {
                slugtale_lib::DiagnosticEvent::HotkeyTransition { .. } => "hotkey_transition",
                slugtale_lib::DiagnosticEvent::AudioCaptureFailed { .. } => "audio_capture_failed",
                slugtale_lib::DiagnosticEvent::InsertionFailed { .. } => "insertion_failed",
                slugtale_lib::DiagnosticEvent::InsertionRescued => "insertion_rescued",
                _ => "other",
            };
            self.record(tag);
        }

        fn show_dictation_bar(&self, phase: DictationPhase, _settings: &slugtale_lib::Settings) {
            self.record(&format!("show:{}", phase.as_str()));
        }

        fn hide_dictation_bar(&self) {
            self.record("hide");
        }

        fn emit_dictation_audio_level(&self, level: f32) {
            if level == 0.0 {
                self.record("clear_level");
            }
        }

        fn notify_capture_failure(&self, _error: &str) {
            self.record("notify_failure");
        }

        fn play_dictation_sound(&self, sound: slugtale_lib::DictationSound) {
            let name = match sound {
                slugtale_lib::DictationSound::Start => "start",
                slugtale_lib::DictationSound::Stop => "stop",
            };
            self.record(&format!("sound:{name}"));
        }

        fn record_stage_timing(&self, timing: &slugtale_lib::StageTiming) {
            self.record(&format!("stage:{}", timing.event));
        }
    }

    impl DictationParts for TestSurface {
        fn diagnostic_log(
            &self,
            settings: &slugtale_lib::Settings,
        ) -> slugtale_lib::ErasedDiagnosticLog {
            let lines = Arc::clone(&self.lines);
            slugtale_lib::ErasedDiagnosticLog::new(
                settings.diagnostic_logging,
                BoxedDiagnosticSink::new(move |line: &str| {
                    lines.lock().unwrap().push(line.to_string());
                }),
            )
        }

        fn dictation_stack(
            &self,
            _settings: &slugtale_lib::Settings,
        ) -> Result<slugtale_lib::DictationStack<BoxedDiagnosticSink>, String> {
            Err("no transcription engine in adapter test".to_string())
        }

        fn prepared_insertion(
            &self,
            _target_pid: Option<i32>,
        ) -> Result<slugtale_lib::PreparedInsertion, String> {
            Err("no text insertion in adapter test".to_string())
        }
    }

    #[derive(Clone, Default)]
    struct TestMicrophone {
        level: Arc<std::sync::Mutex<Option<slugtale_lib::AudioLevelCallback>>>,
        watermark: Arc<std::sync::atomic::AtomicU64>,
        cuts: Arc<std::sync::Mutex<Vec<u64>>>,
    }

    impl TestMicrophone {
        fn voice(&self, level: f32) {
            if slugtale_lib::is_voice_level(level) {
                self.watermark
                    .fetch_add(8_000, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(callback) = self.level.lock().unwrap().clone() {
                callback(level);
            }
        }
    }

    struct TestRecorder {
        microphone: TestMicrophone,
    }

    impl TestRecorder {
        fn captured() -> slugtale_lib::CapturedAudio {
            slugtale_lib::CapturedAudio {
                sample_rate_hz: 16_000,
                samples: vec![0.4; 480],
            }
        }
    }

    impl slugtale_lib::DictationRecorder for TestRecorder {
        fn prepare(&mut self) -> Result<(), slugtale_lib::AudioCaptureError> {
            Ok(())
        }

        fn start(&mut self) -> Result<(), slugtale_lib::AudioCaptureError> {
            Ok(())
        }

        fn stop(&mut self) -> Result<slugtale_lib::CapturedAudio, slugtale_lib::AudioCaptureError> {
            Ok(Self::captured())
        }

        fn cancel(&mut self) -> Result<(), slugtale_lib::AudioCaptureError> {
            Ok(())
        }

        fn cut_segment(
            &mut self,
            cut: u64,
        ) -> Result<slugtale_lib::CapturedAudio, slugtale_lib::AudioCaptureError> {
            self.microphone.cuts.lock().unwrap().push(cut);
            Ok(Self::captured())
        }

        fn voice_watermark_cell(&self) -> Arc<std::sync::atomic::AtomicU64> {
            Arc::clone(&self.microphone.watermark)
        }

        fn set_level_callback(&mut self, callback: Option<slugtale_lib::AudioLevelCallback>) {
            *self.microphone.level.lock().unwrap() = callback;
        }
    }

    fn test_host() -> (
        Arc<DictationHost<TestRecorder>>,
        Arc<TestSurface>,
        TestMicrophone,
    ) {
        let surface = Arc::new(TestSurface::default());
        let microphone = TestMicrophone::default();
        let recorder = TestRecorder {
            microphone: microphone.clone(),
        };
        let effects: Arc<dyn DictationEffects> = surface.clone();
        let parts: Arc<dyn DictationParts> = surface.clone();
        let usage = slugtale_lib::UsageQueue::start(Arc::new(
            |_: slugtale_lib::LocalDate, _: slugtale_lib::CountedSegment| {},
        ))
        .expect("usage writer starts");
        let host = Arc::new(DictationHost::with_recorder_and_focus_target_source(
            effects,
            parts,
            recorder,
            usage,
            Arc::new(|| Some(4_242)),
        ));
        let runtime_host: Arc<dyn slugtale_lib::DictationRuntimeHost> = host.clone();
        let watermark_host = Arc::clone(&host);
        let runtime = slugtale_lib::DictationRuntime::start(runtime_host, move || {
            watermark_host.voice_watermark()
        })
        .expect("runtime starts");
        host.set_runtime(Arc::new(runtime)).unwrap();
        (host, surface, microphone)
    }

    fn start_through_adapter(host: &DictationHost<TestRecorder>, ui: &str) {
        let event = route_dictation_ui_event(ui).unwrap();
        assert_eq!(event, DictationEvent::Start);
        host.handle_dictation_event(event).unwrap();
    }

    fn wait_for_hide(surface: &TestSurface) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if surface.calls().iter().any(|call| call == "hide") {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dictation never hid the bar: {:?}",
                surface.calls()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn ui_names_route_to_exactly_one_host_event_and_unknown_is_rejected() {
        assert_eq!(route_dictation_ui_event("start"), Ok(DictationEvent::Start));
        assert_eq!(route_dictation_ui_event("stop"), Ok(DictationEvent::Stop));
        assert_eq!(
            route_dictation_ui_event("cancel"),
            Ok(DictationEvent::Cancel)
        );
        let error = route_dictation_ui_event("pause").unwrap_err();
        assert_eq!(error, "unknown dictation event: pause");
    }

    #[test]
    fn a_control_event_wins_over_the_fallback_and_absence_uses_it() {
        assert_eq!(
            resolve_end_event(Some(DictationEvent::Stop), DictationEvent::Cancel),
            DictationEvent::Stop
        );
        assert_eq!(
            resolve_end_event(None, DictationEvent::Cancel),
            DictationEvent::Cancel
        );
        assert_eq!(
            resolve_end_event(None, DictationEvent::Stop),
            DictationEvent::Stop
        );
    }

    #[test]
    fn start_through_the_adapter_opens_the_microphone_and_shows_recording() {
        let (host, surface, _microphone) = test_host();

        start_through_adapter(&host, "start");

        let calls = surface.calls();
        assert_eq!(calls[0], "hotkey_transition");
        let cue = calls
            .iter()
            .position(|call| call == "sound:start")
            .expect("start plays its cue");
        assert_eq!(calls[cue + 1], "show:recording");
        assert!(!calls.contains(&"show:transcribing".to_string()));
        assert!(!calls.contains(&"hide".to_string()));
    }

    #[test]
    fn stop_through_the_adapter_shows_transcribing_and_settles_hidden() {
        let (host, surface, _microphone) = test_host();
        start_through_adapter(&host, "start");
        surface.calls.lock().unwrap().clear();

        let stop = route_dictation_ui_event("stop").unwrap();
        assert_eq!(stop, DictationEvent::Stop);
        host.handle_dictation_event(stop).unwrap();

        assert!(surface
            .calls()
            .iter()
            .any(|call| call == "show:transcribing"));
        wait_for_hide(&surface);
    }

    #[test]
    fn cancel_through_the_adapter_discards_and_leaves_no_queued_text() {
        let (host, surface, _microphone) = test_host();
        start_through_adapter(&host, "start");

        let cancel = route_dictation_ui_event("cancel").unwrap();
        assert_eq!(cancel, DictationEvent::Cancel);
        host.handle_dictation_event(cancel).unwrap();

        let calls = surface.calls();
        assert!(calls.contains(&"hide".to_string()));
        assert!(!calls.contains(&"show:transcribing".to_string()));
        let stale = host.run_dictation_segment(
            999_999,
            slugtale_lib::CapturedAudio::mono_16khz(vec![0.4; 480]),
            slugtale_lib::DictationSegmentPosition::First,
        );
        assert!(stale.is_err());
    }

    #[test]
    fn a_segment_pause_flushes_through_host_while_recording_carries_on() {
        let (host, surface, microphone) = test_host();
        start_through_adapter(&host, "start");

        microphone.voice(slugtale_lib::VOICE_LEVEL + 0.2);
        std::thread::sleep(std::time::Duration::from_millis(2_300));
        microphone.voice(0.0);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !microphone.cuts.lock().unwrap().is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pause flush never cut the microphone: {:?}",
                surface.calls()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(microphone.cuts.lock().unwrap().clone(), vec![8_000]);
    }

    #[test]
    fn a_stale_segment_for_an_unknown_session_takes_nothing() {
        let (host, _surface, _microphone) = test_host();
        start_through_adapter(&host, "start");

        let stale = host.run_dictation_segment(
            42_000,
            slugtale_lib::CapturedAudio::mono_16khz(vec![0.4; 480]),
            slugtale_lib::DictationSegmentPosition::First,
        );
        let error = stale.unwrap_err();
        assert!(
            error.contains("no longer running"),
            "stale work must be refused: {error}"
        );
    }

    struct FakeRuntime {
        text: String,
    }

    impl slugtale_lib::AsrRuntime for FakeRuntime {
        fn transcribe(
            &self,
            _audio: slugtale_lib::CapturedAudio,
        ) -> Result<slugtale_lib::FinalTranscription, slugtale_lib::AsrError> {
            Ok(slugtale_lib::FinalTranscription::plain(self.text.clone()))
        }
    }

    struct FakeInsertion {
        inserted: Arc<std::sync::Mutex<Vec<String>>>,
        fails: bool,
    }

    impl slugtale_lib::TextInsertion for FakeInsertion {
        fn insert(
            &self,
            transcription: &slugtale_lib::FinalTranscription,
        ) -> Result<(), slugtale_lib::TextInsertionError> {
            self.inserted
                .lock()
                .unwrap()
                .push(transcription.text.clone());
            if self.fails {
                return Err(slugtale_lib::TextInsertionError::new(
                    "fake insertion failure",
                ));
            }
            Ok(())
        }
    }

    struct FakeRescue {
        rescued: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl slugtale_lib::InsertionRescue for FakeRescue {
        fn rescue(
            &self,
            transcription: &slugtale_lib::FinalTranscription,
        ) -> Result<(), slugtale_lib::InsertionRescueError> {
            self.rescued
                .lock()
                .unwrap()
                .push(transcription.text.clone());
            Ok(())
        }
    }

    #[test]
    fn a_failed_insertion_is_preserved_by_rescue_and_a_clean_insert_is_not() {
        let audio = slugtale_lib::CapturedAudio::mono_16khz(vec![0.4; 480]);

        let rescued = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inserted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runtime = FakeRuntime {
            text: "rescued words".to_string(),
        };
        let insertion = FakeInsertion {
            inserted: Arc::clone(&inserted),
            fails: true,
        };
        let rescue = FakeRescue {
            rescued: Arc::clone(&rescued),
        };
        let outcome = slugtale_lib::DictationWorkflow::new(
            &runtime,
            &insertion,
            &rescue,
            slugtale_lib::TranscriptCleanupMode::Basic,
        )
        .complete(audio.clone(), slugtale_lib::DictationSegmentPosition::First)
        .expect("rescue preserves the transcription");
        assert!(outcome.rescued);
        assert!(outcome.inserted);
        assert_eq!(outcome.insertion_failure.is_some(), true);
        assert_eq!(*rescued.lock().unwrap(), vec!["Rescued words"]);

        let rescued = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inserted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let insertion = FakeInsertion {
            inserted: Arc::clone(&inserted),
            fails: false,
        };
        let rescue = FakeRescue {
            rescued: Arc::clone(&rescued),
        };
        let outcome = slugtale_lib::DictationWorkflow::new(
            &runtime,
            &insertion,
            &rescue,
            slugtale_lib::TranscriptCleanupMode::Basic,
        )
        .complete(audio, slugtale_lib::DictationSegmentPosition::First)
        .expect("clean insertion succeeds");
        assert!(outcome.inserted);
        assert!(!outcome.rescued);
        assert_eq!(*inserted.lock().unwrap(), vec!["Rescued words"]);
        assert!(rescued.lock().unwrap().is_empty());
    }
}
