//! The Local Model (CONTEXT.md, ADR-0018): where its one file lives, and the
//! handle that downloads, installs, and removes it.
//!
//! Two halves, split because they answer different questions:
//!
//! - **Path resolution** — [`LocalModelRef`] is the file a dictation would
//!   actually open, resolved once from the Settings File's own choice and the
//!   managed default. It is pure and answers "is the model ready?" without
//!   touching a byte, so Dictation Readiness and the Whisper engine cannot
//!   answer from two different files.
//! - **Installation** — [`LocalModelManager`] hands out the bytes, and the
//!   rules that decide which bytes are allowed in live in [`download`].
//!
//! The manager writes the Settings File through the [`crate::AppFiles`] store
//! rather than a path of its own, so the store's cached Settings value is the
//! same one the engine and Dictation Readiness read.

pub(crate) mod download;

pub use download::{
    ensure_default_model_with_sha256, throttled_progress, DownloadProgress, HttpModelDownloader,
    ModelDownloader, ModelError,
};

use download::delete_default_model;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL_ID: &str = "base.en";
pub const DEFAULT_MODEL_FILENAME: &str = "ggml-base.en.bin";
pub const DEFAULT_MODEL_DOWNLOAD_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
const DEFAULT_MODEL_SHA256: &str =
    "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub id: String,
    pub filename: String,
    pub path: std::path::PathBuf,
    pub present: bool,
    pub bytes: Option<u64>,
}

pub fn default_model_path(model_dir: &std::path::Path) -> std::path::PathBuf {
    model_dir.join(DEFAULT_MODEL_FILENAME)
}

/// The Local Model file a dictation would actually open, resolved once.
///
/// The Settings File's own choice wins over the managed default, exactly as the
/// engine catalogue resolves it, so Dictation Readiness and Engine Availability
/// cannot answer "is the Local Model ready?" from two different files. They used
/// to: readiness looked only at the default path, so a user with a valid custom
/// model in Settings was told dictation could not start while the engine
/// reported the model available and warm-up was suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModelRef {
    path: std::path::PathBuf,
}

impl LocalModelRef {
    /// The Settings override, then the managed default. `None` when Settings
    /// name nothing and there is no model directory to fall back to.
    pub fn resolve(
        settings: &crate::Settings,
        model_dir: Option<&std::path::Path>,
    ) -> Option<Self> {
        settings
            .model
            .as_ref()
            .map(std::path::PathBuf::from)
            .or_else(|| model_dir.map(default_model_path))
            .map(|path| Self { path })
    }

    /// A reference to one already-named file, for callers that resolved the
    /// path themselves and only want the readiness question answered one way.
    pub fn at(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn is_present(&self) -> bool {
        self.path.exists()
    }
}

fn local_model_status(model_dir: &std::path::Path) -> LocalModelStatus {
    let path = default_model_path(model_dir);
    let bytes = path.metadata().ok().map(|metadata| metadata.len());

    LocalModelStatus {
        id: DEFAULT_MODEL_ID.to_string(),
        filename: DEFAULT_MODEL_FILENAME.to_string(),
        path,
        present: bytes.is_some(),
        bytes,
    }
}

/// Downloads and deletes the managed default Local Model, and points the
/// Settings File at whatever is installed.
///
/// It writes the Settings File through the [`crate::AppFiles`] store rather than
/// a path of its own, so the store's cached Settings value is the same one the
/// engine and Dictation Readiness read.
///
/// Cheap to clone: a model directory and a handle to the file store, so the
/// engine that owns the Local Model can be handed one without being handed the
/// whole app.
#[derive(Clone)]
pub struct LocalModelManager {
    model_dir: std::path::PathBuf,
    files: crate::AppFiles,
}

impl LocalModelManager {
    pub fn new(files: crate::AppFiles) -> Result<Self, ModelError> {
        let model_dir = files.model_dir().map_err(ModelError::Download)?;
        Ok(Self { model_dir, files })
    }

    /// The directory this manager installs the Local Model into. The engine
    /// catalogue asks for it so the engines it builds and the manager that
    /// installs for them can never point at two different directories.
    pub fn model_dir(&self) -> &std::path::Path {
        &self.model_dir
    }

    pub fn status(&self) -> LocalModelStatus {
        local_model_status(&self.model_dir)
    }

    /// Record `model_path` as the Local Model the Settings File names, or clear
    /// it when there is nothing installed. Public so the store's own tests can
    /// drive the bypass that used to exist without a download.
    ///
    /// This runs on a download thread while the user keeps changing Settings, so
    /// it goes through the store's transaction rather than reading a copy and
    /// saving it back: a copy read before a newer choice was saved would put the
    /// older Settings back on disk, model path included.
    pub fn record_installed_model(
        &self,
        model_path: Option<std::path::PathBuf>,
    ) -> Result<(), ModelError> {
        self.files
            .update_settings(|settings| {
                settings.model = model_path.map(|path| path.to_string_lossy().to_string());
                Ok(())
            })
            .map_err(ModelError::Download)?;
        Ok(())
    }

    pub fn download_default(
        &self,
        downloader: &dyn ModelDownloader,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<LocalModelStatus, ModelError> {
        self.download_default_with_sha256(downloader, DEFAULT_MODEL_SHA256, on_progress)
    }

    /// Download the managed default artifact against an explicit trusted
    /// digest. The app uses [`DEFAULT_MODEL_SHA256`]; accepting the digest here
    /// keeps the integrity boundary testable with small deterministic fixtures.
    pub fn download_default_with_sha256(
        &self,
        downloader: &dyn ModelDownloader,
        expected_sha256: &str,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<LocalModelStatus, ModelError> {
        let status = ensure_default_model_with_sha256(
            &self.model_dir,
            downloader,
            expected_sha256,
            on_progress,
        )?;
        self.record_installed_model(status.present.then(|| status.path.clone()))?;
        Ok(status)
    }

    pub fn delete_default(&self) -> Result<LocalModelStatus, ModelError> {
        let status = delete_default_model(&self.model_dir)?;
        self.record_installed_model(None)?;
        Ok(status)
    }

    fn reveal_location(&self) -> RevealLocation {
        reveal_location(&self.model_dir)
    }

    pub fn open_in_file_manager(&self) -> std::io::Result<()> {
        open_in_file_manager(&self.reveal_location())
    }
}

/// Where the "show in file manager" action should point: reveal-and-select the
/// downloaded model when it exists, otherwise open the containing models folder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RevealLocation {
    SelectFile(std::path::PathBuf),
    OpenDir(std::path::PathBuf),
}

fn reveal_location(model_dir: &std::path::Path) -> RevealLocation {
    let file = default_model_path(model_dir);
    if file.exists() {
        RevealLocation::SelectFile(file)
    } else {
        RevealLocation::OpenDir(model_dir.to_path_buf())
    }
}

/// Open the model location in the native file manager (Finder/Explorer). The
/// spawned helper returns immediately, so this never blocks the caller. This
/// module owns the reveal-or-open decision; the OS spawn lives behind the
/// Platform Adapter (ADR-0021).
fn open_in_file_manager(location: &RevealLocation) -> std::io::Result<()> {
    match location {
        RevealLocation::SelectFile(file) => reveal_in_file_manager(file, true),
        RevealLocation::OpenDir(dir) => {
            std::fs::create_dir_all(dir)?;
            reveal_in_file_manager(dir, false)
        }
    }
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    crate::macos::reveal_in_file_manager(path, select)
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    crate::windows::reveal_in_file_manager(path, select)
}

#[cfg(target_os = "linux")]
fn reveal_in_file_manager(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    crate::linux::reveal_in_file_manager(path, select)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn reveal_in_file_manager(_path: &std::path::Path, _select: bool) -> std::io::Result<()> {
    // Other platforms reveal once their Platform Adapter lands (ADR-0021).
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn local_model_status_reports_base_en_path_and_missing_state() {
        let model_dir = unique_test_dir("model-status");
        std::fs::remove_dir_all(&model_dir).ok();

        let status = local_model_status(&model_dir);

        assert_eq!(status.id, "base.en");
        assert_eq!(status.filename, "ggml-base.en.bin");
        assert_eq!(status.path, model_dir.join("ggml-base.en.bin"));
        assert!(!status.present);
        assert_eq!(status.bytes, None);
    }
    #[test]
    fn a_local_model_ref_prefers_the_settings_override_over_the_default() {
        let model_dir = unique_test_dir("ref-resolve");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(default_model_path(&model_dir), b"model").unwrap();
        let custom = model_dir.join("custom.bin");
        std::fs::write(&custom, b"model").unwrap();

        let default = LocalModelRef::resolve(&crate::Settings::default(), Some(&model_dir));
        assert_eq!(
            default.clone().map(|model| model.path().to_path_buf()),
            Some(default_model_path(&model_dir))
        );
        assert!(default.unwrap().is_present());

        let chosen = LocalModelRef::resolve(
            &crate::Settings {
                model: Some(custom.to_string_lossy().to_string()),
                ..crate::Settings::default()
            },
            Some(&model_dir),
        );
        assert_eq!(chosen.map(|model| model.path().to_path_buf()), Some(custom));

        // A Settings File still naming a model the user deleted resolves to that
        // missing file, not to the default: the engine opens it, so readiness
        // has to report the same file missing.
        let stale = LocalModelRef::resolve(
            &crate::Settings {
                model: Some(model_dir.join("deleted.bin").to_string_lossy().to_string()),
                ..crate::Settings::default()
            },
            Some(&model_dir),
        );
        assert!(!stale.unwrap().is_present());

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn model_manager_persists_only_a_verified_model_path() {
        const TRUSTED_SHA256: &str =
            "6d6065cea517391b0166d6a74be33c924cc416b959fa1eee6a146094195b639d";
        let model_dir = unique_test_dir("manager-model");
        let files = crate::AppFiles::from_dirs_for_test(
            Some(model_dir.clone()),
            Some(model_dir.join("data")),
        );
        let manager = LocalModelManager::new(files.clone()).unwrap();
        let downloader = FakeModelDownloader::new(b"trusted model");

        let status = manager
            .download_default_with_sha256(&downloader, TRUSTED_SHA256, &mut |_| {})
            .unwrap();
        let settings = files.settings();

        assert!(status.present);
        assert_eq!(
            settings.model,
            Some(
                default_model_path(&files.model_dir().unwrap())
                    .to_string_lossy()
                    .to_string()
            )
        );

        std::fs::remove_dir_all(model_dir).ok();
    }

    #[test]
    fn reveal_location_selects_existing_model_file() {
        let model_dir = unique_test_dir("reveal-present");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(default_model_path(&model_dir), b"model").unwrap();

        assert_eq!(
            reveal_location(&model_dir),
            RevealLocation::SelectFile(default_model_path(&model_dir))
        );

        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn reveal_location_opens_dir_when_model_missing() {
        let model_dir = unique_test_dir("reveal-missing");
        std::fs::remove_dir_all(&model_dir).ok();

        assert_eq!(
            reveal_location(&model_dir),
            RevealLocation::OpenDir(model_dir.clone())
        );
    }

    #[test]
    fn local_model_manager_deletes_model_and_clears_active_model_path() {
        let model_dir = unique_test_dir("manager-delete");
        let files = crate::AppFiles::from_dirs_for_test(
            Some(model_dir.clone()),
            Some(model_dir.join("data")),
        );
        let installed = default_model_path(&files.model_dir().unwrap());
        std::fs::create_dir_all(files.model_dir().unwrap()).unwrap();
        std::fs::write(&installed, b"local model bytes").unwrap();
        files
            .save_settings(&crate::Settings {
                model: Some(installed.to_string_lossy().to_string()),
                ..crate::Settings::default()
            })
            .unwrap();
        let manager = LocalModelManager::new(files.clone()).unwrap();

        let status = manager.delete_default().expect("manager deletes model");
        let settings = files.settings();

        assert!(!status.present);
        assert_eq!(settings.model, None);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    pub(super) struct FakeModelDownloader {
        bytes: &'static [u8],
        total: Option<u64>,
        pub(super) urls: std::cell::RefCell<Vec<String>>,
    }

    impl FakeModelDownloader {
        pub(super) fn new(bytes: &'static [u8]) -> Self {
            Self {
                bytes,
                total: Some(bytes.len() as u64),
                urls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl ModelDownloader for FakeModelDownloader {
        fn download(
            &self,
            url: &str,
            destination: &std::path::Path,
            on_progress: &mut dyn FnMut(DownloadProgress),
        ) -> Result<(), ModelError> {
            self.urls.borrow_mut().push(url.to_string());
            let total = self.total;
            on_progress(DownloadProgress {
                downloaded: 0,
                total,
            });
            std::fs::write(destination, self.bytes).map_err(ModelError::Io)?;
            on_progress(DownloadProgress {
                downloaded: self.bytes.len() as u64,
                total,
            });
            Ok(())
        }
    }

    /// One directory per test, so two tests running in parallel cannot find each
    /// other's model file.
    pub(super) fn unique_test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
