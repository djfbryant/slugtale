
use crate::{
    apply_and_persist, apply_usage_settings, delete_usage, load_settings, load_usage,
    record_counted_segment, save_settings, save_usage, CountedSegment, DiagnosticEvent,
    FileDiagnosticSink, LocalDate, LocalModelManager, Settings, SharedDiagnosticLog, UsageFile,
};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const SETTINGS_FILE: &str = "settings.json";
const USAGE_FILE: &str = "usage.json";
const DIAGNOSTIC_LOG_FILE: &str = "diagnostics.log";
const MODELS_DIR: &str = "models";

type UsageOwner<'a> = std::sync::MutexGuard<'a, ()>;

#[cfg(test)]
type Probe = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
struct Parked {
    arrived: std::sync::mpsc::Sender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
impl Parked {
    fn park(&self) {
        self.arrived.send(()).ok();
        lock(&self.resume).recv().ok();
    }
}

#[derive(Clone)]
pub struct AppFiles {
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    settings: Arc<Mutex<Option<Settings>>>,
    usage_owner: Arc<Mutex<()>>,
    diagnostic_log: SharedDiagnosticLog<FileDiagnosticSink>,
    #[cfg(test)]
    disk_reads: Arc<AtomicUsize>,
    #[cfg(test)]
    settings_probe: Arc<Mutex<Option<Probe>>>,
    #[cfg(test)]
    usage_probe: Arc<Mutex<Option<Probe>>>,
    #[cfg(test)]
    opt_out_probe: Arc<Mutex<Option<Probe>>>,
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
            usage_owner: Arc::new(Mutex::new(())),
            diagnostic_log: SharedDiagnosticLog::new(false, sink),
            #[cfg(test)]
            disk_reads: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            settings_probe: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            usage_probe: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            opt_out_probe: Arc::new(Mutex::new(None)),
        }
    }

    pub fn settings_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(SETTINGS_FILE))
    }

    pub fn usage_path(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(USAGE_FILE))
    }

    pub fn model_dir(&self) -> Result<PathBuf, String> {
        self.data_dir
            .as_ref()
            .map(|dir| dir.join(MODELS_DIR))
            .ok_or_else(|| "could not resolve model directory".to_string())
    }

    pub fn model_manager(&self) -> Result<LocalModelManager, String> {
        LocalModelManager::new(self.clone()).map_err(|error| error.to_string())
    }

    /// This is a read. A caller that intends to change something goes through
    /// [`Self::update_settings`] or [`Self::update_settings_and_apply`] instead,
    /// because the clone this returns is a snapshot: by the time the caller saves
    /// it, someone else may have written a newer choice.
    pub fn settings(&self) -> Settings {
        let mut cached = lock(&self.settings);
        if cached.is_none() {
            *cached = Some(self.read_settings());
        }
        cached.clone().expect("just filled")
    }

    /// A `change` that returns `Err` writes nothing and leaves the cache holding
    /// what the file still says, so a refused change is not a partial one.
    pub fn update_settings(
        &self,
        change: impl FnOnce(&mut Settings) -> Result<(), String>,
    ) -> Result<Settings, String> {
        let mut cached = lock(&self.settings);
        if cached.is_none() {
            *cached = Some(self.read_settings());
        }
        let mut settings = cached.clone().expect("just filled");

        #[cfg(test)]
        Self::fire(&self.settings_probe);
        change(&mut settings)?;

        self.write_settings_file(&settings)?;
        *cached = Some(settings.clone());
        Ok(settings)
    }

    /// The outside change happens inside the transaction, so the world and the
    /// file cannot be seen disagreeing, and a failed save rolls the outside world
    /// back onto the value that was actually in force.
    pub fn update_settings_and_apply(
        &self,
        apply: impl FnOnce(&mut Settings),
        side_effect: impl Fn(&Settings) -> Result<(), String>,
    ) -> Result<Settings, String> {
        let mut cached = lock(&self.settings);
        if cached.is_none() {
            *cached = Some(self.read_settings());
        }
        let current = cached.clone().expect("just filled");

        let settings = apply_and_persist(
            &current,
            apply,
            side_effect,
            |settings| self.write_settings_file(settings),
        )?;
        *cached = Some(settings.clone());
        Ok(settings)
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut cached = lock(&self.settings);
        self.write_settings_file(settings)?;
        *cached = Some(settings.clone());
        Ok(())
    }

    fn write_settings_file(&self, settings: &Settings) -> Result<(), String> {
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

    pub fn save_usage(&self, usage: &UsageFile) -> Result<(), String> {
        let path = self
            .usage_path()
            .ok_or_else(|| "could not resolve usage path".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        save_usage(&path, usage).map_err(|error| error.to_string())
    }

    /// asked to be deleted and does not bring it back. And a write that fails is
    /// a skip, not an error: the dictation it counted has already been inserted,
    /// and a Daily Usage Record is never worth a failed one.
    pub fn record_counted_segment(&self, date: LocalDate, segment: CountedSegment) -> bool {
        let owner = self.usage_owner();
        if !self.store_usage_while_owning(&owner) || self.usage_path().is_none() {
            return false;
        }
        let mut usage = self.usage();
        #[cfg(test)]
        Self::fire(&self.usage_probe);
        record_counted_segment(&mut usage, date, segment);
        match self.save_usage(&usage) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("could not write the usage file: {error}");
                false
            }
        }
    }

    /// and is then removed rather than left behind. Once this returns, no racing
    /// write can put the Usage File back.
    pub fn set_usage_storing(&self, enabled: bool) -> Result<Settings, String> {
        let owner = self.usage_owner();
        #[cfg(test)]
        Self::fire(&self.opt_out_probe);

        let settings = self.update_settings(|settings| {
            apply_usage_settings(settings, enabled);
            Ok(())
        })?;
        if !enabled {
            self.delete_usage_file_while_owning(&owner)?;
        }
        Ok(settings)
    }

    fn usage_owner(&self) -> UsageOwner<'_> {
        lock(&self.usage_owner)
    }

    fn store_usage_while_owning(&self, _owner: &UsageOwner<'_>) -> bool {
        self.settings().store_usage
    }

    pub fn diagnostic_log(&self, enabled: bool) -> SharedDiagnosticLog<FileDiagnosticSink> {
        self.diagnostic_log.set_enabled(enabled);
        self.diagnostic_log.clone()
    }

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

    pub fn delete_usage_file(&self) -> Result<(), String> {
        let owner = self.usage_owner();
        self.delete_usage_file_while_owning(&owner)
    }

    fn delete_usage_file_while_owning(&self, _owner: &UsageOwner<'_>) -> Result<(), String> {
        let Some(path) = self.usage_path() else {
            return Ok(());
        };
        delete_usage(&path).map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn install_settings_probe(&self, probe: Probe) {
        *lock(&self.settings_probe) = Some(probe);
    }

    #[cfg(test)]
    fn install_usage_probe(&self, probe: Probe) {
        *lock(&self.usage_probe) = Some(probe);
    }

    #[cfg(test)]
    fn install_opt_out_probe(&self, probe: Probe) {
        *lock(&self.opt_out_probe) = Some(probe);
    }

    #[cfg(test)]
    fn fire(probe: &Mutex<Option<Probe>>) {
        let installed = lock(probe).take();
        if let Some(probe) = installed {
            probe();
        }
    }

    #[cfg(test)]
    fn settings_file_reads(&self) -> usize {
        self.disk_reads.load(Ordering::Relaxed)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

    fn parkable() -> (Probe, ParkHandle) {
        let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let parked = Arc::new(Parked {
            arrived: arrived_tx,
            resume: Mutex::new(resume_rx),
        });
        let probe: Probe = Arc::new(move || parked.park());
        (
            probe,
            ParkHandle {
                arrived: arrived_rx,
                resume: resume_tx,
            },
        )
    }

    struct ParkHandle {
        arrived: std::sync::mpsc::Receiver<()>,
        resume: std::sync::mpsc::Sender<()>,
    }

    impl ParkHandle {
        fn wait_parked(&self) {
            self.arrived.recv().expect("the parked thread reported in");
        }

        fn release(&self) {
            self.resume.send(()).ok();
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

        assert_eq!(files.settings().hotkey.as_deref(), Some("cmd+shift+d"));
        assert_eq!(
            files.clone().settings().hotkey.as_deref(),
            Some("cmd+shift+d")
        );
        let cold = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));
        assert_eq!(cold.settings().hotkey.as_deref(), Some("cmd+shift+d"));
        assert_eq!(
            load_settings(&config_dir.join("settings.json"))
                .hotkey
                .as_deref(),
            Some("cmd+shift+d")
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

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

        let logged = std::fs::read_to_string(config_dir.join("diagnostics.log")).unwrap();
        assert_eq!(logged.lines().count(), 3, "logged: {logged}");

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn one_diagnostic_log_is_shared_by_every_recorder() {
        let (files, config_dir, data_dir) = test_store("shared-log");
        let event = DiagnosticEvent::hotkey_transition(crate::DictationEvent::Stop);
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

    #[test]
    fn turning_storing_off_mid_dictation_drops_the_in_flight_segment() {
        let (files, config_dir, data_dir) = test_store("usage-opt-out");
        let mut settings = Settings::default();
        settings.store_usage = true;
        files.save_settings(&settings).unwrap();
        let date = LocalDate::new(2026, 8, 17);

        assert!(files.record_counted_segment(date, counted()));
        assert_eq!(files.usage().days.len(), 1);

        files.set_usage_storing(false).unwrap();
        assert!(!files.settings().store_usage);
        assert!(!files.usage_path().unwrap().exists());

        assert!(!files.record_counted_segment(date, counted()));
        assert!(
            !files.usage_path().unwrap().exists(),
            "an opt-out segment must not recreate the Usage File"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn an_opt_out_that_lands_after_an_in_flight_segment_leaves_no_usage_file() {
        let (files, config_dir, data_dir) = test_store("usage-opt-out-after-write");
        let mut on = Settings::default();
        on.store_usage = true;
        files.save_settings(&on).unwrap();

        let (probe, park) = parkable();
        files.install_usage_probe(probe);

        let writer = files.clone();
        let recording = std::thread::spawn(move || {
            writer.record_counted_segment(LocalDate::new(2026, 8, 17), counted())
        });
        park.wait_parked();

        let turning_off = files.clone();
        let opted_out = std::thread::spawn(move || turning_off.set_usage_storing(false));

        park.release();
        assert!(
            recording.join().unwrap(),
            "the in-flight segment should still be written before the opt-out"
        );
        opted_out.join().unwrap().expect("the opt-out succeeds");

        assert!(!files.settings().store_usage);
        assert!(
            !files.usage_path().unwrap().exists(),
            "the opt-out must be the last writer of the Usage File"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_counted_segment_racing_an_opt_out_never_recreates_the_usage_file() {
        let (files, config_dir, data_dir) = test_store("usage-opt-out-before-write");
        let mut on = Settings::default();
        on.store_usage = true;
        files.save_settings(&on).unwrap();

        let (probe, park) = parkable();
        files.install_opt_out_probe(probe);

        let turning_off = files.clone();
        let opted_out = std::thread::spawn(move || turning_off.set_usage_storing(false));
        park.wait_parked();

        let writer = files.clone();
        let recording = std::thread::spawn(move || {
            writer.record_counted_segment(LocalDate::new(2026, 8, 17), counted())
        });

        park.release();
        opted_out.join().unwrap().expect("the opt-out succeeds");
        assert!(
            !recording.join().unwrap(),
            "a segment that reaches the store after the opt-out must be skipped"
        );

        assert!(
            !files.usage_path().unwrap().exists(),
            "the Usage File must not come back after the opt-out returned"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn two_segments_recorded_at_once_both_land() {
        let (files, config_dir, data_dir) = test_store("usage-concurrent-counts");
        let mut on = Settings::default();
        on.store_usage = true;
        files.save_settings(&on).unwrap();

        let (probe, park) = parkable();
        files.install_usage_probe(probe);

        let first = files.clone();
        let one = std::thread::spawn(move || {
            first.record_counted_segment(
                LocalDate::new(2026, 8, 17),
                CountedSegment {
                    words: 10,
                    speaking_seconds: 3.0,
                    starts_dictation: true,
                },
            )
        });
        park.wait_parked();

        let second = files.clone();
        let two = std::thread::spawn(move || {
            second.record_counted_segment(
                LocalDate::new(2026, 8, 18),
                CountedSegment {
                    words: 7,
                    speaking_seconds: 2.0,
                    starts_dictation: false,
                },
            )
        });

        park.release();
        assert!(one.join().unwrap());
        assert!(two.join().unwrap());

        let usage = files.usage();
        assert_eq!(
            usage.days.len(),
            2,
            "both days must survive: {:?}",
            usage.days
        );
        assert_eq!(crate::totals_all_time(&usage).words, 17);

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_model_install_racing_a_settings_choice_cannot_overwrite_the_newer_choice() {
        let (files, config_dir, data_dir) = test_store("stale-model-install");
        let mut initial = Settings::default();
        initial.store_usage = false;
        files.save_settings(&initial).unwrap();

        let (probe, park) = parkable();
        files.install_settings_probe(probe);

        let choosing = files.clone();
        let choice = std::thread::spawn(move || {
            choosing.update_settings(|settings| {
                crate::apply_microphone_settings(settings, true);
                Ok(())
            })
        });
        park.wait_parked();

        let installing = files.clone();
        let install = std::thread::spawn(move || {
            let installed = installing.model_dir().unwrap().join("ggml-base.en.bin");
            installing
                .model_manager()
                .unwrap()
                .record_installed_model(Some(installed))
                .unwrap();
        });

        park.release();
        choice.join().unwrap().unwrap();
        install.join().unwrap();

        let settings = files.settings();
        assert!(
            settings.prefer_built_in_microphone,
            "the model install overwrote a newer user choice"
        );
        assert!(
            settings.model.is_some(),
            "the installed model path should still be recorded"
        );

        let cold = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));
        assert!(cold.settings().prefer_built_in_microphone);

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_settings_snapshot_does_not_follow_later_changes() {
        let (files, config_dir, data_dir) = test_store("settings-snapshot");
        let snapshot = files.settings();

        files
            .update_settings(|settings| {
                settings.hotkey = Some("cmd+shift+d".to_string());
                Ok(())
            })
            .unwrap();

        assert_eq!(snapshot.hotkey, None, "a snapshot is a copy taken at one moment");
        assert_eq!(files.settings().hotkey.as_deref(), Some("cmd+shift+d"));

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn saving_a_stale_snapshot_replaces_a_newer_change_because_it_skips_the_settings_transaction() {
        let (files, config_dir, data_dir) = test_store("stale-snapshot-save");
        let stale = files.settings();
        files
            .update_settings(|settings| {
                settings.hotkey = Some("cmd+shift+d".to_string());
                Ok(())
            })
            .unwrap();

        files.save_settings(&stale).unwrap();

        assert_eq!(files.settings().hotkey, None);
        assert_eq!(load_settings(&config_dir.join("settings.json")).hotkey, None);

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_refused_change_writes_nothing_and_leaves_the_cache_alone() {
        let (files, config_dir, data_dir) = test_store("refused-change");
        let mut on = Settings::default();
        on.store_usage = true;
        files.save_settings(&on).unwrap();

        let refused = files.update_settings(|settings| {
            settings.hotkey = Some("cmd+shift+d".to_string());
            Err("no".to_string())
        });

        assert!(refused.is_err());
        assert_eq!(files.settings().hotkey, None);
        assert_eq!(
            load_settings(&config_dir.join("settings.json")).hotkey,
            None,
            "a refused change must not reach the file either"
        );

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn a_failed_usage_write_is_a_skip_and_not_a_failure() {
        let (files, config_dir, data_dir) = test_store("usage-unwritable");
        let mut settings = Settings::default();
        settings.store_usage = true;
        files.save_settings(&settings).unwrap();
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
