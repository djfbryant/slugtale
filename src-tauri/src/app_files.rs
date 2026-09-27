//! Where Slugtale's local files live, and the only code that reads or writes
//! them.
//!
//! Settings File, Usage File, Local Diagnostic Log, and the model directory all
//! resolve from the app's config and data directories. Before this module, each
//! path was a private free function in the Tauri tier and every consumer paid
//! its own resolution; moving a file meant finding four functions.
//!
//! The store is the whole answer now, reads and writes both. It owns the only
//! two recorders that touch a file on a background path, the Local Diagnostic
//! Log and the Counted Segment writer, so the Dictation Runtime reaches a file
//! the same way every Settings command does. It is also the only writer of the
//! Settings File: the Local Model Manager writes through it, which is what lets
//! [`AppFiles::settings`] be a clone of a value already in memory instead of a
//! disk read behind a process-wide file lock on every call.

use crate::{
    delete_usage, load_settings, load_usage, record_counted_segment, save_settings, save_usage,
    CountedSegment, DiagnosticEvent, FileDiagnosticSink, LocalDate, LocalModelManager, Settings,
    SharedDiagnosticLog, UsageFile,
};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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
    /// The Settings File, read once and handed out as clones. Every clone of the
    /// store shares this, so a write through one door is the next read through
    /// every other.
    settings: Arc<Mutex<Option<Settings>>>,
    /// One Local Diagnostic Log for the app's life. The recorders built from it
    /// are cheap clones of one shared log rather than one log per caller.
    diagnostic_log: SharedDiagnosticLog<FileDiagnosticSink>,
    /// How many times the Settings File has actually been read. Tests use it to
    /// show that a diagnostic event or a second read costs no I/O.
    #[cfg(test)]
    disk_reads: Arc<AtomicUsize>,
}

impl AppFiles {
    pub fn from_app(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        Self::with_dirs(
            app.path().app_config_dir().ok(),
            app.path().app_data_dir().ok(),
        )
    }

    #[cfg(test)]
    pub(crate) fn from_dirs_for_test(
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
    ) -> Self {
        Self::with_dirs(config_dir, data_dir)
    }

    #[cfg(test)]
    fn from_dirs(config_dir: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        Self::with_dirs(config_dir, data_dir)
    }

    fn with_dirs(config_dir: Option<PathBuf>, data_dir: Option<PathBuf>) -> Self {
        let sink = config_dir
            .as_ref()
            .map(|dir| FileDiagnosticSink::new(dir.join(DIAGNOSTIC_LOG_FILE)))
            .unwrap_or_else(FileDiagnosticSink::unavailable);

        Self {
            config_dir,
            data_dir,
            settings: Arc::new(Mutex::new(None)),
            // Enabled per ask, from the current preference. The log is created
            // once, so a later change has to reach it through `set_enabled`.
            diagnostic_log: SharedDiagnosticLog::new(false, sink),
            #[cfg(test)]
            disk_reads: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn settings_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(SETTINGS_FILE))
    }

    /// The Usage File (CONTEXT.md): a sibling of the Settings File.
    pub fn usage_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(USAGE_FILE))
    }

    pub fn model_dir(&self) -> Result<PathBuf, String> {
        self.data_dir
            .as_ref()
            .map(|dir| dir.join(MODELS_DIR))
            .ok_or_else(|| "could not resolve model directory".to_string())
    }

    /// The Local Model Manager (CONTEXT.md), which downloads and deletes the
    /// managed model and points the Settings File at it.
    pub fn model_manager(&self) -> Result<LocalModelManager, String> {
        LocalModelManager::new(self.clone()).map_err(|error| error.to_string())
    }

    /// Current Settings, or defaults when no file exists yet. Cloned from the
    /// value the store already holds; the file is read once, on the first call
    /// after a write.
    pub fn settings(&self) -> Settings {
        let mut cached = match self.settings.lock() {
            Ok(cached) => cached,
            Err(poisoned) => poisoned.into_inner(),
        };
        if cached.is_none() {
            *cached = Some(self.read_settings());
        }
        cached.clone().expect("just filled")
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let path = self
            .settings_path()
            .ok_or_else(|| "could not resolve settings path".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        // Write first, then adopt: a failed save must leave the store holding
        // what the file says, which is the old value.
        save_settings(&path, settings).map_err(|error| error.to_string())?;
        self.adopt_settings(settings.clone());
        Ok(())
    }

    pub fn usage(&self) -> UsageFile {
        self.usage_path()
            .map(|path| load_usage(&path))
            .unwrap_or_default()
    }

    pub fn save_usage(&self, usage: &UsageFile) -> Result<(), String> {
        let path = self
            .usage_path()
            .ok_or_else(|| "could not resolve usage path".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        save_usage(&path, usage).map_err(|error| error.to_string())
    }

    /// Take one Counted Segment toward Usage (ADR-0025), and say whether it was
    /// written.
    ///
    /// Two decisions live here rather than in the caller. The opt-in is read at
    /// the last possible moment, so a segment that was in flight when the user
    /// turned storing off does not land in a file they just asked to be deleted.
    /// And a write that fails is a skip, not an error: the dictation it counted
    /// has already been inserted, and a Daily Usage Record is never worth a
    /// failed one.
    pub fn record_counted_segment(&self, date: LocalDate, segment: CountedSegment) -> bool {
        if !self.settings().store_usage || self.usage_path().is_none() {
            return false;
        }
        let mut usage = self.usage();
        record_counted_segment(&mut usage, date, segment);
        match self.save_usage(&usage) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("could not write the usage file: {error}");
                false
            }
        }
    }

    /// The app's one Local Diagnostic Log, gated by the current preference.
    /// Every recorder built from it appends to the same file under the same
    /// lock; the old forwarding layer built a fresh log per call, so nothing was
    /// shared and two recorders could interleave around each other.
    pub fn diagnostic_log(&self, enabled: bool) -> SharedDiagnosticLog<FileDiagnosticSink> {
        self.diagnostic_log.set_enabled(enabled);
        self.diagnostic_log.clone()
    }

    /// Record one Local Diagnostic Log event under the current preference. Every
    /// event Slugtale logs goes through here, so the gate is one comparison
    /// rather than one per caller.
    pub fn record_diagnostic_event(&self, event: DiagnosticEvent) {
        self.diagnostic_log(self.settings().diagnostic_logging)
            .record(event);
    }

    fn read_settings(&self) -> Settings {
        #[cfg(test)]
        self.disk_reads.fetch_add(1, Ordering::Relaxed);
        self.settings_path()
            .map(|path| load_settings(&path))
            .unwrap_or_default()
    }

    fn adopt_settings(&self, settings: Settings) {
        match self.settings.lock() {
            Ok(mut cached) => *cached = Some(settings),
            Err(poisoned) => *poisoned.into_inner() = Some(settings),
        }
    }

    /// Delete the Usage File outright when the user turns storing off, so
    /// "stop storing this" means the stored thing is gone rather than left to
    /// rot unread. The Typing Baseline is in the Settings File and is untouched.
    pub fn delete_usage_file(&self) -> Result<(), String> {
        let Some(path) = self.usage_path() else {
            return Ok(());
        };
        delete_usage(&path).map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn settings_file_reads(&self) -> usize {
        self.disk_reads.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{record_counted_segment, CountedSegment, DiagnosticEvent, LocalDate};

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

    fn test_store(name: &str) -> (AppFiles, PathBuf, PathBuf) {
        let config_dir = unique_test_dir(name);
        let data_dir = unique_test_dir(&format!("{name}-data"));
        (
            AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone())),
            config_dir,
            data_dir,
        )
    }

    fn counted() -> CountedSegment {
        CountedSegment {
            words: 12,
            speaking_seconds: 4.0,
            starts_dictation: true,
        }
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
    fn a_settings_write_is_visible_to_the_next_read_through_every_door() {
        let (files, config_dir, data_dir) = test_store("every-door");
        let mut settings = Settings::default();
        settings.hotkey = Some("cmd+shift+d".to_string());

        files.save_settings(&settings).unwrap();

        // The store's own read.
        assert_eq!(files.settings().hotkey.as_deref(), Some("cmd+shift+d"));
        // A clone of the store, which is what every Tauri command and the
        // Local Model Manager hold.
        assert_eq!(
            files.clone().settings().hotkey.as_deref(),
            Some("cmd+shift+d")
        );
        // A brand new store over the same directory, which has to agree.
        let cold = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));
        assert_eq!(cold.settings().hotkey.as_deref(), Some("cmd+shift+d"));
        // And the file on disk, in case the cache and the file ever drift.
        assert_eq!(
            load_settings(&config_dir.join("settings.json"))
                .hotkey
                .as_deref(),
            Some("cmd+shift+d")
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    /// The Local Model Manager is the one writer that used to bypass the store.
    /// If it ever does again, its write is invisible to every cached reader and
    /// the model the engine opens disagrees with the model Settings name.
    #[test]
    fn a_model_manager_write_is_visible_to_the_next_read_through_every_door() {
        let (files, config_dir, data_dir) = test_store("manager-door");
        let mut settings = Settings::default();
        settings.store_usage = false;
        files.save_settings(&settings).unwrap();
        let manager = files.model_manager().unwrap();
        let installed = data_dir.join("models").join("ggml-base.en.bin");

        manager
            .record_installed_model(Some(installed.clone()))
            .expect("the manager writes through the store");

        assert_eq!(
            files.settings().model.as_deref(),
            Some(installed.to_string_lossy().as_ref())
        );
        assert_eq!(
            files.clone().settings().model.as_deref(),
            Some(installed.to_string_lossy().as_ref())
        );
        let cold = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));
        assert_eq!(
            cold.settings().model.as_deref(),
            Some(installed.to_string_lossy().as_ref())
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_second_read_costs_no_disk_read() {
        let (files, config_dir, data_dir) = test_store("cached-read");

        files.settings();
        files.settings();
        files.settings();

        assert_eq!(
            files.settings_file_reads(),
            1,
            "the Settings File is read once, not once per call"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_diagnostic_event_does_not_re_read_the_settings_file() {
        let (files, config_dir, data_dir) = test_store("diagnostic-read");
        let mut settings = Settings::default();
        settings.diagnostic_logging = true;
        files.save_settings(&settings).unwrap();
        let reads_after_save = files.settings_file_reads();

        for step in ["one", "two", "three"] {
            files.record_diagnostic_event(DiagnosticEvent::hotkey_transition(
                crate::DictationEvent::Stop,
            ));
            assert!(
                files.settings_file_reads() == reads_after_save,
                "recording {step} re-read the Settings File"
            );
        }
        assert_eq!(
            reads_after_save, 0,
            "the save carried the value, not a read"
        );

        // The log is shared, so all three lines reached the one file.
        let logged = std::fs::read_to_string(config_dir.join("diagnostics.log")).unwrap();
        assert_eq!(logged.lines().count(), 3, "logged: {logged}");

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    /// `SharedDiagnosticLog` says shared, so it has to be shared. The gate is
    /// the observable: a recorder built while logging was off, as the dictation
    /// decorators are, must see the gate a later caller sets. Two separate logs
    /// would each keep their own flag and the held one would stay silent
    /// forever.
    #[test]
    fn one_diagnostic_log_is_shared_by_every_recorder() {
        let (files, config_dir, data_dir) = test_store("shared-log");
        let event = DiagnosticEvent::hotkey_transition(crate::DictationEvent::Stop);
        // A recorder built while the preference is still off.
        let held = files.diagnostic_log(false);

        let mut settings = files.settings();
        settings.diagnostic_logging = true;
        files.save_settings(&settings).unwrap();
        files.record_diagnostic_event(event.clone());
        held.record(event);

        let logged = std::fs::read_to_string(config_dir.join("diagnostics.log")).unwrap();
        assert_eq!(
            logged.lines().count(),
            2,
            "the recorder built before the change did not see the shared gate: {logged}"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_turned_off_preference_records_nothing() {
        let (files, config_dir, data_dir) = test_store("diagnostic-off");

        files.record_diagnostic_event(DiagnosticEvent::hotkey_transition(
            crate::DictationEvent::Stop,
        ));

        assert!(!config_dir.join("diagnostics.log").exists());

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    /// Turning storing off mid-dictation drops the segment that was in flight.
    /// The opt-in is read when the segment is written, not when the writer was
    /// built, so there is no window where a queued segment lands in a file the
    /// user just asked to be deleted.
    #[test]
    fn turning_storing_off_mid_dictation_drops_the_in_flight_segment() {
        let (files, config_dir, data_dir) = test_store("usage-opt-out");
        let mut settings = Settings::default();
        settings.store_usage = true;
        files.save_settings(&settings).unwrap();
        let date = LocalDate::new(2026, 8, 17);

        assert!(files.record_counted_segment(date, counted()));
        assert_eq!(files.usage().days.len(), 1);

        // The user turns storing off. The Usage File is deleted outright.
        let mut off = files.settings();
        crate::apply_usage_settings(&mut off, false);
        files.save_settings(&off).unwrap();
        files.delete_usage_file().unwrap();
        assert!(!files.usage_path().unwrap().exists());

        // The segment that was already in flight when the choice changed.
        assert!(!files.record_counted_segment(date, counted()));
        assert!(
            !files.usage_path().unwrap().exists(),
            "an opt-out segment must not recreate the Usage File"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    /// A failed usage write is a skip, not a dictation failure. The segment has
    /// already been inserted; a Daily Usage Record is never worth failing one.
    #[test]
    fn a_failed_usage_write_is_a_skip_and_not_a_failure() {
        let (files, config_dir, data_dir) = test_store("usage-unwritable");
        let mut settings = Settings::default();
        settings.store_usage = true;
        files.save_settings(&settings).unwrap();
        // A directory where the Usage File belongs: the atomic rename the writer
        // uses cannot land on it, so the write fails and nothing panics.
        std::fs::create_dir_all(files.usage_path().unwrap()).unwrap();

        let recorded = files.record_counted_segment(LocalDate::new(2026, 8, 17), counted());

        assert!(!recorded, "an unwritable Usage File is a skip");

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn usage_round_trips_through_the_store() {
        let (files, config_dir, data_dir) = test_store("usage-round-trip");

        let mut usage = UsageFile::default();
        record_counted_segment(
            &mut usage,
            LocalDate::new(2026, 8, 17),
            CountedSegment {
                words: 12,
                speaking_seconds: 4.0,
                starts_dictation: true,
            },
        );
        files.save_usage(&usage).unwrap();

        assert_eq!(files.usage().days.len(), 1);
        assert_eq!(files.usage().days[0].words, 12);

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }
}
