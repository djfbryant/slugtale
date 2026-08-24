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
    let mut settings = previous.clone();
    slugtale_lib::apply_voice_activation_settings(&mut settings, enabled);
    if enabled
        && app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .whisper_provider(&settings)
            .is_none()
    {
        return Err("Voice activation needs the local Whisper model.".to_string());
    }

    sync_worker(app, enabled)?;
    if let Err(error) = save_current_settings(app, &settings) {
        let _ = sync_worker(app, previous.voice_activation_enabled);
        return Err(error);
    }
    Ok(settings)
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
const POLL: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const CAPTURE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const MIN_NEW_SAMPLES: usize = 32_000;
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const OVERLAP_SAMPLES: usize = 16_000;
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const SPEECH_FRAME_SAMPLES: usize = 320;
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const MINIMUM_SPEECH_RMS: f32 = 0.006;
#[cfg(all(target_os = "macos", feature = "voice-activation"))]
const SPEECH_CONTRAST_RATIO: f32 = 1.5;

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn run_worker(app: tauri::AppHandle, receiver: std::sync::mpsc::Receiver<VoiceActivationCommand>) {
    use slugtale_lib::{
        CpalAudioRecorder, NewAudioState, SpeechWindowBuffer, VoiceActivationCapture,
        WakeWordConfig, WakeWordDetector,
    };

    while let Ok(command) = receiver.recv() {
        if command == VoiceActivationCommand::Stop {
            continue;
        }

        let mut capture = VoiceActivationCapture::new(CpalAudioRecorder::new());
        let mut capture_error_reported = false;
        let mut microphone_problem_reported = false;
        let mut window = SpeechWindowBuffer::new();
        let mut detector = WakeWordDetector::new(WakeWordConfig::default());

        loop {
            use std::sync::mpsc::TryRecvError;
            match receiver.try_recv() {
                Ok(VoiceActivationCommand::Stop) | Err(TryRecvError::Disconnected) => break,
                Ok(VoiceActivationCommand::Listen) | Err(TryRecvError::Empty) => {}
            }

            if target_is_dictating(&app) {
                if capture.is_open() {
                    capture.rebuild(CpalAudioRecorder::new());
                }
                window.clear();
                std::thread::sleep(POLL);
                continue;
            }

            if !capture.is_open() {
                if !slugtale_lib::PlatformReadiness::microphone_granted(&CurrentPlatform::new()) {
                    if !microphone_problem_reported {
                        report_voice_activation_microphone_problem(&app);
                        microphone_problem_reported = true;
                    }
                    std::thread::sleep(CAPTURE_RETRY);
                    continue;
                }
                if let Err(error) = capture.start() {
                    if !capture_error_reported {
                        eprintln!("voice activation could not open the microphone: {error}");
                        capture_error_reported = true;
                    }
                    capture.rebuild(CpalAudioRecorder::new());
                    std::thread::sleep(CAPTURE_RETRY);
                    continue;
                }
                capture_error_reported = false;
                window.clear();
                eprintln!("voice activation: listening");
            }

            std::thread::sleep(POLL);
            let chunk = match capture.take_segment() {
                Ok(chunk) => chunk,
                Err(error) => {
                    eprintln!("voice activation capture failed: {error}");
                    capture.rebuild(CpalAudioRecorder::new());
                    capture_error_reported = true;
                    window.clear();
                    std::thread::sleep(CAPTURE_RETRY);
                    continue;
                }
            };
            window.push(&chunk.samples);

            if !window.ready_for_evaluation(MIN_NEW_SAMPLES) {
                continue;
            }
            let audio_state = window.new_audio_state(
                SPEECH_FRAME_SAMPLES,
                MINIMUM_SPEECH_RMS,
                SPEECH_CONTRAST_RATIO,
            );
            match audio_state {
                NewAudioState::DigitalSilence => {
                    if !microphone_problem_reported {
                        eprintln!("voice activation: microphone supplied digital silence");
                        report_voice_activation_microphone_problem(&app);
                        microphone_problem_reported = true;
                    }
                    capture.rebuild(CpalAudioRecorder::new());
                    window.clear();
                    std::thread::sleep(CAPTURE_RETRY);
                    continue;
                }
                NewAudioState::Quiet => {
                    microphone_problem_reported = false;
                    window.retain_recent(OVERLAP_SAMPLES);
                    continue;
                }
                NewAudioState::Speech => microphone_problem_reported = false,
            }

            let audio = slugtale_lib::CapturedAudio::mono_16khz(window.take_for_evaluation());
            window.retain_recent(OVERLAP_SAMPLES);

            // Wake checks always use greedy decoding. The user's wider beam is
            // useful for dictation text, but wasteful for a two-word phrase.
            let mut settings = load_current_settings(&app);
            settings.speed_profile = slugtale_lib::SpeedProfile::Fast;
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .whisper_provider(&settings);
            let Some(provider) = provider else {
                continue;
            };

            match provider.transcribe(&audio) {
                Ok(transcription) => {
                    let text = transcription.transcription.text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let score = slugtale_lib::wake_phrase_score(text);
                    // Scores are safe to log. Transcript text and audio are not.
                    eprintln!("voice activation: score {score:.2}");
                    if detector.on_transcript(text, now_unix_ms()).is_some() {
                        window.clear();
                        eprintln!("voice activation: wake phrase detected");
                        trigger_start(&app);
                    }
                }
                Err(error) => eprintln!("voice activation transcription failed: {error}"),
            }
        }

        capture.close();
        eprintln!("voice activation: stopped listening");
    }
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
        .ok()
        .and_then(|registration| {
            registration
                .lifecycle
                .as_ref()
                .map(slugtale_lib::DictationLifecycle::is_dictating)
        })
        .unwrap_or(false)
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn trigger_start(app: &tauri::AppHandle) {
    if typing_challenge_is_open(app) {
        return;
    }
    let activation =
        build_activation_snapshot_for(app, slugtale_lib::DictationInput::VoiceActivation);
    if !activation.dictation_available() {
        report_not_ready(app, &activation.report);
        return;
    }

    let event = {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = match state.0.lock() {
            Ok(registration) => registration,
            Err(_) => return,
        };
        registration
            .lifecycle
            .as_mut()
            .and_then(slugtale_lib::DictationLifecycle::start)
    };
    let Some(event) = event else {
        return;
    };

    if let Ok(registration) = app.state::<HotkeyRegistrationState>().0.lock() {
        request_escape_registration(&registration, true);
    }

    if let Err(error) = handle_dictation_event_with(app, event, Some(activation)) {
        if let Ok(mut registration) = app.state::<HotkeyRegistrationState>().0.lock() {
            if let Some(lifecycle) = registration.lifecycle.as_mut() {
                lifecycle.stop();
            }
            request_escape_registration(&registration, false);
        }
        eprintln!("voice activation could not start dictation: {error}");
    }
}

#[cfg(all(target_os = "macos", feature = "voice-activation"))]
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(all(target_os = "macos", feature = "voice-activation", test))]
mod tests {
    use super::*;

    #[test]
    fn a_new_worker_channel_starts_with_listen() {
        let (_sender, receiver) = listening_channel().unwrap();
        assert_eq!(receiver.recv().unwrap(), VoiceActivationCommand::Listen);
    }
}
