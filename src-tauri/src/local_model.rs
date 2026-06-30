use serde::{Deserialize, Serialize};

pub const DEFAULT_MODEL_ID: &str = "base.en";
pub const DEFAULT_MODEL_FILENAME: &str = "ggml-base.en.bin";
pub const DEFAULT_MODEL_DOWNLOAD_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";

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

pub fn local_model_status(model_dir: &std::path::Path) -> LocalModelStatus {
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

pub fn local_model_ready(model_dir: &std::path::Path) -> bool {
    local_model_status(model_dir).present
}

#[derive(Debug)]
pub enum ModelError {
    Io(std::io::Error),
    Download(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "model file error: {error}"),
            Self::Download(message) => write!(f, "model download error: {message}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<std::io::Error> for ModelError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct LocalModelManager {
    model_dir: std::path::PathBuf,
    settings_path: std::path::PathBuf,
}

impl LocalModelManager {
    pub fn new(model_dir: std::path::PathBuf, settings_path: std::path::PathBuf) -> Self {
        Self {
            model_dir,
            settings_path,
        }
    }

    pub fn status(&self) -> LocalModelStatus {
        local_model_status(&self.model_dir)
    }

    pub fn ready(&self) -> bool {
        self.status().present
    }

    pub fn active_model_path(&self, settings: &crate::Settings) -> std::path::PathBuf {
        settings
            .model
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| default_model_path(&self.model_dir))
    }

    pub fn download_default(
        &self,
        downloader: &dyn ModelDownloader,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<LocalModelStatus, ModelError> {
        let status = ensure_default_model(&self.model_dir, downloader, on_progress)?;
        self.persist_active_model(status.present.then(|| status.path.clone()))?;
        Ok(status)
    }

    pub fn delete_default(&self) -> Result<LocalModelStatus, ModelError> {
        let status = delete_default_model(&self.model_dir)?;
        self.persist_active_model(None)?;
        Ok(status)
    }

    pub fn reveal_location(&self) -> RevealLocation {
        reveal_location(&self.model_dir)
    }

    pub fn open_in_file_manager(&self) -> std::io::Result<()> {
        open_in_file_manager(&self.reveal_location())
    }

    fn persist_active_model(
        &self,
        model_path: Option<std::path::PathBuf>,
    ) -> Result<(), ModelError> {
        let mut settings = crate::load_settings(&self.settings_path);
        settings.model = model_path.map(|path| path.to_string_lossy().to_string());
        crate::save_settings(&self.settings_path, &settings)?;
        Ok(())
    }
}

/// Progress reported while streaming a model download. `total` is `None` when
/// the server does not advertise a Content-Length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

pub trait ModelDownloader {
    fn download(
        &self,
        url: &str,
        destination: &std::path::Path,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<(), ModelError>;
}

pub struct HttpModelDownloader;

impl ModelDownloader for HttpModelDownloader {
    fn download(
        &self,
        url: &str,
        destination: &std::path::Path,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<(), ModelError> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|error| ModelError::Download(error.to_string()))?;
        let total = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut reader = response.body_mut().as_reader();
        let mut file = std::fs::File::create(destination)?;
        copy_with_progress(&mut reader, &mut file, total, on_progress)?;
        Ok(())
    }
}

/// Stream `reader` into `writer`, reporting cumulative bytes after each chunk so
/// callers can surface download progress. Emits an initial zero-byte update so
/// the UI can show an active state before the first chunk arrives.
fn copy_with_progress(
    reader: &mut dyn std::io::Read,
    writer: &mut dyn std::io::Write,
    total: Option<u64>,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> std::io::Result<u64> {
    let mut buffer = [0u8; 64 * 1024];
    let mut downloaded = 0u64;
    on_progress(DownloadProgress { downloaded, total });

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        downloaded += read as u64;
        on_progress(DownloadProgress { downloaded, total });
    }

    Ok(downloaded)
}

pub fn ensure_default_model(
    model_dir: &std::path::Path,
    downloader: &dyn ModelDownloader,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<LocalModelStatus, ModelError> {
    let current = local_model_status(model_dir);
    if current.present {
        return Ok(current);
    }

    std::fs::create_dir_all(model_dir)?;
    let staged_path = model_dir.join(format!("{DEFAULT_MODEL_FILENAME}.download"));
    std::fs::remove_file(&staged_path).ok();
    let mut expected_bytes = None;
    downloader.download(DEFAULT_MODEL_DOWNLOAD_URL, &staged_path, &mut |progress| {
        if progress.total.is_some() {
            expected_bytes = progress.total;
        }
        on_progress(progress);
    })?;

    let downloaded_bytes = staged_path.metadata()?.len();
    if downloaded_bytes == 0 {
        std::fs::remove_file(&staged_path).ok();
        return Err(ModelError::Download(
            "downloaded model was empty".to_string(),
        ));
    }
    if let Some(expected_bytes) = expected_bytes {
        if downloaded_bytes != expected_bytes {
            std::fs::remove_file(&staged_path).ok();
            return Err(ModelError::Download(format!(
                "downloaded model was incomplete: expected {expected_bytes} bytes, got {downloaded_bytes}"
            )));
        }
    }

    std::fs::rename(staged_path, default_model_path(model_dir))?;
    Ok(local_model_status(model_dir))
}

pub fn delete_default_model(model_dir: &std::path::Path) -> Result<LocalModelStatus, ModelError> {
    let path = default_model_path(model_dir);
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ModelError::Io(error)),
    }

    Ok(local_model_status(model_dir))
}

/// Where the "show in file manager" action should point: reveal-and-select the
/// downloaded model when it exists, otherwise open the containing models folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevealLocation {
    SelectFile(std::path::PathBuf),
    OpenDir(std::path::PathBuf),
}

pub fn reveal_location(model_dir: &std::path::Path) -> RevealLocation {
    let file = default_model_path(model_dir);
    if file.exists() {
        RevealLocation::SelectFile(file)
    } else {
        RevealLocation::OpenDir(model_dir.to_path_buf())
    }
}

/// Open the model location in the native file manager (Finder/Explorer). The
/// spawned helper returns immediately, so this never blocks the caller.
pub fn open_in_file_manager(location: &RevealLocation) -> std::io::Result<()> {
    match location {
        RevealLocation::SelectFile(file) => open_path(file, true),
        RevealLocation::OpenDir(dir) => {
            std::fs::create_dir_all(dir)?;
            open_path(dir, false)
        }
    }
}

#[cfg(target_os = "macos")]
fn open_path(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("open");
    if select {
        command.arg("-R");
    }
    command.arg(path).spawn()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_path(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    let mut command = std::process::Command::new("explorer");
    if select {
        command.arg(format!("/select,{}", path.display()));
    } else {
        command.arg(path);
    }
    command.spawn()?;
    Ok(())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn open_path(path: &std::path::Path, _select: bool) -> std::io::Result<()> {
    let target = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    std::process::Command::new("xdg-open").arg(target).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
    fn local_model_ready_reports_default_model_presence() {
        let model_dir = unique_test_dir("model-ready");
        std::fs::create_dir_all(&model_dir).unwrap();
        assert!(!local_model_ready(&model_dir));

        std::fs::write(default_model_path(&model_dir), b"model").unwrap();

        assert!(local_model_ready(&model_dir));
        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn ensure_default_model_downloads_missing_base_en_model() {
        let model_dir = unique_test_dir("model-download");
        std::fs::remove_dir_all(&model_dir).ok();
        let downloader = FakeModelDownloader::new(b"local model bytes");

        let mut progress = Vec::new();
        let status =
            ensure_default_model(&model_dir, &downloader, &mut |update| progress.push(update))
                .unwrap();

        assert_eq!(
            downloader.urls.borrow().as_slice(),
            &[DEFAULT_MODEL_DOWNLOAD_URL]
        );
        assert!(status.present);
        assert_eq!(status.bytes, Some(17));
        assert_eq!(
            std::fs::read(default_model_path(&model_dir)).unwrap(),
            b"local model bytes"
        );
        assert_eq!(
            progress.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total: Some(17)
            })
        );
        assert_eq!(
            progress.last().copied(),
            Some(DownloadProgress {
                downloaded: 17,
                total: Some(17)
            })
        );

        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn ensure_default_model_rejects_incomplete_downloads() {
        let model_dir = unique_test_dir("model-download-short");
        std::fs::remove_dir_all(&model_dir).ok();
        let downloader = FakeModelDownloader::new(b"partial model").with_total(100);

        let error = ensure_default_model(&model_dir, &downloader, &mut |_| {}).unwrap_err();

        assert_eq!(
            error.to_string(),
            "model download error: downloaded model was incomplete: expected 100 bytes, got 13"
        );
        assert!(!default_model_path(&model_dir).exists());
        assert!(!model_dir.join("ggml-base.en.bin.download").exists());

        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn copy_with_progress_reports_zero_then_each_chunk() {
        let data = vec![7u8; 200_000];
        let mut reader = std::io::Cursor::new(data.clone());
        let mut writer: Vec<u8> = Vec::new();
        let total = Some(data.len() as u64);
        let mut updates = Vec::new();

        let copied = copy_with_progress(&mut reader, &mut writer, total, &mut |update| {
            updates.push(update)
        })
        .unwrap();

        assert_eq!(copied, data.len() as u64);
        assert_eq!(writer, data);
        assert_eq!(
            updates.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total
            })
        );
        assert_eq!(
            updates.last().copied(),
            Some(DownloadProgress {
                downloaded: data.len() as u64,
                total
            })
        );
        // 200_000 bytes over 64 KiB chunks reports the initial update plus one
        // per chunk, so the bar advances rather than jumping straight to done.
        assert!(updates.len() >= 4);
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
    fn delete_default_model_removes_downloaded_model() {
        let model_dir = unique_test_dir("model-delete");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(default_model_path(&model_dir), b"local model bytes").unwrap();

        let status = delete_default_model(&model_dir).unwrap();

        assert!(!status.present);
        assert_eq!(status.bytes, None);
        assert!(!default_model_path(&model_dir).exists());

        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn local_model_manager_downloads_model_and_persists_active_model_path() {
        let model_dir = unique_test_dir("manager-download");
        let settings_path = model_dir.join("settings.json");
        std::fs::remove_dir_all(&model_dir).ok();
        let manager = LocalModelManager::new(model_dir.clone(), settings_path.clone());
        let downloader = FakeModelDownloader::new(b"local model bytes");

        let status = manager
            .download_default(&downloader, &mut |_| {})
            .expect("manager downloads model");
        let settings = crate::load_settings(&settings_path);

        assert!(status.present);
        assert_eq!(
            settings.model,
            Some(default_model_path(&model_dir).to_string_lossy().to_string())
        );

        std::fs::remove_dir_all(&model_dir).ok();
    }
    #[test]
    fn local_model_manager_deletes_model_and_clears_active_model_path() {
        let model_dir = unique_test_dir("manager-delete");
        let settings_path = model_dir.join("settings.json");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(default_model_path(&model_dir), b"local model bytes").unwrap();
        crate::save_settings(
            &settings_path,
            &crate::Settings {
                model: Some(default_model_path(&model_dir).to_string_lossy().to_string()),
                ..crate::Settings::default()
            },
        )
        .unwrap();
        let manager = LocalModelManager::new(model_dir.clone(), settings_path.clone());

        let status = manager.delete_default().expect("manager deletes model");
        let settings = crate::load_settings(&settings_path);

        assert!(!status.present);
        assert_eq!(settings.model, None);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    struct FakeModelDownloader {
        bytes: &'static [u8],
        total: Option<u64>,
        urls: std::cell::RefCell<Vec<String>>,
    }

    impl FakeModelDownloader {
        fn new(bytes: &'static [u8]) -> Self {
            Self {
                bytes,
                total: Some(bytes.len() as u64),
                urls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn with_total(mut self, total: u64) -> Self {
            self.total = Some(total);
            self
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

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
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
