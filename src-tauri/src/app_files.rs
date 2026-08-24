//! File locations: the one module that knows where Slugtale's local files live.
//!
//! Settings File, Usage File, Local Diagnostic Log, and the model directory
//! all resolve from the app's config and data directories. Before this module,
//! each path was a private free function in the Tauri tier and every consumer
//! paid its own resolution; moving a file meant finding four functions. The
//! store deliberately does not cache values — the Local Model Manager writes
//! the Settings File through its own path, so a cache here would silently drop
//! that update.

use crate::{
    load_settings, load_usage, save_settings, FileDiagnosticSink, Settings, SharedDiagnosticLog,
    UsageFile,
};
use std::path::PathBuf;

const SETTINGS_FILE: &str = "settings.json";
const USAGE_FILE: &str = "usage.json";
const DIAGNOSTIC_LOG_FILE: &str = "diagnostics.log";
const MODELS_DIR: &str = "models";

#[derive(Clone)]
pub struct AppFiles {
    /// Config-dir files are the user-facing ones (ADR-0018): the Settings File
    /// and the Usage File sit side by side so opting out deletes one obvious
    /// file and nothing else.
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
}

impl AppFiles {
    pub fn from_app(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        Self {
            config_dir: app.path().app_config_dir().ok(),
            data_dir: app.path().app_data_dir().ok(),
        }
    }

    #[cfg(test)]
    fn from_dirs(config_dir: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        Self {
            config_dir,
            data_dir,
        }
    }

    pub fn settings_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(SETTINGS_FILE))
    }

    /// The Usage File (CONTEXT.md): a sibling of the Settings File.
    pub fn usage_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(USAGE_FILE))
    }

    pub fn diagnostic_log_path(&self) -> Option<PathBuf> {
        self.config_dir
            .as_ref()
            .map(|dir| dir.join(DIAGNOSTIC_LOG_FILE))
    }

    pub fn model_dir(&self) -> Result<PathBuf, String> {
        self.data_dir
            .as_ref()
            .map(|dir| dir.join(MODELS_DIR))
            .ok_or_else(|| "could not resolve model directory".to_string())
    }

    /// Current Settings, or defaults when no file exists yet.
    pub fn settings(&self) -> Settings {
        self.settings_path()
            .map(|path| load_settings(&path))
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self
            .settings_path()
            .ok_or_else(|| "could not resolve settings path".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        save_settings(&path, settings).map_err(|error| error.to_string())
    }

    pub fn usage(&self) -> UsageFile {
        self.usage_path()
            .map(|path| load_usage(&path))
            .unwrap_or_default()
    }

    /// A log handle gated by the user's diagnostic-logging preference; an
    /// unresolvable path degrades to a no-op sink rather than failing dictation.
    pub fn diagnostic_log(&self, enabled: bool) -> SharedDiagnosticLog<FileDiagnosticSink> {
        let sink = self
            .diagnostic_log_path()
            .map(FileDiagnosticSink::new)
            .unwrap_or_else(FileDiagnosticSink::unavailable);
        SharedDiagnosticLog::new(enabled, sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-app-files-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn every_file_resolves_from_its_directory() {
        let config_dir = unique_test_dir("config");
        let data_dir = unique_test_dir("data");
        let files = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));

        assert_eq!(
            files.settings_path(),
            Some(config_dir.join("settings.json"))
        );
        assert_eq!(files.usage_path(), Some(config_dir.join("usage.json")));
        assert_eq!(
            files.diagnostic_log_path(),
            Some(config_dir.join("diagnostics.log"))
        );
        assert_eq!(files.model_dir().unwrap(), data_dir.join("models"));

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn an_unresolvable_config_dir_answers_none_and_defaults() {
        let files = AppFiles::from_dirs(None, Some(unique_test_dir("data-only")));

        assert_eq!(files.settings_path(), None);
        assert_eq!(files.usage_path(), None);
        assert_eq!(
            files.settings(),
            Settings::default(),
            "no file means defaults"
        );
    }

    #[test]
    fn saved_settings_round_trip_through_the_store() {
        let config_dir = unique_test_dir("round-trip");
        let files = AppFiles::from_dirs(Some(config_dir.clone()), None);

        let mut settings = Settings::default();
        settings.diagnostic_logging = true;
        files.save_settings(&settings).unwrap();

        assert_eq!(files.settings().diagnostic_logging, true);

        std::fs::remove_dir_all(config_dir).ok();
    }
}
