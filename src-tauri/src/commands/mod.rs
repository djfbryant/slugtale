//! The binary's Tauri command adapters, grouped by the domain each one serves.
//!
//! `main.rs` owns startup: the builder, the managed state, the window events and
//! the one `generate_handler!` list that registers every command. These modules
//! own the adapters themselves, so each command's neighbours are the commands
//! that share its domain — settings, dictation, engines, usage, updates — rather
//! than everything that happened to land in `main.rs` first. The domain rules
//! themselves stay in `slugtale_lib`; the modules here only reach the store and
//! the Tauri runtime on their behalf.

pub(crate) mod dictation;
pub(crate) mod engines;
pub(crate) mod platform;
pub(crate) mod settings;
pub(crate) mod updates;
pub(crate) mod usage;

use tauri::Manager;

use slugtale_lib::AppFiles;

/// The app's one file store. Every command, the Dictation Surface, and the
/// readiness probes reach the Settings File, the Usage File, the Local
/// Diagnostic Log and the model directory through it, so there is one answer to
/// where a file is and one place that writes one.
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

/// Change the current Settings as one transaction and get the value that stuck.
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

/// Start warming the engine the current Settings choose, off the dictation path.
/// Called when readiness reports a usable Local Model, when an engine is
/// installed or chosen, and once at startup.
pub(crate) fn warm_effective_primary_engine(app: &tauri::AppHandle) {
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
