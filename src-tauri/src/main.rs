#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri_plugin_updater::UpdaterExt;

mod app_paths;
mod dictation_bar_window;
mod hotkey_registration;
mod voice_activation;

use app_paths::{
    current_diagnostic_log, load_current_settings, load_current_usage, model_dir, model_manager,
    record_diagnostic_event, save_current_settings, usage_path,
};
use dictation_bar_window::{apply_dictation_bar_appearance, hide_dictation_bar, show_dictation_bar};
use hotkey_registration::{
    HotkeyRegistrationState, request_escape_registration, setup_configured_hotkey,
    update_registered_hotkey,
};
use slugtale_lib::{DictationHost, DictationPhase, DictationSurface};

use slugtale_lib::AppFiles;

/// The Typing Challenge window's label. It is created on demand rather than
/// declared in tauri.conf.json: most users never open it, and a hidden window
/// carrying a live webview for the life of the app is a cost with no benefit.
const TYPING_CHALLENGE_WINDOW: &str = "typing-challenge";

/// Whether the Typing Challenge window is on screen.
///
/// A flag rather than asking the window itself, because the only reader is the
/// global key worker and that runs on every hotkey press. Querying window
/// visibility from a background thread costs a round trip to the main thread,
/// and the hotkey path is the one place in this app where latency is felt.
use slugtale_lib::TypingChallengeOpen;

use slugtale_lib::TranscriptionProvider;

/// Drive the recording surface (ADR-0014) from a dictation lifecycle event:
/// play the start/stop sound and show or hide the Dictation Bar. The bar's Stop
/// and Cancel controls route here; the global hotkey lifecycle routes the
/// configured activation hotkey and Escape here while preserving text-target
/// focus.
#[tauri::command]
fn dictation_event(app: tauri::AppHandle, event: String) -> Result<(), String> {
    match event.as_str() {
        "start" => {
            let event = slugtale_lib::parse_dictation_ui_event("start")?;
            dictation_host(&app).handle_dictation_event(event)
        }
        "stop" => stop_active_dictation(&app),
        "cancel" => cancel_active_dictation(&app),
        other => Err(slugtale_lib::parse_dictation_ui_event(other).unwrap_err()),
    }
}

/// Stop from the Dictation Bar and reset the shared control at the same time.
/// Voice Activation can then trigger again without a stale active state.
fn stop_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.stop(),
        slugtale_lib::DictationEvent::Stop,
    )
}

/// Cancel through the same lifecycle bridge used by the global Escape handler
/// so a click on the Dictation Bar cannot leave toggle/hold state believing a
/// discarded dictation is still active.
fn cancel_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.cancel(),
        slugtale_lib::DictationEvent::Cancel,
    )
}

/// End the active dictation through the shared lifecycle bridge, disarming bare
/// Escape while the registration lock is held. When no lifecycle answered — no
/// registration yet, or nothing active — the fallback event still runs so a
/// leftover Dictation Bar never outlives its dictation.
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
            // Disarm failures only matter when the worker is gone entirely,
            // which means the app is shutting down; dropping the request is
            // then the honest outcome.
            let _ = request_escape_registration(&registration, false);
        }
        event
    };

    match event {
        Some(event) => dictation_host(app).handle_dictation_event(event),
        None => dictation_host(app).handle_dictation_event(fallback),
    }
}

/// Begin a dictation from any activation input — a Hotkey press or a Voice
/// Activation wake phrase — through one readiness-gated sequence. The hotkey
/// worker and Voice Activation used to run two private copies of this dance
/// and had already drifted on the typing-challenge guard and the rollback.
///
/// `set_escape(true)` arms bare Escape before recording starts, so there is no
/// active but uncancellable dictation; `set_escape(false)` disarms it. The
/// hotkey worker arms synchronously, Voice Activation asks the global-key
/// worker — the caller owns both that difference and the honest error report,
/// because an arm failure must roll the begin back like any other failed step.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn begin_dictation(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
    set_escape: &mut dyn FnMut(bool) -> Result<(), String>,
) -> Result<(), String> {
    // The Typing Challenge measures how fast the user types, so their hotkey
    // has to stay plain text for those thirty seconds. Swallowed here — before
    // any readiness snapshot is paid for or lifecycle state moves — so
    // releasing it later cannot resume anything. The guard stays in the host:
    // DictationControl only decides requests that reach it.
    if typing_challenge_is_open(app) {
        return Ok(());
    }

    let (activation, dictation_available) = {
        let activation = build_activation_snapshot_for(app, input);
        let available = activation.dictation_available();
        if !available {
            report_not_ready(app, &activation.report);
        }
        (Some(activation), available)
    };

    let event = {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        registration.control.begin(dictation_available)
    };
    let Ok(event) = event else {
        // NotReady has already had its user-facing report; AlreadyDictating
        // report; AlreadyDictating means a later input changes nothing.
        return Ok(());
    };

    // Recording has not started yet; arming Escape here keeps the window where
    // the lifecycle says dictating but Escape is not global down to nothing.
    if let Err(error) = set_escape(true) {
        if let Ok(mut registration) = app.state::<HotkeyRegistrationState>().0.lock() {
            registration.control.abandon_begin();
        }
        eprintln!("dictation did not start because global Escape could not be registered");
        return Err(error);
    }

    if let Err(error) = dictation_host(app).handle_dictation_event_with(event, activation) {
        // Roll the lifecycle back so the next activation can try again instead
        // of finding a discarded dictation still marked active.
        if let Ok(mut registration) = app.state::<HotkeyRegistrationState>().0.lock() {
            registration.control.abandon_begin();
            let _ = request_escape_registration(&registration, false);
        }
        return Err(error);
    }

    Ok(())
}

fn warm_effective_primary_engine(app: &tauri::AppHandle) {
    let settings = load_current_settings(app);
    let catalogue = app.state::<slugtale_lib::TranscriptionEngineCatalogue>();
    let Some(warm_up) = catalogue.prepare_primary_warm_up(&settings) else {
        return;
    };
    // Release before loading so switching engines never leaves two large
    // models resident on a memory-constrained Mac.
    catalogue.release_models_except(warm_up.engine());
    tauri::async_runtime::spawn_blocking(move || {
        let _ = warm_up.run();
    });
}

/// The Tauri adapter for the dictation lifecycle's surface: the bar window,
/// Settings reads, diagnostics, and failure notifications, reached through the
/// one AppHandle.
struct TauriSurface {
    app: tauri::AppHandle,
}

impl DictationSurface for TauriSurface {
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
        if let Some(window) = self.app.get_webview_window("dictation-bar") {
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

    fn diagnostic_log(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> slugtale_lib::SharedDiagnosticLog<slugtale_lib::FileDiagnosticSink> {
        current_diagnostic_log(&self.app, settings)
    }

    fn dictation_stack(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Result<slugtale_lib::DictationStack<slugtale_lib::FileDiagnosticSink>, String> {
        let diagnostic_log = self.diagnostic_log(settings);
        self.app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .dictation_stack(settings, diagnostic_log)
            .map_err(|error| error.to_string())
    }
}

/// The app's one dictation lifecycle host, managed by setup before any
/// activation input can arrive.
fn dictation_host(app: &tauri::AppHandle) -> Arc<DictationHost> {
    app.state::<Arc<DictationHost>>().inner().clone()
}

/// The host half of the Dictation Runtime's adapter: microphone cuts, the
/// transcription-and-insertion workflow, the Usage handoff, and the bar hide
/// that follows the final job. Everything OS-touching lives here; the runtime
/// owns ordering, rescue suspension, and panic containment.
struct AppHost {
    app: tauri::AppHandle,
    host: Arc<DictationHost>,
}

impl slugtale_lib::DictationRuntimeHost for AppHost {
    fn take_pause_segment(&mut self, cut: u64) -> Option<slugtale_lib::CapturedAudio> {
        self.host.take_dictation_segment(cut)
    }

    fn complete(
        &mut self,
        audio: slugtale_lib::CapturedAudio,
        position: slugtale_lib::DictationSegmentPosition,
    ) -> Result<slugtale_lib::DictationSegmentOutcome, String> {
        self.host.run_dictation_segment(audio, position)
    }

    fn last_job_settled(&mut self) {
        // The worker calls this after the final job settles whatever the
        // outcome, so the bar stays up until every earlier Segment Pause has
        // landed too, not just this last one (slugtale-0t4).
        hide_dictation_bar(&self.app);
    }
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

/// The app's answers to the five readiness facts, probed through one
/// interface so both snapshot paths see identical state (slugtale-g1o.6).
struct AppReadinessProbes<'a> {
    app: &'a tauri::AppHandle,
}

impl slugtale_lib::ReadinessProbes for AppReadinessProbes<'_> {
    fn settings(&self) -> slugtale_lib::Settings {
        load_current_settings(self.app)
    }

    fn microphone_granted(&self) -> bool {
        slugtale_lib::PlatformReadiness::microphone_granted(&CurrentPlatform::new())
    }

    fn insertion_granted(&self) -> bool {
        slugtale_lib::PlatformReadiness::insertion_granted(&CurrentPlatform::new())
    }

    fn local_model_ready(&self) -> bool {
        local_model_ready(self.app)
    }

    fn engine_availability(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Vec<(
        slugtale_lib::TranscriptionEngine,
        slugtale_lib::EngineAvailability,
    )> {
        current_engine_availability(self.app, settings)
    }
}

fn current_settings_readiness(app: &tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    readiness_snapshot_for(app, |settings| {
        if voice_activation::supported() && settings.voice_activation_enabled {
            slugtale_lib::DictationInput::VoiceActivation
        } else {
            slugtale_lib::DictationInput::Hotkey
        }
    })
    .report
}

/// One readiness snapshot over the app's probes. `input` decides which
/// activation's requirements the report reflects.
fn readiness_snapshot_for(
    app: &tauri::AppHandle,
    input: impl FnOnce(&slugtale_lib::Settings) -> slugtale_lib::DictationInput,
) -> slugtale_lib::DictationActivation {
    slugtale_lib::readiness_snapshot(&AppReadinessProbes { app }, input)
}

/// Whether the Whisper ggml file — or a user-selected custom model — is on disk.
fn local_model_ready(app: &tauri::AppHandle) -> bool {
    model_manager(app)
        .map(|manager| manager.ready())
        .unwrap_or_else(|_| {
            load_current_settings(app)
                .model
                .as_ref()
                .is_some_and(|path| PathBuf::from(path).exists())
        })
}

fn build_activation_snapshot_for(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
) -> slugtale_lib::DictationActivation {
    readiness_snapshot_for(app, |_| input)
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
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .availability(settings)
}

/// Tell the user which required items are missing and open Settings, where
/// they can act on each one.
fn report_not_ready(
    app: &tauri::AppHandle,
    report: &slugtale_lib::SettingsReadinessReport,
) -> bool {
    let missing = slugtale_lib::missing_required_items(report);
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
    true
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
        warm_effective_primary_engine(&app);
    }
    if !report.dictation_available {
        let missing = slugtale_lib::missing_required_items(&report);
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
    slugtale_lib::apply_and_persist(
        &previous,
        |settings| slugtale_lib::apply_hotkey_settings(settings, hotkey, activation_mode),
        |settings| update_registered_hotkey(&app, settings),
        |settings| save_current_settings(&app, settings),
    )
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
fn voice_activation_supported() -> bool {
    voice_activation::supported()
}

/// Save the Voice Activation opt-in and bring the listener in line immediately.
/// Change the worker first, then persist. A failed worker must not leave a saved
/// "on" value while nothing is listening.
#[tauri::command]
fn save_voice_activation_settings(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    voice_activation::save_settings(&app, enabled)
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
    slugtale_lib::apply_and_persist(
        &previous,
        |settings| slugtale_lib::apply_launch_at_login_settings(settings, enabled),
        |settings| set_launch_at_login_state(&app, settings.launch_at_login),
        |settings| save_current_settings(&app, settings),
    )
}

/// What Settings renders for one app-update check (slugtale-9pr). `version` is
/// the newer build's version when one is available, and is what the user sees
/// before deciding to install.
#[derive(Debug, Clone, serde::Serialize)]
struct AppUpdateView {
    available: bool,
    version: Option<String>,
}

impl AppUpdateView {
    fn none() -> Self {
        Self {
            available: false,
            version: None,
        }
    }

    fn available(version: String) -> Self {
        Self {
            available: true,
            version: Some(version),
        }
    }
}

/// Ask GitHub Releases whether a newer signed build exists (ADR-0022). The
/// endpoint and public key live in tauri.conf.json; signature verification is
/// enforced by the plugin before anything is staged.
#[tauri::command]
async fn check_for_app_update(app: tauri::AppHandle) -> Result<AppUpdateView, String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater.check().await.map_err(|error| error.to_string())?;
    Ok(match update {
        Some(update) => AppUpdateView::available(update.version),
        None => AppUpdateView::none(),
    })
}

/// Download, verify, stage, and relaunch into a checked app update. The restart
/// never returns; the process replaces itself with the new build.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
async fn install_app_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no app update is available".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|error| error.to_string())?;
    app.restart();
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
) -> Result<Arc<dyn TranscriptionProvider>, String> {
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .whisper_provider(settings)
        .ok_or_else(|| "could not resolve a local model directory for Whisper".to_string())
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
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
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
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .apple_provider();
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
    // Start warming the newly effective engine now so the first dictation
    // after the change does not pay for a cold model load.
    warm_effective_primary_engine(&app);
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
                // Throttle IPC traffic: the initial update, then one per ~1 MB,
                // plus the final update (slugtale-dtl).
                let mut forward = slugtale_lib::throttled_progress(move |progress| {
                    let _ = on_progress.send(progress);
                });
                manager
                    .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())??;
            if status.present {
                warm_effective_primary_engine(&app);
            }
        }
        slugtale_lib::TranscriptionEngine::Parakeet => {
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            let asset_dir = provider.asset_dir().to_path_buf();
            tauri::async_runtime::spawn_blocking(move || {
                // Throttle IPC traffic: the initial update, then one per ~1 MB,
                // plus the final update (slugtale-dtl).
                let mut forward = slugtale_lib::throttled_progress(move |progress| {
                    let _ = on_progress.send(progress);
                });
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
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .apple_provider();
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
            let provider = app
                .state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .parakeet_provider()
                .ok_or_else(|| "transcription engines are not ready yet".to_string())?;
            slugtale_lib::delete_parakeet_assets(provider.asset_dir())
                .map_err(|error| error.to_string())?;
            provider.refresh_availability();
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
        today: usage_span(
            &slugtale_lib::totals_for_day(&usage, today),
            words_per_minute,
        ),
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
fn set_typing_estimate(
    app: tauri::AppHandle,
    estimate: Option<u32>,
) -> Result<UsageSummary, String> {
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
        // Throttle IPC traffic: the initial update, then one per ~1 MB, plus
        // the final update (slugtale-dtl).
        let mut forward = slugtale_lib::throttled_progress(move |progress| {
            let _ = on_progress.send(progress);
        });
        manager
            .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    if status.present {
        warm_effective_primary_engine(&app);
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

/// Start the Dictation Runtime's Usage writer body (ADR-0025): the opt-in is
/// checked here, at the last possible moment, so a segment that was in flight
/// when the user turned storing off does not land in a file they just asked to
/// be deleted. Every failure below is a skip, not an error.
fn usage_writer(app: tauri::AppHandle) -> std::sync::Arc<slugtale_lib::UsageSink> {
    std::sync::Arc::new(move |date, segment| {
        if !load_current_settings(&app).store_usage {
            return;
        }
        let Some(path) = usage_path(&app) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }

        let mut usage = slugtale_lib::load_usage(&path);
        slugtale_lib::record_counted_segment(&mut usage, date, segment);
        if let Err(error) = slugtale_lib::save_usage(&path, &usage) {
            eprintln!("could not write the usage file: {error}");
            return;
        }

        // The Usage pane is the only surface that shows any of this, so
        // it is the only thing told. Nothing reaches the Pill, the tray,
        // or a notification (ADR-0025).
        if let Some(window) = app.get_webview_window("settings") {
            let _ = window.emit("usage-changed", ());
        }
    })
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
        .manage(slugtale_lib::TranscriptionEngineCatalogue::default())
        .manage(HotkeyRegistrationState::default())
        .manage(TypingChallengeOpen::default())
        .manage(voice_activation::VoiceActivationState::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // Every local file path resolves through this one store, so it has
            // to exist before anything that reads or writes a file.
            app.manage(AppFiles::from_app(app.handle()));
            // The dictation lifecycle host owns its own state; it is managed
            // here, before the hotkey worker starts, so every activation input
            // finds it in place.
            let host = Arc::new(DictationHost::new(Arc::new(TauriSurface {
                app: app.handle().clone(),
            })));
            app.manage(host.clone());
            slugtale_lib::setup_tray(app)?;
            // The Dictation Segment worker outlives every dictation: it is what
            // keeps segments landing in the order they were spoken.
            // The runtime probes the capture ring's voiced-sample watermark at
            // the moment a Pause Flush is due — the microphone half of the
            // watermark cut (ADR-0026).
            let watermark_host = host.clone();
            let runtime = slugtale_lib::DictationRuntime::start(
                AppHost {
                    app: app.handle().clone(),
                    host: host.clone(),
                },
                move || watermark_host.voice_watermark(),
                usage_writer(app.handle().clone()),
            )
            .map_err(std::io::Error::other)?;
            host.set_runtime(Arc::new(runtime))
                .map_err(std::io::Error::other)?;
            // Usage writes happen off the Dictation Workflow path (ADR-0025), so
            // the queue that carries them has to exist before the first segment.
            // The Dictation Runtime starts that writer; nothing to do here.
            // The hotkey worker starts last: from here on every activation
            // input finds both the host and the runtime in place, so a press
            // during setup cannot hit DictationHost::runtime()'s
            // "dictation runtime started" panic.
            setup_configured_hotkey(app)?;
            // Reconcile the OS login item with the stored preference so a moved or
            // rebuilt app (dev binaries change path) does not drift out of sync.
            let settings = load_current_settings(app.handle());
            let _ = set_launch_at_login_state(app.handle(), settings.launch_at_login);
            if let Ok(model_dir) = model_dir(app.handle()) {
                app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                    .set_model_dir(model_dir);
            }
            warm_effective_primary_engine(app.handle());
            // Prepare Audio Capture while idle so the first Hotkey does not pay
            // for device discovery and ring allocation (slugtale-g1o.3). Only
            // when the microphone permission is already granted: preparation
            // must never prompt, and a denied microphone stays on the normal
            // permission path.
            if slugtale_lib::PlatformReadiness::microphone_granted(&CurrentPlatform::new()) {
                dictation_host(app.handle()).prepare_capture();
            }
            // Voice Activation is opt-in: the always-on listener only starts
            // when a previously saved preference asks for it (slugtale-e95).
            if let Err(error) = voice_activation::sync_worker(
                app.handle(),
                load_current_settings(app.handle()).voice_activation_enabled,
            ) {
                eprintln!("voice activation worker did not start: {error}");
            }
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
            get_settings_readiness,
            get_settings,
            dictation_bar_displays,
            open_microphone_settings,
            open_text_insertion_settings,
            save_hotkey_settings,
            save_transcription_settings,
            save_transcript_cleanup_settings,
            voice_activation_supported,
            save_voice_activation_settings,
            save_dictation_bar_settings,
            dictation_bar_pointer_over,
            save_launch_at_login,
            check_for_app_update,
            install_app_update,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcription_engines,
            set_transcription_engines,
            install_engine_assets,
            remove_engine_assets,
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
            app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
                .shutdown();
        }
    });

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
