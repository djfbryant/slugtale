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

/// A test-only hook that fires at one chosen point inside a transaction.
///
/// The faults this store fixes are only observable when two threads are
/// genuinely inside a transaction at the same time, and a test cannot arrange
/// that against the very locks that are supposed to make it impossible. A probe
/// lets a test park one operation inside its owner, attempt a second, and assert
/// on what the second one saw — a real interleaving rather than a timing race.
#[cfg(test)]
type Probe = Arc<dyn Fn() + Send + Sync>;

/// The rendezvous halves a probe needs. Kept as one type so a probe stays
/// `Sync`: it is shared through `Arc`, so a receiver inside it would make the
/// whole store unshareable between threads.
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
    /// Config-dir files are the user-facing ones (ADR-0018): the Settings File
    /// and the Usage File sit side by side so opting out deletes one obvious
    /// file and nothing else.
    config_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    /// The Settings File, read once and handed out as clones. Every clone of the
    /// store shares this, so a write through one door is the next read through
    /// every other.
    settings: Arc<Mutex<Option<Settings>>>,
    /// The single owner of Usage File state. A counted segment updates the file
    /// and turning storing off deletes it; both are mutations of one path, so
    /// both take this lock instead of each deciding on its own. Whichever
    /// arrives first finishes first, and the second sees the result.
    usage_owner: Arc<Mutex<()>>,
    /// One Local Diagnostic Log for the app's life. The recorders built from it
    /// are cheap clones of one shared log rather than one log per caller.
    diagnostic_log: SharedDiagnosticLog<FileDiagnosticSink>,
    /// How many times the Settings File has actually been read. Tests use it to
    /// show that a diagnostic event or a second read costs no I/O.
    #[cfg(test)]
    disk_reads: Arc<AtomicUsize>,
    #[cfg(test)]
    settings_probe: Arc<Mutex<Option<Probe>>>,
    /// Fires inside a counted segment. Separate from `opt_out_probe` so a test
    /// that wants to park one specific operation can: a probe taken from a shared
    /// slot goes to whichever thread arrives first, which is not necessarily the
    /// one the test is about.
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
            // Enabled per ask, from the current preference. The log is created
            // once, so a later change has to reach it through `set_enabled`.
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
    ///
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

    /// The one transaction every mutating caller uses: take the current Settings
    /// — from the cache, or from the file on the first call — let the caller
    /// change them, write the Settings File, then adopt the new value into the
    /// cache. All four steps happen under one lock.
    ///
    /// Holding the lock across the whole change is the point. Reading a copy,
    /// editing it and saving it back leaves a window in which a background model
    /// install writes a newer choice and this caller then puts its older
    /// snapshot over it on disk and in the cache. No such window exists here,
    /// because no other mutator can be between the read and the write.
    ///
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
        // Adopt only once the file is written: a failed save must leave the store
        // holding what the file says, which is the old value.
        *cached = Some(settings.clone());
        Ok(settings)
    }

    /// [`Self::update_settings`] for a setting whose change has an effect outside
    /// the Settings File — a registered hotkey, the OS login item, the Voice
    /// Activation worker.
    ///
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

    /// Replace the Settings File wholesale.
    ///
    /// Settings commands use [`Self::update_settings`] instead, so none of them
    /// can save a snapshot they read before another writer changed it. This door
    /// stays for a caller that genuinely holds a complete value rather than a
    /// change, and it takes the same lock and adopts the cache only after the
    /// file is written, so it cannot leave disk and cache disagreeing either.
    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let mut cached = lock(&self.settings);
        self.write_settings_file(settings)?;
        *cached = Some(settings.clone());
        Ok(())
    }

    /// The disk half of every settings write, with no cache and no lock of its
    /// own. The transactions above call it while they already hold the settings
    /// lock and adopt the value themselves once it succeeds.
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

    /// Take one Counted Segment toward Usage (ADR-0025), and say whether it was
    /// written.
    ///
    /// Three decisions live here rather than in the caller. The Usage owner is
    /// taken first, so this read-modify-write cannot interleave with the opt-out
    /// that deletes the file, nor with another segment writing the same day —
    /// both of which used to be able to lose the other's work. The opt-in is read
    /// inside the owner, at the last possible moment, so a segment already in
    /// flight when the user turned storing off does not land in a file they just
    /// asked to be deleted and does not bring it back. And a write that fails is
    /// a skip, not an error: the dictation it counted has already been inserted,
    /// and a Daily Usage Record is never worth a failed one.
    pub fn record_counted_segment(&self, date: LocalDate, segment: CountedSegment) -> bool {
        let _owner = self.usage_owner();
        if !self.settings().store_usage || self.usage_path().is_none() {
            return false;
        }
        let mut usage = self.usage();
        // Past the opt-in now: this is the in-flight moment a racing opt-out has to
        // account for, so it is where a test can park the writer.
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

    /// Record the Daily Usage Record opt-in and, when the user turns storing off,
    /// delete the Usage File — as one transaction against the Usage owner.
    ///
    /// The choice is saved before the delete, and both happen before the owner is
    /// released. So a counted segment arriving after this call finds storing off
    /// and skips, and a counted segment already inside the owner finishes first
    /// and is then removed rather than left behind. Once this returns, no racing
    /// write can put the Usage File back.
    pub fn set_usage_storing(&self, enabled: bool) -> Result<Settings, String> {
        let _owner = self.usage_owner();
        #[cfg(test)]
        Self::fire(&self.opt_out_probe);

        let settings = self.update_settings(|settings| {
            apply_usage_settings(settings, enabled);
            Ok(())
        })?;
        if !enabled {
            self.delete_usage_file_while_owning()?;
        }
        Ok(settings)
    }

    /// Hold the single Usage owner for as long as the guard lives. Every mutation
    /// of the Usage File takes it, so the opt-out and a counted segment are never
    /// in the file at the same time.
    fn usage_owner(&self) -> std::sync::MutexGuard<'_, ()> {
        lock(&self.usage_owner)
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

    /// Delete the Usage File outright when the user turns storing off, so
    /// "stop storing this" means the stored thing is gone rather than left to
    /// rot unread. The Typing Baseline is in the Settings File and is untouched.
    ///
    /// Takes the Usage owner, so a counted segment cannot be writing the file
    /// while it is deleted. Turning the preference off is
    /// [`Self::set_usage_storing`], which does the save and the delete together;
    /// this door stays for a caller that only wants the file gone.
    pub fn delete_usage_file(&self) -> Result<(), String> {
        let _owner = self.usage_owner();
        self.delete_usage_file_while_owning()
    }

    /// The delete half of [`Self::delete_usage_file`], for a caller already
    /// holding the Usage owner — which cannot be re-taken, because the lock is
    /// not recursive.
    fn delete_usage_file_while_owning(&self) -> Result<(), String> {
        let Some(path) = self.usage_path() else {
            return Ok(());
        };
        delete_usage(&path).map_err(|error| error.to_string())
    }

    /// Install a probe that fires once, inside the next settings transaction,
    /// between the read and the write. A parked probe holds the settings lock, so
    /// a second writer the test starts is genuinely blocked behind it.
    #[cfg(test)]
    fn install_settings_probe(&self, probe: Probe) {
        *lock(&self.settings_probe) = Some(probe);
    }

    /// Install a probe that fires once, inside the next counted segment. See
    /// [`Self::install_settings_probe`].
    #[cfg(test)]
    fn install_usage_probe(&self, probe: Probe) {
        *lock(&self.usage_probe) = Some(probe);
    }

    /// Install a probe that fires once, inside the next opt-out. Its own slot, so
    /// parking one operation cannot park the other.
    #[cfg(test)]
    fn install_opt_out_probe(&self, probe: Probe) {
        *lock(&self.opt_out_probe) = Some(probe);
    }

    /// Run a probe if one is installed, once: the lock is released before the
    /// probe is called, and the probe is taken out of its slot, so parking this
    /// thread cannot block the probe slot or park a later transaction too.
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

/// Lock one of the store's shared mutexes, treating a poisoned lock as still
/// usable. A panic in one Settings command must not turn every later one into a
/// failure, and a transaction cannot leave the store half-written anyway: the
/// cache is adopted only after the file is written, and a `change` that panics
/// never reaches that line.
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

    /// A rendezvous that lets a test park one operation inside a transaction.
    ///
    /// The returned probe reports reaching the point and then blocks. The handle
    /// has to be driven in two steps — wait for the report, then release — because
    /// a test that only releases would spawn the other thread while neither side
    /// is parked yet, and then which of them wins the owner would be a matter of
    /// scheduling rather than of the test.
    fn parkable() -> (Probe, ParkHandle) {
        let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        // The receiver lives behind a mutex so the closure stays `Sync`; only the
        // probe's own thread ever parks, so contention is not a concern.
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
        /// Block until the probed operation has reached the point and is parked.
        fn wait_parked(&self) {
            self.arrived.recv().expect("the parked thread reported in");
        }

        /// Let the parked operation continue.
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

        // The user turns storing off, which saves the choice and deletes the file
        // together.
        files.set_usage_storing(false).unwrap();
        assert!(!files.settings().store_usage);
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

    /// The interleaving that used to return the Usage File: a counted segment
    /// checks the opt-in, the user opts out and the file is deleted, and the
    /// writer then creates the file again from a snapshot taken before the
    /// opt-out. The store and the command that owns it now share one owner, so
    /// the writer either finishes before the delete or sees storing off.
    ///
    /// The probe parks the writer past its opt-in check and still holding the
    /// owner, so the opt-out on the other thread is genuinely blocked behind it
    /// rather than merely likely to be. The two probes have separate slots so
    /// each test parks the operation it is actually reasoning about.
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

        // Started while the writer holds the owner: it cannot finish until the
        // writer has.
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

    /// The other order. Here the opt-out is parked inside the owner after the
    /// choice is saved but before the file is deleted, and a counted segment
    /// attempts to write throughout. Once the opt-out returns, the segment must
    /// not put the file back: it has to see the choice that was just saved.
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

        // This blocks on the owner until the opt-out has deleted the file.
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

    /// The counts themselves are the other half of the requirement. Two segments
    /// recorded at once used to read the file, add to their own copy and write it
    /// back, so whichever wrote last erased the other's words.
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

    /// The stale-snapshot half of the finding: a background model install used to
    /// read Settings, change one field and save the whole value back, so a user
    /// choice saved after that read was silently overwritten with the copy taken
    /// before it.
    #[test]
    fn a_model_install_racing_a_settings_choice_cannot_overwrite_the_newer_choice() {
        let (files, config_dir, data_dir) = test_store("stale-model-install");
        let mut initial = Settings::default();
        initial.store_usage = false;
        files.save_settings(&initial).unwrap();

        // Park the user's choice mid-transaction: read done, write not yet.
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

        // The install is started while the choice holds the transaction.
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

        // And the file agrees with the cache, on a cold read.
        let cold = AppFiles::from_dirs(Some(config_dir.clone()), Some(data_dir.clone()));
        assert!(cold.settings().prefer_built_in_microphone);

        std::fs::remove_dir_all(config_dir).ok();
        std::fs::remove_dir_all(data_dir).ok();
    }

    /// A refused change must not half-apply: the store keeps what the file says.
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
