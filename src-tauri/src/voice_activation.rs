//! macOS Voice Activation adapter.
//!
//! This module owns the always-listening microphone and its worker. The pure
//! phrase, speech-gate, and window rules stay in `slugtale_lib::wake_word`,
//! where every platform can test them without audio hardware.

use super::*;

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
#[derive(Default)]
pub(super) struct VoiceActivationState(
    std::sync::Mutex<Option<std::sync::mpsc::Sender<VoiceActivationCommand>>>,
);

#[cfg(not(all(target_os = "macos", feature = "voice-activation")))]
#[derive(Default)]
pub(super) struct VoiceActivationState(());

pub(super) const fn supported() -> bool {
    cfg!(all(target_os = "macos", feature = "voice-activation"))
}

/// Validate, apply, and persist the Voice Activation choice as one operation.
/// The Tauri command delegates here so worker state and the saved checkbox
/// cannot drift apart.
pub(super) fn save_settings(
    app: &tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    if enabled && !supported() {
        return Err("Voice activation is not available in this version of Slugtale.".to_string());
    }

    let previous = load_current_settings(app);
    slugtale_lib::apply_and_persist(
        &previous,
        |settings| slugtale_lib::apply_voice_activation_settings(settings, enabled),
        // Validation rides the side-effect step: the worker must not start
        // without an engine that can run the wake checks.
        |settings| {
            if enabled
                && app
                    .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                    .whisper_provider(settings)
                    .is_none()
            {
                return Err("Voice activation needs the local Whisper model.".to_string());
            }
            sync_worker(app, settings.voice_activation_enabled)
        },
        |settings| save_current_settings(app, settings),
    )
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VoiceActivationCommand {
    Listen,
    Stop,
}

/// Bring the running listener in line with the stored preference.
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
pub(super) fn sync_worker(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let state = app.state::<VoiceActivationState>();
    let mut listener = state
        .0
        .lock()
        .map_err(|_| "voice activation mutex poisoned".to_string())?;

    if let Some(sender) = listener.as_ref() {
        let command = if enabled {
            VoiceActivationCommand::Listen
        } else {
            VoiceActivationCommand::Stop
        };
        if sender.send(command).is_ok() {
            return Ok(());
        }
        *listener = None;
    }

    if !enabled {
        return Ok(());
    }

    let (sender, receiver) = listening_channel()?;
    let app_handle = app.clone();
    std::thread::Builder::new()
        .name("slugtale-voice-activation".to_string())
        .spawn(move || run_worker(app_handle, receiver))
        .map_err(|error| error.to_string())?;
    *listener = Some(sender);
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "voice-activation")))]
pub(super) fn sync_worker(_app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        Err("Voice activation is not available in this version of Slugtale.".to_string())
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn listening_channel() -> Result<
    (
        std::sync::mpsc::Sender<VoiceActivationCommand>,
        std::sync::mpsc::Receiver<VoiceActivationCommand>,
    ),
    String,
> {
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(VoiceActivationCommand::Listen)
        .map_err(|_| "voice activation worker stopped before listening".to_string())?;
    Ok((sender, receiver))
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn whisper_ready(app: &tauri::AppHandle) -> bool {
    let settings = load_current_settings(app);
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .whisper_provider(&settings)
        .is_some()
}

/// Wait up to `timeout` for a command so turning Voice Activation off closes
/// the microphone without sitting out a poll or retry sleep.
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenWait {
    Continue,
    Stop,
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn wait_or_stop(
    receiver: &std::sync::mpsc::Receiver<VoiceActivationCommand>,
    timeout: std::time::Duration,
) -> ListenWait {
    match receiver.recv_timeout(timeout) {
        Ok(VoiceActivationCommand::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            ListenWait::Stop
        }
        Ok(VoiceActivationCommand::Listen) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            ListenWait::Continue
        }
    }
}

/// The macOS half of the Voice Activation adapter: it owns the app handle and
/// answers the listen loop's questions. Every decision lives in
/// `slugtale_lib::run_listen_loop`; nothing here but app reads and effects.
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
struct AppWakeListener {
    app: tauri::AppHandle,
    receiver: std::sync::mpsc::Receiver<VoiceActivationCommand>,
    capture: slugtale_lib::VoiceActivationCapture<slugtale_lib::CpalAudioRecorder>,
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
impl AppWakeListener {
    fn new(
        app: tauri::AppHandle,
        receiver: std::sync::mpsc::Receiver<VoiceActivationCommand>,
    ) -> Self {
        Self {
            app,
            receiver,
            capture: slugtale_lib::VoiceActivationCapture::new(
                slugtale_lib::CpalAudioRecorder::new(),
            ),
        }
    }
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
impl slugtale_lib::WakeListener for AppWakeListener {
    fn next_command(&mut self) -> Option<slugtale_lib::ListenerCommand> {
        self.receiver.recv().ok().map(|command| match command {
            VoiceActivationCommand::Listen => slugtale_lib::ListenerCommand::Listen,
            VoiceActivationCommand::Stop => slugtale_lib::ListenerCommand::Stop,
        })
    }

    fn stop_requested(&self) -> bool {
        matches!(
            self.receiver.try_recv(),
            Ok(VoiceActivationCommand::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected)
        )
    }

    fn wait(&mut self, timeout: std::time::Duration) -> bool {
        wait_or_stop(&self.receiver, timeout) == ListenWait::Continue
    }

    fn dictating(&self) -> bool {
        target_is_dictating(&self.app)
    }

    fn engine_ready(&self) -> bool {
        whisper_ready(&self.app)
    }

    fn microphone_granted(&self) -> bool {
        slugtale_lib::PlatformReadiness::microphone_granted(&CurrentPlatform::new())
    }

    fn capture_is_open(&self) -> bool {
        self.capture.is_open()
    }

    fn start_capture(&mut self) -> Result<(), String> {
        self.capture.start()
    }

    fn rebuild_capture(&mut self) {
        self.capture.rebuild(slugtale_lib::CpalAudioRecorder::new());
    }

    fn close_capture(&mut self) {
        self.capture.close();
    }

    fn take_segment(&mut self) -> Result<Vec<f32>, String> {
        self.capture
            .take_segment()
            .map(|chunk| chunk.samples)
            .map_err(|error| error.to_string())
    }

    fn wake_check(&mut self, samples: Vec<f32>) -> slugtale_lib::WakeCheck {
        let audio = slugtale_lib::CapturedAudio::mono_16khz(samples);
        // Wake checks always use greedy decoding. The user's wider beam is
        // useful for dictation text, but wasteful for a two-word phrase.
        let mut settings = load_current_settings(&self.app);
        settings.speed_profile = slugtale_lib::SpeedProfile::Fast;
        let Some(provider) = self
            .app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .whisper_provider(&settings)
        else {
            return slugtale_lib::WakeCheck::EngineUnavailable;
        };
        match provider.transcribe(&audio) {
            Ok(transcription) => slugtale_lib::WakeCheck::Transcript(
                transcription.transcription.text.trim().to_string(),
            ),
            Err(error) => slugtale_lib::WakeCheck::TranscriptionFailed(error.to_string()),
        }
    }

    fn report_microphone_problem(&mut self) {
        report_voice_activation_microphone_problem(&self.app);
    }

    fn trigger_wake(&mut self) {
        trigger_start(&self.app);
    }
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn run_worker(app: tauri::AppHandle, receiver: std::sync::mpsc::Receiver<VoiceActivationCommand>) {
    slugtale_lib::run_listen_loop(&mut AppWakeListener::new(app, receiver));
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn report_voice_activation_microphone_problem(app: &tauri::AppHandle) {
    let activation =
        build_activation_snapshot_for(app, slugtale_lib::DictationInput::VoiceActivation);
    if !activation.dictation_available() {
        report_not_ready(app, &activation.report);
        return;
    }

    let _ = slugtale_lib::notify(
        "Slugtale cannot hear the microphone",
        "Check the selected microphone and its input level in System Settings.",
    );
    slugtale_lib::show_settings(app.clone());
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn target_is_dictating(app: &tauri::AppHandle) -> bool {
    app.state::<HotkeyRegistrationState>()
        .0
        .lock()
        .map(|registration| registration.control.is_dictating())
        .unwrap_or(false)
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn trigger_start(app: &tauri::AppHandle) {
    // Escape arming rides the global-key worker's queue rather than happening
    // synchronously here: registration must not run inside this worker thread,
    // which holds no shortcut-plugin locks but also cannot block dictation.
    // Failures are reported, not swallowed — a begin whose Escape cannot be
    // armed must roll back like any other failed activation step.
    let mut set_escape = |should_register: bool| {
        let state = app.state::<HotkeyRegistrationState>();
        let registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        request_escape_registration(&registration, should_register)
    };

    if let Err(error) = begin_dictation(
        app,
        slugtale_lib::DictationInput::VoiceActivation,
        &mut set_escape,
    ) {
        eprintln!("voice activation could not start dictation: {error}");
    }
}

#[cfg(all(target_os = "macos", feature = "voice-activation", test))]
mod tests {
    use super::*;

    #[test]
    fn a_new_worker_channel_starts_with_listen() {
        let (_sender, receiver) = listening_channel().unwrap();
        assert_eq!(receiver.recv().unwrap(), VoiceActivationCommand::Listen);
    }

    #[test]
    fn wait_or_stop_wakes_immediately_on_stop() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(VoiceActivationCommand::Stop).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            wait_or_stop(&receiver, std::time::Duration::from_secs(2)),
            ListenWait::Stop
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }

    #[test]
    fn wait_or_stop_continues_after_a_timeout() {
        let (_sender, receiver) = std::sync::mpsc::channel();
        assert_eq!(
            wait_or_stop(&receiver, std::time::Duration::from_millis(10)),
            ListenWait::Continue
        );
    }

    #[test]
    fn stop_requested_is_false_when_the_channel_is_empty() {
        let (_sender, receiver) = std::sync::mpsc::channel();
        assert!(!stop_requested(&receiver));
    }

    #[test]
    fn stop_requested_is_true_after_stop() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(VoiceActivationCommand::Stop).unwrap();
        assert!(stop_requested(&receiver));
    }
}
