#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

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

#[derive(Clone)]
struct TauriDictationEventSink {
    app: tauri::AppHandle,
}

impl slugtale_lib::DictationEventSink for TauriDictationEventSink {
    fn emit(&mut self, event: slugtale_lib::DictationEvent) {
        if let Err(error) = handle_dictation_event(&self.app, event) {
            eprintln!("dictation event failed: {error}");
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistrationState(Mutex<HotkeyRegistration>);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Default)]
struct HotkeyRegistration {
    current_hotkey: Option<String>,
    adapter: Option<slugtale_lib::HotkeyDictationAdapter<TauriDictationEventSink>>,
}

#[tauri::command]
fn show_settings(app: tauri::AppHandle) {
    slugtale_lib::show_settings(app);
}

/// Drive the recording surface (ADR-0014) from a dictation lifecycle event:
/// play the start/stop sound and show or hide the Dictation Bar. The bar's Stop
/// and Cancel controls and its Escape key all route here; the hotkey lifecycle
/// (slugtale-h8z.3) will route `start` and `stop` here too once wired.
#[tauri::command]
fn dictation_event(app: tauri::AppHandle, event: String) -> Result<(), String> {
    let event = match event.as_str() {
        "start" => slugtale_lib::DictationEvent::Start,
        "stop" => slugtale_lib::DictationEvent::Stop,
        "cancel" => slugtale_lib::DictationEvent::Cancel,
        other => return Err(format!("unknown dictation event: {other}")),
    };

    handle_dictation_event(&app, event)
}

fn handle_dictation_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DictationEvent,
) -> Result<(), String> {
    match event {
        slugtale_lib::DictationEvent::Start => {
            // Capture the app the user is dictating into before our own bar can
            // take focus, so insertion can re-target it later (slugtale-squ).
            capture_focus_target(app);
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
        // Cancel clears the bar immediately and discards the audio.
        slugtale_lib::DictationEvent::Cancel => {
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
    #[cfg(target_os = "macos")]
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
        guard.on_event(event).map_err(|error| error.to_string())?
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
            // then hide it once insertion completes (slugtale-0t4).
            show_dictation_bar(app, DictationPhase::Transcribing);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                match complete_captured_dictation(app.clone(), audio).await {
                    Ok(transcription) => {
                        eprintln!(
                            "inserted cleaned final transcription: {} chars",
                            transcription.text.chars().count()
                        );
                    }
                    Err(error) => eprintln!("dictation workflow failed: {error}"),
                }
                hide_dictation_bar(&app);
            });
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
    Arc::new(move |level| emit_dictation_audio_level(&app, level))
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

async fn complete_captured_dictation(
    app: tauri::AppHandle,
    audio: slugtale_lib::CapturedAudio,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let model_path = settings
        .model
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(&app)?));
    let runtime = app.state::<WhisperRuntimeCache>().runtime_for(&model_path);
    let target_pid = app
        .state::<FocusTargetState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| *guard);

    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            // Bring the user's app back to the front so synthesized keystrokes land
            // in its focused field rather than wherever focus drifted (slugtale-squ).
            if let Some(pid) = target_pid {
                if slugtale_lib::activate_app(pid) {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                }
            }

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
            let workflow = slugtale_lib::DictationWorkflow::new(&*runtime, &insertion, &rescue);
            workflow.complete(audio).map_err(|error| error.to_string())
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (runtime, audio, target_pid);
            Err("text insertion is only implemented for macOS".to_string())
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn setup_configured_hotkey(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let settings = load_current_settings(app.handle());

    let mut builder =
        tauri_plugin_global_shortcut::Builder::new().with_handler(move |app, _shortcut, event| {
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
                Ok(mut registration) => {
                    if let Some(adapter) = registration.adapter.as_mut() {
                        adapter.on_hotkey(input);
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
    registration.adapter = settings.hotkey.as_ref().map(|_| {
        slugtale_lib::HotkeyDictationAdapter::new(
            settings.activation_mode,
            TauriDictationEventSink { app: app.clone() },
        )
    });
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

fn show_dictation_bar(app: &tauri::AppHandle, phase: DictationPhase) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        // Tell the frontend which state to render before showing, so the bar never
        // flashes a stale "recording" pill when it reappears for transcription.
        let _ = window.emit("dictation-phase", phase.as_str());
        position_bottom_center(&window);
        let _ = window.show();
        if slugtale_lib::dictation_bar_should_take_focus() {
            let _ = window.set_focus();
        }
    }
}

fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let _ = window.hide();
    }
}

/// Place the Dictation Bar near the bottom-center of the active display, above
/// the Dock, matching the resident dictation pills users know from FluidVoice
/// and Wispr Flow.
fn position_bottom_center(window: &tauri::WebviewWindow) {
    let monitor = match window.current_monitor() {
        Ok(Some(monitor)) => monitor,
        _ => match window.primary_monitor() {
            Ok(Some(monitor)) => monitor,
            _ => return,
        },
    };

    let Ok(size) = window.outer_size() else {
        return;
    };

    let screen = monitor.size();
    let origin = monitor.position();
    // ~96pt of breathing room above the bottom edge, scaled to the display.
    let margin = (96.0 * monitor.scale_factor()) as i32;
    let x = origin.x + (screen.width as i32 - size.width as i32) / 2;
    let y = origin.y + screen.height as i32 - size.height as i32 - margin;
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

#[tauri::command]
fn get_settings_readiness(app: tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    let settings = load_current_settings(&app);
    let platform = model_dir(&app)
        .map(|dir| CurrentPlatform::for_readiness(&dir))
        .unwrap_or_else(|_| CurrentPlatform::new(settings.model.as_deref().map(PathBuf::from)));
    slugtale_lib::settings_readiness_report(&settings, &platform)
}

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> slugtale_lib::Settings {
    load_current_settings(&app)
}

#[tauri::command]
fn open_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return slugtale_lib::run_microphone_permission_setup(
            &slugtale_lib::MacosMicrophonePermissionSetup,
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("microphone settings shortcut is only implemented for macOS".to_string())
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

    #[cfg(not(target_os = "macos"))]
    {
        Err("text insertion settings shortcut is only implemented for macOS".to_string())
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
fn get_local_model_status(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    Ok(slugtale_lib::local_model_status(&model_dir(&app)?))
}

#[tauri::command]
async fn download_local_model(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    let dir = model_dir(&app)?;
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
        slugtale_lib::ensure_default_model(&dir, &slugtale_lib::HttpModelDownloader, &mut forward)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    save_model_setting(&app, status.present.then(|| status.path.clone()))?;
    Ok(status)
}

#[tauri::command]
fn delete_local_model(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    let status =
        slugtale_lib::delete_default_model(&model_dir(&app)?).map_err(|error| error.to_string())?;
    save_model_setting(&app, None)?;
    Ok(status)
}

#[tauri::command]
fn reveal_model_location(app: tauri::AppHandle) -> Result<(), String> {
    let location = slugtale_lib::reveal_location(&model_dir(&app)?);
    slugtale_lib::open_in_file_manager(&location).map_err(|error| error.to_string())
}

#[tauri::command]
async fn transcribe_captured_audio(
    app: tauri::AppHandle,
    cache: tauri::State<'_, WhisperRuntimeCache>,
    sample_rate_hz: u32,
    samples: Vec<f32>,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let model_path = settings
        .model
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(&app)?));
    let runtime = cache.runtime_for(&model_path);
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    tauri::async_runtime::spawn_blocking(move || {
        slugtale_lib::transcribe_captured_audio(&*runtime, audio).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Caches the loaded Whisper runtime across transcriptions so the model file is
/// read from disk once rather than on every call. The runtime is rebuilt only
/// when the configured model path changes.
#[derive(Default)]
struct WhisperRuntimeCache(Mutex<Option<Arc<slugtale_lib::LocalWhisperRuntime>>>);

impl WhisperRuntimeCache {
    fn runtime_for(&self, model_path: &Path) -> Arc<slugtale_lib::LocalWhisperRuntime> {
        let mut guard = self.0.lock().expect("whisper runtime cache mutex poisoned");
        if let Some(existing) = guard.as_ref() {
            if existing.model_path() == model_path {
                return existing.clone();
            }
        }

        let runtime = Arc::new(slugtale_lib::LocalWhisperRuntime::new(
            model_path.to_path_buf(),
        ));
        *guard = Some(runtime.clone());
        runtime
    }
}

#[derive(Default)]
struct CurrentPlatform {
    model_path: Option<PathBuf>,
}

impl CurrentPlatform {
    fn new(model_path: Option<PathBuf>) -> Self {
        Self { model_path }
    }

    fn for_readiness(model_dir: &Path) -> Self {
        Self::new(Some(slugtale_lib::default_model_path(model_dir)))
    }

    #[cfg(target_os = "macos")]
    fn macos_platform(&self) -> slugtale_lib::MacosPlatform {
        slugtale_lib::MacosPlatform::new(self.model_path.clone().unwrap_or_default())
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

fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    settings_path(app)
        .map(|path| slugtale_lib::load_settings(&path))
        .unwrap_or_default()
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

fn save_model_setting(app: &tauri::AppHandle, model_path: Option<PathBuf>) -> Result<(), String> {
    let mut settings = load_current_settings(app);
    settings.model = model_path.map(|path| path.to_string_lossy().to_string());
    save_current_settings(app, &settings)
}

impl slugtale_lib::PlatformReadiness for CurrentPlatform {
    fn microphone_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().microphone_granted();
        }

        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn insertion_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().insertion_granted();
        }

        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn local_model_present(&self) -> bool {
        self.model_path.as_ref().is_some_and(|path| path.exists())
    }
}

fn main() {
    tauri::Builder::default()
        .manage(WhisperRuntimeCache::default())
        .manage(RecordingFeedbackState::default())
        .manage(FocusTargetState::default())
        .manage(AudioCaptureState::default())
        .manage(HotkeyRegistrationState::default())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            slugtale_lib::setup_tray(app)?;
            setup_configured_hotkey(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if slugtale_lib::hides_on_close(window.label()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            show_settings,
            get_settings_readiness,
            get_settings,
            open_microphone_settings,
            open_text_insertion_settings,
            save_hotkey_settings,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcribe_captured_audio,
            dictation_event
        ])
        .run(tauri::generate_context!())
        .expect("error while running Slugtale");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_uses_default_local_model_when_settings_model_is_unset() {
        let model_dir = unique_test_dir("readiness-default-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(slugtale_lib::default_model_path(&model_dir), b"model").unwrap();

        let platform = CurrentPlatform::for_readiness(&model_dir);

        assert!(slugtale_lib::PlatformReadiness::local_model_present(
            &platform
        ));

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn readiness_uses_default_local_model_when_settings_model_is_stale() {
        let model_dir = unique_test_dir("readiness-stale-model-setting");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(slugtale_lib::default_model_path(&model_dir), b"model").unwrap();

        let stale_settings = slugtale_lib::Settings {
            model: Some(
                model_dir
                    .join("missing-custom-model.bin")
                    .to_string_lossy()
                    .to_string(),
            ),
            ..slugtale_lib::Settings::default()
        };
        let platform = CurrentPlatform::for_readiness(&model_dir);
        let report = slugtale_lib::settings_readiness_report(&stale_settings, &platform);
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(local_model.ready);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-main-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
