//! Settings commands: readiness, the Settings File writers, the OS permission
//! shortcuts, and Launch at Login.

use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;

use crate::dictation_bar_window::apply_dictation_bar_appearance;
use crate::hotkey_registration::update_registered_hotkey;
use crate::voice_activation;

use super::engines::current_engine_availability;
use super::platform::CurrentPlatform;
use super::{app_files, load_current_settings, record_diagnostic_event, update_current_settings};

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
        CurrentPlatform::new().microphone_granted()
    }

    fn insertion_granted(&self) -> bool {
        CurrentPlatform::new().insertion_granted()
    }

    fn local_model(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Option<slugtale_lib::LocalModelRef> {
        self.app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .local_model(settings)
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

pub(crate) fn build_activation_snapshot_for(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
) -> slugtale_lib::DictationActivation {
    readiness_snapshot_for(app, |_| input)
}

/// Tell the user which required items are missing and open Settings, where
/// they can act on each one.
pub(crate) fn report_not_ready(
    app: &tauri::AppHandle,
    report: &slugtale_lib::SettingsReadinessReport,
) {
    record_readiness_incomplete(app, report);

    let missing = slugtale_lib::missing_required_items(report);
    if !missing.is_empty() {
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
}

/// Name the unmet required items of a report in the Local Diagnostic Log, once.
/// Both the not-ready report and the readiness read go through here, so what is
/// logged and what the user is told cannot disagree.
fn record_readiness_incomplete(
    app: &tauri::AppHandle,
    report: &slugtale_lib::SettingsReadinessReport,
) {
    let missing = slugtale_lib::missing_required_items(report);
    if missing.is_empty() {
        return;
    }
    record_diagnostic_event(
        app,
        slugtale_lib::DiagnosticEvent::readiness_incomplete(&missing),
    );
}

/// The readiness report the Settings pane shows, and nothing else: a read. The
/// model warm-up that used to ride along here is the caller's to ask for when
/// it means one, so opening the pane cannot start an engine load as a side
/// effect of being looked at.
#[tauri::command]
pub(crate) fn get_settings_readiness(
    app: tauri::AppHandle,
) -> slugtale_lib::SettingsReadinessReport {
    let report = current_settings_readiness(&app);
    record_readiness_incomplete(&app, &report);
    report
}

#[tauri::command]
pub(crate) fn get_settings(app: tauri::AppHandle) -> slugtale_lib::Settings {
    load_current_settings(&app)
}

#[tauri::command]
pub(crate) fn save_hotkey_settings(
    app: tauri::AppHandle,
    hotkey: Option<String>,
    activation_mode: slugtale_lib::ActivationMode,
) -> Result<slugtale_lib::Settings, String> {
    app_files(&app).update_settings_and_apply(
        |settings| slugtale_lib::apply_hotkey_settings(settings, hotkey, activation_mode),
        |settings| update_registered_hotkey(&app, settings),
    )
}

#[tauri::command]
pub(crate) fn save_transcription_settings(
    app: tauri::AppHandle,
    speed_profile: slugtale_lib::SpeedProfile,
) -> Result<slugtale_lib::Settings, String> {
    update_current_settings(&app, |settings| {
        slugtale_lib::apply_transcription_settings(settings, speed_profile);
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn save_transcript_cleanup_settings(
    app: tauri::AppHandle,
    cleanup_mode: slugtale_lib::TranscriptCleanupMode,
) -> Result<slugtale_lib::Settings, String> {
    update_current_settings(&app, |settings| {
        slugtale_lib::apply_transcript_cleanup_settings(settings, cleanup_mode);
        Ok(())
    })
}

/// Save the Segment Pause length in seconds (ADR-0026). The value is validated
/// inside the Settings File's one transaction, so a rejected value writes
/// nothing. No part of the save reaches the running runtime: the next dictation
/// derives its pause from the Settings snapshot it pins at Start, so a dictation
/// already in progress keeps the length it armed with and no save can publish a
/// stale length to a dictation about to begin.
#[tauri::command]
pub(crate) fn save_segment_pause_settings(
    app: tauri::AppHandle,
    segment_pause_secs: i64,
) -> Result<slugtale_lib::Settings, String> {
    update_current_settings(&app, |settings| {
        slugtale_lib::apply_segment_pause_settings(settings, segment_pause_secs)
    })
}

/// Save whether dictation records from the built-in microphone when the
/// default one is Bluetooth. The next dictation picks the new microphone up.
#[tauri::command]
pub(crate) fn save_microphone_settings(
    app: tauri::AppHandle,
    prefer_built_in_microphone: bool,
) -> Result<slugtale_lib::Settings, String> {
    update_current_settings(&app, |settings| {
        slugtale_lib::apply_microphone_settings(settings, prefer_built_in_microphone);
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn voice_activation_supported() -> bool {
    voice_activation::supported()
}

/// Save the Voice Activation opt-in and bring the listener in line immediately.
/// Change the worker first, then persist. A failed worker must not leave a saved
/// "on" value while nothing is listening.
#[tauri::command]
pub(crate) fn save_voice_activation_settings(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    voice_activation::save_settings(&app, enabled)
}

#[tauri::command]
pub(crate) fn save_dictation_bar_settings(
    app: tauri::AppHandle,
    bar_position: slugtale_lib::BarPosition,
    accent_color: slugtale_lib::AccentColor,
    bar_display: slugtale_lib::BarDisplay,
) -> Result<slugtale_lib::Settings, String> {
    let settings = update_current_settings(&app, |settings| {
        slugtale_lib::apply_dictation_bar_settings(
            settings,
            bar_position,
            accent_color,
            bar_display,
        );
        Ok(())
    })?;
    apply_dictation_bar_appearance(&app, &settings);
    Ok(settings)
}

/// Register or unregister the app as an OS login item to match the desired state.
/// Backed by tauri-plugin-autostart (a macOS LaunchAgent), which keeps this off the
/// dictation hot path and gives the Windows port the same abstraction for free.
/// Launch at Login is informational and optional (slugtale-9bx): it is not a
/// Dictation Readiness item, and a disabled preference never blocks dictation.
pub(crate) fn set_launch_at_login_state(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|error| error.to_string())
    } else {
        autolaunch.disable().map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub(crate) fn save_launch_at_login(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<slugtale_lib::Settings, String> {
    app_files(&app).update_settings_and_apply(
        |settings| slugtale_lib::apply_launch_at_login_settings(settings, enabled),
        |settings| set_launch_at_login_state(&app, settings.launch_at_login),
    )
}
