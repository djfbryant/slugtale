
pub(crate) mod dictation;
pub(crate) mod engines;
pub(crate) mod platform;
pub(crate) mod settings;
pub(crate) mod updates;
pub(crate) mod usage;

use tauri::Manager;

use slugtale_lib::{AppFiles, TypingChallengeOpen};

pub(crate) fn app_files(app: &tauri::AppHandle) -> AppFiles {
    app.state::<AppFiles>().inner().clone()
}

/// Read the current Settings. Every command that *changes* one goes through the
/// store's transaction instead — `update_settings` or `update_settings_and_apply`
/// on [`AppFiles`] — because this returns a snapshot, and saving a snapshot read
/// before another writer changed it is how a background model install used to
/// overwrite a newer choice.
pub(crate) fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    app_files(app).settings()
}

pub(crate) fn update_current_settings(
    app: &tauri::AppHandle,
    change: impl FnOnce(&mut slugtale_lib::Settings) -> Result<(), String>,
) -> Result<slugtale_lib::Settings, String> {
    app_files(app).update_settings(change)
}

pub(crate) fn model_manager(
    app: &tauri::AppHandle,
) -> Result<slugtale_lib::LocalModelManager, String> {
    app_files(app).model_manager()
}

pub(crate) fn record_diagnostic_event(app: &tauri::AppHandle, event: slugtale_lib::DiagnosticEvent) {
    app_files(app).record_diagnostic_event(event);
}

pub(crate) fn warm_effective_primary_engine(app: &tauri::AppHandle) {
    let settings = load_current_settings(app);
    let catalogue = app.state::<slugtale_lib::TranscriptionEngineCatalogue>();
    let Some(warm_up) = catalogue.begin_primary_warm_up(&settings) else {
        return;
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _ = warm_up.run();
    });
}

pub(crate) fn dictation_input_is_inert(app: &tauri::AppHandle) -> bool {
    app.state::<TypingChallengeOpen>().get()
}
