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

#[derive(Debug, Clone, Copy)]
struct DownloadTimeouts {
    total: std::time::Duration,
    stalled_read: std::time::Duration,
}

impl DownloadTimeouts {
    const PRODUCTION: Self = Self {
        total: std::time::Duration::from_secs(60 * 60),
        stalled_read: std::time::Duration::from_secs(30),
    };
}

impl HttpModelDownloader {
    fn download_with_timeouts(
        &self,
        url: &str,
        destination: &std::path::Path,
        on_progress: &mut dyn FnMut(DownloadProgress),
        timeouts: DownloadTimeouts,
    ) -> Result<(), ModelError> {
        let mut response = ureq::get(url)
            .config()
            .timeout_global(Some(timeouts.total))
            .timeout_recv_body(Some(timeouts.stalled_read))
            .build()
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

impl ModelDownloader for HttpModelDownloader {
    fn download(
        &self,
        url: &str,
        destination: &std::path::Path,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<(), ModelError> {
        self.download_with_timeouts(url, destination, on_progress, DownloadTimeouts::PRODUCTION)
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

/// How much new data must arrive before the frontend hears about it again.
/// A progress bar reads smoothly at a handful of updates per second; at 64 KiB
/// chunks a ~140 MB model would otherwise flood the IPC channel with thousands
/// of messages (slugtale-dtl).
const PROGRESS_THROTTLE_BYTES: u64 = 1024 * 1024;

/// Wrap a progress sink so it hears the initial zero-byte update and the final
/// complete one, but intermediate updates only once per
/// `PROGRESS_THROTTLE_BYTES`. The download commands hand this the Tauri IPC
/// channel so the settings UI gets a smooth bar without the flood.
pub fn throttled_progress(mut send: impl FnMut(DownloadProgress)) -> impl FnMut(DownloadProgress) {
    let mut last_sent = 0u64;
    move |progress| {
        let complete = progress
            .total
            .is_some_and(|total| progress.downloaded >= total);
        if progress.downloaded == 0
            || complete
            || progress.downloaded - last_sent >= PROGRESS_THROTTLE_BYTES
        {
            last_sent = progress.downloaded;
            send(progress);
        }
    }
}

/// Install the managed default artifact only when it matches a trusted SHA-256
/// digest. The staged file is removed on every validation failure so corrupt
/// bytes can never become the active Local Model.
pub fn ensure_default_model_with_sha256(
    model_dir: &std::path::Path,
    downloader: &dyn ModelDownloader,
    expected_sha256: &str,
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
        return Err(reject_staged_download(
            &staged_path,
            "downloaded model was empty".to_string(),
        ));
    }
    if let Some(expected_bytes) = expected_bytes {
        if downloaded_bytes != expected_bytes {
            return Err(reject_staged_download(
                &staged_path,
                format!(
                    "downloaded model was incomplete: expected {expected_bytes} bytes, got {downloaded_bytes}"
                ),
            ));
        }
    }

    let actual_sha256 = match sha256_file(&staged_path) {
        Ok(digest) => digest,
        Err(error) => {
            return Err(reject_staged_download(
                &staged_path,
                format!("could not verify downloaded model checksum: {error}"),
            ));
        }
    };
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(reject_staged_download(
            &staged_path,
            format!(
                "downloaded model checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
            ),
        ));
    }

    std::fs::rename(staged_path, default_model_path(model_dir))?;
    Ok(local_model_status(model_dir))
}

fn reject_staged_download(path: &std::path::Path, message: String) -> ModelError {
    match std::fs::remove_file(path) {
        Ok(()) => ModelError::Download(message),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ModelError::Download(message),
        Err(error) => ModelError::Download(format!(
            "{message}; could not delete invalid staged model: {error}"
        )),
    }
}

fn sha256_file(path: &std::path::Path) -> Result<String, ModelError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn delete_default_model(model_dir: &std::path::Path) -> Result<LocalModelStatus, ModelError> {
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
    fn model_download_installs_only_when_the_pinned_checksum_matches() {
        const TRUSTED_SHA256: &str =
            "6d6065cea517391b0166d6a74be33c924cc416b959fa1eee6a146094195b639d";
        let trusted_dir = unique_test_dir("trusted-model");
        let corrupt_dir = unique_test_dir("corrupt-model");
        let trusted = FakeModelDownloader::new(b"trusted model");
        let corrupt = FakeModelDownloader::new(b"corrupt model");
        let mut progress = Vec::new();

        let installed = ensure_default_model_with_sha256(
            &trusted_dir,
            &trusted,
            TRUSTED_SHA256,
            &mut |update| progress.push(update),
        )
        .unwrap();
        assert!(installed.present);
        assert_eq!(installed.bytes, Some(13));
        assert_eq!(
            trusted.urls.borrow().as_slice(),
            &[DEFAULT_MODEL_DOWNLOAD_URL]
        );
        assert_eq!(
            progress.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total: Some(13),
            })
        );
        assert_eq!(
            progress.last().copied(),
            Some(DownloadProgress {
                downloaded: 13,
                total: Some(13),
            })
        );

        let error =
            ensure_default_model_with_sha256(&corrupt_dir, &corrupt, TRUSTED_SHA256, &mut |_| {})
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "model download error: downloaded model checksum mismatch: expected 6d6065cea517391b0166d6a74be33c924cc416b959fa1eee6a146094195b639d, got 1d606b22e45655bf1b0908053d32f069e2cb36f77d920d900499032f61c07f86"
        );
        assert!(!default_model_path(&corrupt_dir).exists());
        assert!(!corrupt_dir.join("ggml-base.en.bin.download").exists());

        std::fs::remove_dir_all(trusted_dir).ok();
        std::fs::remove_dir_all(corrupt_dir).ok();
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
    fn http_download_errors_when_the_response_body_stalls() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release_server, wait_for_release) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\npartial")
                .unwrap();
            stream.flush().unwrap();
            let _ = wait_for_release.recv_timeout(std::time::Duration::from_secs(2));
        });

        let model_dir = unique_test_dir("stalled-http-download");
        std::fs::create_dir_all(&model_dir).unwrap();
        let destination = model_dir.join("partial.bin");
        let (send_result, receive_result) = std::sync::mpsc::channel();
        let download = std::thread::spawn(move || {
            let result = HttpModelDownloader.download_with_timeouts(
                &format!("http://{address}/model.bin"),
                &destination,
                &mut |_| {},
                DownloadTimeouts {
                    total: std::time::Duration::from_secs(1),
                    stalled_read: std::time::Duration::from_millis(100),
                },
            );
            let _ = send_result.send(result);
        });

        let result = receive_result.recv_timeout(std::time::Duration::from_secs(1));
        let _ = release_server.send(());
        server.join().unwrap();
        download.join().unwrap();
        std::fs::remove_dir_all(model_dir).ok();

        assert!(result
            .expect("the download must honor its timeout")
            .is_err());
    }
    #[test]
    fn http_download_allows_a_slow_body_that_keeps_making_progress() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n")
                .unwrap();
            for byte in b"steady" {
                stream.write_all(std::slice::from_ref(byte)).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });

        let model_dir = unique_test_dir("steady-http-download");
        std::fs::create_dir_all(&model_dir).unwrap();
        let destination = model_dir.join("steady.bin");
        let result = HttpModelDownloader.download_with_timeouts(
            &format!("http://{address}/model.bin"),
            &destination,
            &mut |_| {},
            DownloadTimeouts {
                total: std::time::Duration::from_secs(1),
                stalled_read: std::time::Duration::from_millis(200),
            },
        );
        server.join().unwrap();

        assert!(result.is_ok());
        assert_eq!(std::fs::read(destination).unwrap(), b"steady");
        std::fs::remove_dir_all(model_dir).ok();
    }
    #[test]
    fn throttled_progress_always_sends_the_first_and_last_update() {
        let total = Some(2 * PROGRESS_THROTTLE_BYTES);
        let mut sent = Vec::new();
        {
            let mut throttle = throttled_progress(|update| sent.push(update));
            // The initial update, then a trickle of sub-threshold steps, then
            // the final complete byte count.
            for downloaded in [0, 1, 500_000, 999_999, 2 * PROGRESS_THROTTLE_BYTES] {
                throttle(DownloadProgress { downloaded, total });
            }
        }

        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total
            })
        );
        assert_eq!(
            sent.last().copied(),
            Some(DownloadProgress {
                downloaded: 2 * PROGRESS_THROTTLE_BYTES,
                total
            })
        );
    }

    #[test]
    fn throttled_progress_sends_intermediate_updates_once_per_threshold() {
        // No total: the first test pins the completion path, this one isolates
        // the byte-threshold path.
        let total = None;
        let mut sent = Vec::new();
        {
            let mut throttle = throttled_progress(|update| sent.push(update));
            let mut updates = vec![0u64];
            for megabyte in 1..=3u64 {
                // A step just past each megabyte boundary, plus one that lands
                // short of the next boundary and must be swallowed.
                updates.push(megabyte * PROGRESS_THROTTLE_BYTES + 1);
                updates.push((megabyte + 1) * PROGRESS_THROTTLE_BYTES - 1);
            }
            for downloaded in updates {
                throttle(DownloadProgress { downloaded, total });
            }
        }

        assert_eq!(
            sent.iter()
                .map(|update| update.downloaded)
                .collect::<Vec<_>>(),
            vec![
                0,
                PROGRESS_THROTTLE_BYTES + 1,
                2 * PROGRESS_THROTTLE_BYTES + 1,
                3 * PROGRESS_THROTTLE_BYTES + 1
            ]
        );
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
