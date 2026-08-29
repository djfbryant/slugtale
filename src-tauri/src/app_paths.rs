//! Where Slugtale's local files live and how they are read and written.
//!
//! Every path resolves through the one [`slugtale_lib::AppFiles`] store managed
//! at startup, so commands, the dictation surface, and the readiness probes all
//! see the same Settings, Usage, model, and diagnostic files.

use std::path::PathBuf;

use slugtale_lib::AppFiles;
use tauri::Manager;

pub(super) fn app_files(app: &tauri::AppHandle) -> AppFiles {
    app.state::<AppFiles>().inner().clone()
}

pub(super) fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app_files(app).settings_path()
}

pub(super) fn model_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_files(app).model_dir()
}

pub(super) fn model_manager(
    app: &tauri::AppHandle,
) -> Result<slugtale_lib::LocalModelManager, String> {
    let settings_path =
        settings_path(app).ok_or_else(|| "could not resolve settings path".to_string())?;
    Ok(slugtale_lib::LocalModelManager::new(
        model_dir(app)?,
        settings_path,
    ))
}

/// The Usage File (CONTEXT.md): a sibling of the Settings File, so opting out
/// deletes one obvious file and nothing else.
pub(super) fn usage_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app_files(app).usage_path()
}

pub(super) fn load_current_usage(app: &tauri::AppHandle) -> slugtale_lib::UsageFile {
    app_files(app).usage()
}

pub(super) fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    app_files(app).settings()
}

pub(super) fn current_diagnostic_log(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> slugtale_lib::SharedDiagnosticLog<slugtale_lib::FileDiagnosticSink> {
    app_files(app).diagnostic_log(settings.diagnostic_logging)
}

pub(super) fn record_diagnostic_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DiagnosticEvent,
) {
    let settings = load_current_settings(app);
    current_diagnostic_log(app, &settings).record(event);
}

pub(super) fn save_current_settings(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Result<(), String> {
    app_files(app).save_settings(settings)
}
