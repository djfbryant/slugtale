#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
const DICTATION_ESCAPE_KEY: &str = "Escape";

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

use slugtale_lib::{
    DiagnosticAsrRuntime, DiagnosticInsertionRescue, DiagnosticTextInsertion, FileDiagnosticSink,
    SharedDiagnosticLog,
};

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
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        let was_dictating = registration
            .adapter
            .as_ref()
            .is_some_and(|adapter| adapter.is_dictating());
        if was_dictating {
            if let Some(adapter) = registration.adapter.as_mut() {
                let should_register = adapter.on_global_key(
                    slugtale_lib::DictationKey::Escape,
                    slugtale_lib::HotkeyInput::Pressed,
                );
                request_escape_registration(&registration, should_register);
            }
            return Ok(());
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

async fn complete_captured_dictation(
    app: tauri::AppHandle,
    audio: slugtale_lib::CapturedAudio,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let diagnostic_log = current_diagnostic_log(&app, &settings);
    let model_path = settings
        .model
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(&app)?));
    let runtime = app
        .state::<slugtale_lib::WhisperRuntimeCache>()
        .runtime_for(&model_path);
    // Apply the current Transcription Speed Profile before decoding so the user's
    // accuracy/speed choice takes effect without reloading the model.
    runtime.set_speed_profile(settings.speed_profile);
    let target_pid = app
        .state::<FocusTargetState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| *guard);

    tauri::async_runtime::spawn_blocking(move || {
        // Bring the user's app back to the front so synthesized keystrokes land
        // in its focused field rather than wherever focus drifted (slugtale-squ).
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
            let runtime = DiagnosticAsrRuntime::new(&*runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(&runtime, &insertion, &rescue);
            workflow.complete(audio).map_err(|error| error.to_string())
        }

        #[cfg(target_os = "windows")]
        {
            let insertion = slugtale_lib::WindowsTextInsertion::new();
            let rescue = slugtale_lib::WindowsInsertionRescue::new();
            let runtime = DiagnosticAsrRuntime::new(&*runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(&runtime, &insertion, &rescue);
            workflow.complete(audio).map_err(|error| error.to_string())
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
            let runtime = DiagnosticAsrRuntime::new(&*runtime, diagnostic_log.clone());
            let insertion = DiagnosticTextInsertion::new(&insertion, diagnostic_log.clone());
            let rescue = DiagnosticInsertionRescue::new(&rescue, diagnostic_log);
            let workflow = slugtale_lib::DictationWorkflow::new(&runtime, &insertion, &rescue);
            workflow.complete(audio).map_err(|error| error.to_string())
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = (runtime, audio, target_pid);
            Err("text insertion is not implemented for this platform".to_string())
        }
    })
    .await
    .map_err(|error| error.to_string())?
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
                                    .adapter
                                    .as_ref()
                                    .map(|adapter| !adapter.is_dictating())
                            })
                            .unwrap_or(false);

                        if starts_dictation
                            && sync_escape_registration(&app, &mut escape_registered, true)
                                .is_err()
                        {
                            eprintln!(
                                "dictation did not start because global Escape could not be registered"
                            );
                            continue;
                        }

                        let should_register = app
                            .state::<HotkeyRegistrationState>()
                            .0
                            .lock()
                            .ok()
                            .and_then(|mut registration| {
                                registration
                                    .adapter
                                    .as_mut()
                                    .map(|adapter| adapter.on_global_key(key, input))
                            });
                        if let Some(should_register) = should_register {
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
        let appearance = DictationBarAppearance::from_settings(&load_current_settings(app));
        // Tell the frontend which state to render before showing, so the bar never
        // flashes a stale "recording" pill when it reappears for transcription.
        let _ = window.emit("dictation-phase", phase.as_str());
        // Same reason for the appearance: a bar that appears in the old accent or
        // aligned to the old edge and then jumps is worse than one that never did.
        let _ = window.emit("dictation-appearance", appearance.clone());
        // Placing the bar reads monitor geometry, and those reads block until the
        // main thread answers them. The global-key worker calls this while holding
        // the hotkey registration lock, and the main thread takes that same lock on
        // the next key transition — doing the work inline deadlocks both threads and
        // freezes the tray. Hand the window work to the main thread instead of
        // waiting on it (slugtale-1n4).
        let _ = app.run_on_main_thread(move || {
            position_dictation_bar(&window, appearance.position);
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
    }
}

/// Read the display the Dictation Bar is on, in the form the pure geometry
/// wants. Falls back to the primary monitor when the current one is unknown.
fn dictation_bar_monitor(window: &tauri::WebviewWindow) -> Option<slugtale_lib::MonitorGeometry> {
    let monitor = match window.current_monitor() {
        Ok(Some(monitor)) => monitor,
        _ => match window.primary_monitor() {
            Ok(Some(monitor)) => monitor,
            _ => return None,
        },
    };

    let screen = monitor.size();
    let origin = monitor.position();

    Some(slugtale_lib::MonitorGeometry {
        origin_x: origin.x,
        origin_y: origin.y,
        width: screen.width,
        height: screen.height,
        scale_factor: monitor.scale_factor(),
    })
}

/// Place the Dictation Bar along the bottom edge of the active display, at the
/// corner the user chose, above the Dock. The geometry itself lives in lib.rs;
/// this only supplies the live monitor and window reads.
fn position_dictation_bar(window: &tauri::WebviewWindow, position: slugtale_lib::BarPosition) {
    let Some(monitor) = dictation_bar_monitor(window) else {
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

#[tauri::command]
fn get_settings_readiness(app: tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    let settings = load_current_settings(&app);
    let platform = CurrentPlatform::new();
    let local_model_ready = model_manager(&app)
        .map(|manager| manager.ready())
        .unwrap_or_else(|_| {
            settings
                .model
                .as_ref()
                .is_some_and(|path| PathBuf::from(path).exists())
        });
    let report = slugtale_lib::settings_readiness_report(&settings, &platform, local_model_ready);
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
fn save_dictation_bar_settings(
    app: tauri::AppHandle,
    bar_position: slugtale_lib::BarPosition,
    accent_color: slugtale_lib::AccentColor,
) -> Result<slugtale_lib::Settings, String> {
    let mut settings = load_current_settings(&app);
    slugtale_lib::apply_dictation_bar_settings(&mut settings, bar_position, accent_color);
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
    let _ = app.run_on_main_thread(move || {
        position_dictation_bar(&window, appearance.position);
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
    let runtime = cache.runtime_for(&model_path);
    runtime.set_speed_profile(settings.speed_profile);
    let diagnostic_log = current_diagnostic_log(&app, &settings);
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    tauri::async_runtime::spawn_blocking(move || {
        let runtime = DiagnosticAsrRuntime::new(&*runtime, diagnostic_log);
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
        .manage(HotkeyRegistrationState::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            slugtale_lib::setup_tray(app)?;
            setup_configured_hotkey(app)?;
            // Reconcile the OS login item with the stored preference so a moved or
            // rebuilt app (dev binaries change path) does not drift out of sync.
            let settings = load_current_settings(app.handle());
            let _ = set_launch_at_login_state(app.handle(), settings.launch_at_login);
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
        })
        .invoke_handler(tauri::generate_handler![
            show_settings,
            get_settings_readiness,
            get_settings,
            open_microphone_settings,
            open_text_insertion_settings,
            save_hotkey_settings,
            save_transcription_settings,
            save_dictation_bar_settings,
            dictation_bar_pointer_over,
            save_launch_at_login,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcribe_captured_audio,
            dictation_event
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_uses_default_local_model_when_settings_model_is_unset() {
        let model_dir = unique_test_dir("readiness-default-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(slugtale_lib::default_model_path(&model_dir), b"model").unwrap();

        let platform = CurrentPlatform::new();
        let settings = slugtale_lib::Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..slugtale_lib::Settings::default()
        };
        let report = slugtale_lib::settings_readiness_report(
            &settings,
            &platform,
            slugtale_lib::local_model_ready(&model_dir),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(local_model.ready);

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
        let platform = CurrentPlatform::new();
        let report = slugtale_lib::settings_readiness_report(
            &stale_settings,
            &platform,
            slugtale_lib::local_model_ready(&model_dir),
        );
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
