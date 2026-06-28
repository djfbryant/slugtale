use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

use serde::{Deserialize, Serialize};

/// The behavior assigned to a hotkey when controlling dictation (ADR-0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

/// The local non-secret Settings File (ADR-0018): user preferences such as
/// hotkey, activation mode, model choice, launch-at-login, and diagnostic logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub hotkey: Option<String>,
    pub activation_mode: ActivationMode,
    pub launch_at_login: bool,
    pub diagnostic_logging: bool,
    pub model: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: None,
            activation_mode: ActivationMode::Toggle,
            launch_at_login: false,
            diagnostic_logging: false,
            model: None,
        }
    }
}

/// Write the Settings File as human-readable JSON so it can be inspected
/// during development (ADR-0018).
pub fn save_settings(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, json)
}

/// Load the Settings File, falling back to defaults when it is missing or
/// unreadable (e.g. first run).
pub fn load_settings(path: &std::path::Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

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

pub trait ModelDownloader {
    fn download(&self, url: &str, destination: &std::path::Path) -> Result<(), ModelError>;
}

pub struct HttpModelDownloader;

impl ModelDownloader for HttpModelDownloader {
    fn download(&self, url: &str, destination: &std::path::Path) -> Result<(), ModelError> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|error| ModelError::Download(error.to_string()))?;
        let mut file = std::fs::File::create(destination)?;
        std::io::copy(&mut response.body_mut().as_reader(), &mut file)?;
        Ok(())
    }
}

pub fn ensure_default_model(
    model_dir: &std::path::Path,
    downloader: &dyn ModelDownloader,
) -> Result<LocalModelStatus, ModelError> {
    let current = local_model_status(model_dir);
    if current.present {
        return Ok(current);
    }

    std::fs::create_dir_all(model_dir)?;
    let staged_path = model_dir.join(format!("{DEFAULT_MODEL_FILENAME}.download"));
    std::fs::remove_file(&staged_path).ok();
    downloader.download(DEFAULT_MODEL_DOWNLOAD_URL, &staged_path)?;

    if staged_path.metadata()?.len() == 0 {
        std::fs::remove_file(&staged_path).ok();
        return Err(ModelError::Download(
            "downloaded model was empty".to_string(),
        ));
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedAudio {
    pub sample_rate_hz: u32,
    pub samples: Vec<f32>,
}

impl CapturedAudio {
    pub fn mono_16khz(samples: Vec<f32>) -> Self {
        Self {
            sample_rate_hz: 16_000,
            samples,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalTranscription {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsrError {
    ModelMissing { path: std::path::PathBuf },
    UnsupportedAudio(String),
    Runtime(String),
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModelMissing { path } => {
                write!(f, "local model is missing at {}", path.display())
            }
            Self::UnsupportedAudio(message) => write!(f, "unsupported captured audio: {message}"),
            Self::Runtime(message) => write!(f, "local transcription failed: {message}"),
        }
    }
}

impl std::error::Error for AsrError {}

pub trait AsrRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError>;
}

pub fn transcribe_captured_audio(
    runtime: &dyn AsrRuntime,
    audio: CapturedAudio,
) -> Result<FinalTranscription, AsrError> {
    if audio.sample_rate_hz != 16_000 {
        return Err(AsrError::UnsupportedAudio(
            "Whisper transcription expects 16 kHz mono f32 samples".to_string(),
        ));
    }

    runtime.transcribe(audio)
}

pub struct LocalWhisperRuntime {
    model_path: std::path::PathBuf,
}

impl LocalWhisperRuntime {
    pub fn new(model_path: std::path::PathBuf) -> Self {
        Self { model_path }
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        if !self.model_path.exists() {
            return Err(AsrError::ModelMissing {
                path: self.model_path.clone(),
            });
        }

        let model_path = self
            .model_path
            .to_str()
            .ok_or_else(|| AsrError::Runtime("model path is not valid UTF-8".to_string()))?;
        let context = whisper_rs::WhisperContext::new_with_params(
            model_path,
            whisper_rs::WhisperContextParameters::default(),
        )
        .map_err(|error| AsrError::Runtime(error.to_string()))?;
        let mut state = context
            .create_state()
            .map_err(|error| AsrError::Runtime(error.to_string()))?;
        let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });

        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &audio.samples)
            .map_err(|error| AsrError::Runtime(error.to_string()))?;

        let text = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();

        Ok(FinalTranscription { text })
    }
}

#[cfg(not(feature = "local-whisper-runtime"))]
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, _audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        if !self.model_path.exists() {
            return Err(AsrError::ModelMissing {
                path: self.model_path.clone(),
            });
        }

        Err(AsrError::Runtime(
            "local Whisper runtime was built without the local-whisper-runtime feature".to_string(),
        ))
    }
}

/// Platform Adapter boundary (ADR-0021) for the OS-specific facts that gate
/// dictation: microphone permission, text insertion permission, and whether the
/// local model is present on disk.
pub trait PlatformReadiness {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
    fn local_model_present(&self) -> bool;
}

/// Dictation Readiness (ADR-0013): dictation is only available once microphone
/// permission, text insertion permission, a configured hotkey, and a local model
/// are all ready.
pub fn dictation_ready(settings: &Settings, platform: &dyn PlatformReadiness) -> bool {
    settings.hotkey.is_some()
        && platform.microphone_granted()
        && platform.insertion_granted()
        && platform.local_model_present()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessItem {
    pub id: String,
    pub label: String,
    pub ready: bool,
    pub required: bool,
}

impl ReadinessItem {
    pub fn ready(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: true,
            required,
        }
    }

    pub fn missing(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: false,
            required,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsReadinessReport {
    pub dictation_available: bool,
    pub items: Vec<ReadinessItem>,
}

pub fn settings_readiness_report(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
) -> SettingsReadinessReport {
    SettingsReadinessReport {
        dictation_available: dictation_ready(settings, platform),
        items: vec![
            readiness_item(
                "microphone",
                "Microphone permission",
                true,
                platform.microphone_granted(),
            ),
            readiness_item(
                "text_insertion",
                "Text insertion permission",
                true,
                platform.insertion_granted(),
            ),
            readiness_item("hotkey", "Hotkey", true, settings.hotkey.is_some()),
            readiness_item(
                "local_model",
                "Local model",
                true,
                platform.local_model_present(),
            ),
            readiness_item("launch_at_login", "Launch at login", false, true),
        ],
    }
}

fn readiness_item(id: &str, label: &str, required: bool, ready: bool) -> ReadinessItem {
    if ready {
        ReadinessItem::ready(id, label, required)
    } else {
        ReadinessItem::missing(id, label, required)
    }
}

/// A raw hotkey signal delivered by the OS hotkey adapter (ADR-0021). The
/// adapter only reports key transitions; interpreting them into dictation
/// lifecycle events is the job of [`DictationLifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyInput {
    Pressed,
    Released,
}

/// An explicit dictation lifecycle event handed to the dictation pipeline.
/// `Stop` ends dictation and keeps the resulting transcription; `Cancel`
/// abandons the dictation and discards it (CONTEXT.md: Dictation Bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationEvent {
    Start,
    Stop,
    Cancel,
}

/// The configurable hotkey lifecycle (ADR-0004). Translates raw [`HotkeyInput`]
/// transitions into [`DictationEvent`]s according to the active
/// [`ActivationMode`], so the dictation pipeline receives explicit start, stop,
/// and cancel events without knowing how the hotkey behaves.
pub struct DictationLifecycle {
    mode: ActivationMode,
    dictating: bool,
}

impl DictationLifecycle {
    pub fn new(mode: ActivationMode) -> Self {
        Self {
            mode,
            dictating: false,
        }
    }

    pub fn is_dictating(&self) -> bool {
        self.dictating
    }

    pub fn on_hotkey(&mut self, input: HotkeyInput) -> Option<DictationEvent> {
        match input {
            HotkeyInput::Pressed if self.mode == ActivationMode::Toggle && self.dictating => {
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Pressed => {
                self.dictating = true;
                Some(DictationEvent::Start)
            }
            HotkeyInput::Released if self.mode == ActivationMode::Hold && self.dictating => {
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Released => None,
        }
    }

    pub fn cancel(&mut self) -> Option<DictationEvent> {
        if self.dictating {
            self.dictating = false;
            Some(DictationEvent::Cancel)
        } else {
            None
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::MacosPlatform;

/// macOS implementation of the [`PlatformReadiness`] adapter (ADR-0021). Resolves
/// the OS-specific dictation gates from live system state: microphone permission
/// via AVFoundation, text insertion permission via the Accessibility API, and the
/// local model by checking the model file on disk.
#[cfg(target_os = "macos")]
mod macos {
    use super::PlatformReadiness;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use std::path::PathBuf;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    pub struct MacosPlatform {
        model_path: PathBuf,
    }

    impl MacosPlatform {
        pub fn new(model_path: PathBuf) -> Self {
            Self { model_path }
        }
    }

    impl PlatformReadiness for MacosPlatform {
        fn microphone_granted(&self) -> bool {
            // Safe: passing a framework-provided media-type constant to a class method.
            unsafe {
                let audio = AVMediaTypeAudio.expect("AVMediaTypeAudio constant is always present");
                AVCaptureDevice::authorizationStatusForMediaType(audio)
                    == AVAuthorizationStatus::Authorized
            }
        }

        fn insertion_granted(&self) -> bool {
            // Accessibility trust governs text insertion via synthesized events.
            unsafe { AXIsProcessTrusted() }
        }

        fn local_model_present(&self) -> bool {
            self.model_path.exists()
        }
    }
}

pub fn build_tray_menu_items() -> Vec<(&'static str, &'static str)> {
    vec![("settings", "Settings\u{2026}"), ("quit", "Quit Slugtale")]
}

pub struct AppState {
    pub settings_visible: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            settings_visible: false,
        }
    }
}

/// Whether a window should hide (stay alive) on a close request rather than be
/// destroyed. Slugtale is a tray resident app (ADR-0008): the settings window is
/// reopened from the tray, so closing it must hide it — destroying it both kills
/// the only reopen path and, as the last window, would quit the whole app.
pub fn hides_on_close(window_label: &str) -> bool {
    window_label == "settings"
}

pub fn show_settings(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn setup_tray(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let items = build_tray_menu_items();

    let mut menu_items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = Vec::new();
    for (id, label) in &items {
        let item = MenuItem::with_id(app, *id, *label, true, None::<&str>)?;
        menu_items.push(Box::new(item));
    }

    let menu_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        menu_items.iter().map(|i| i.as_ref()).collect();
    let menu = Menu::with_items(app, &menu_refs)?;

    let icon = Image::from_bytes(include_bytes!("../icons/icon.png"))?;

    TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Slugtale")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                show_settings(app.clone());
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_mode_press_starts_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert!(lifecycle.is_dictating());
    }

    #[test]
    fn hold_mode_release_stops_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Released),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn hold_mode_release_while_idle_does_nothing() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn toggle_mode_second_press_stops_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn toggle_mode_release_is_ignored() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert!(
            lifecycle.is_dictating(),
            "holding the key must not stop toggle dictation"
        );
    }

    #[test]
    fn cancel_while_dictating_emits_cancel_and_returns_to_idle() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(lifecycle.cancel(), Some(DictationEvent::Cancel));
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn cancel_while_idle_does_nothing() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(lifecycle.cancel(), None);
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn settings_window_hides_instead_of_closing() {
        assert!(hides_on_close("settings"));
    }

    #[test]
    fn unknown_windows_are_allowed_to_close() {
        assert!(!hides_on_close("dictation-bar"));
    }

    #[test]
    fn fresh_settings_default_to_unconfigured_and_opt_out() {
        let settings = Settings::default();
        assert_eq!(settings.hotkey, None);
        assert_eq!(settings.activation_mode, ActivationMode::Toggle);
        assert!(!settings.launch_at_login);
        assert!(!settings.diagnostic_logging);
        assert_eq!(settings.model, None);
    }

    #[test]
    fn settings_round_trip_through_saved_file() {
        let path =
            std::env::temp_dir().join(format!("slugtale-settings-{}.json", std::process::id()));
        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            activation_mode: ActivationMode::Hold,
            launch_at_login: true,
            diagnostic_logging: true,
            model: Some("whisper-base.en".to_string()),
        };

        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn loading_missing_settings_file_returns_defaults() {
        let path = std::env::temp_dir().join("slugtale-settings-does-not-exist.json");
        std::fs::remove_file(&path).ok();

        assert_eq!(load_settings(&path), Settings::default());
    }

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
    fn ensure_default_model_downloads_missing_base_en_model() {
        let model_dir = unique_test_dir("model-download");
        std::fs::remove_dir_all(&model_dir).ok();
        let downloader = FakeModelDownloader::new(b"local model bytes");

        let status = ensure_default_model(&model_dir, &downloader).unwrap();

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

        std::fs::remove_dir_all(&model_dir).ok();
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
    fn transcribe_captured_audio_returns_final_transcription_from_asr_runtime() {
        let runtime = FakeAsrRuntime::new("hello from slugtale");
        let audio = CapturedAudio::mono_16khz(vec![0.0, 0.25, -0.25]);

        let transcription = transcribe_captured_audio(&runtime, audio).unwrap();

        assert_eq!(transcription.text, "hello from slugtale");
        assert_eq!(runtime.sample_counts.borrow().as_slice(), &[3]);
    }

    #[test]
    fn transcribe_captured_audio_rejects_non_16khz_audio_before_runtime() {
        let runtime = FakeAsrRuntime::new("should not run");
        let audio = CapturedAudio {
            sample_rate_hz: 44_100,
            samples: vec![0.0],
        };

        let error = transcribe_captured_audio(&runtime, audio).unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Whisper transcription expects 16 kHz mono f32 samples".to_string()
            )
        );
        assert!(runtime.sample_counts.borrow().is_empty());
    }

    #[test]
    fn local_whisper_runtime_reports_missing_model_before_transcription() {
        let model_path = unique_test_dir("missing-model").join(DEFAULT_MODEL_FILENAME);
        let runtime = LocalWhisperRuntime::new(model_path.clone());

        let error = runtime
            .transcribe(CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .unwrap_err();

        assert_eq!(error, AsrError::ModelMissing { path: model_path });
    }

    struct FakePlatform {
        microphone: bool,
        insertion: bool,
        model: bool,
    }

    impl FakePlatform {
        fn all_ready() -> Self {
            Self {
                microphone: true,
                insertion: true,
                model: true,
            }
        }
    }

    struct FakeModelDownloader {
        bytes: &'static [u8],
        urls: std::cell::RefCell<Vec<String>>,
    }

    impl FakeModelDownloader {
        fn new(bytes: &'static [u8]) -> Self {
            Self {
                bytes,
                urls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl ModelDownloader for FakeModelDownloader {
        fn download(&self, url: &str, destination: &std::path::Path) -> Result<(), ModelError> {
            self.urls.borrow_mut().push(url.to_string());
            std::fs::write(destination, self.bytes).map_err(ModelError::Io)
        }
    }

    struct FakeAsrRuntime {
        text: &'static str,
        sample_counts: std::cell::RefCell<Vec<usize>>,
    }

    impl FakeAsrRuntime {
        fn new(text: &'static str) -> Self {
            Self {
                text,
                sample_counts: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl AsrRuntime for FakeAsrRuntime {
        fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
            self.sample_counts.borrow_mut().push(audio.samples.len());
            Ok(FinalTranscription {
                text: self.text.to_string(),
            })
        }
    }

    impl PlatformReadiness for FakePlatform {
        fn microphone_granted(&self) -> bool {
            self.microphone
        }
        fn insertion_granted(&self) -> bool {
            self.insertion
        }
        fn local_model_present(&self) -> bool {
            self.model
        }
    }

    fn configured_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
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

    #[test]
    fn dictation_is_not_ready_when_nothing_is_ready() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
            model: false,
        };
        assert!(!dictation_ready(&Settings::default(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_microphone_permission() {
        let platform = FakePlatform {
            microphone: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_insertion_permission() {
        let platform = FakePlatform {
            insertion: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_configured_hotkey() {
        let settings = Settings {
            hotkey: None,
            ..Settings::default()
        };
        assert!(!dictation_ready(&settings, &FakePlatform::all_ready()));
    }

    #[test]
    fn dictation_is_not_ready_without_local_model() {
        let platform = FakePlatform {
            model: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_ready_when_all_requirements_are_met() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready()
        ));
    }

    #[test]
    fn settings_readiness_report_shows_missing_required_items() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
            model: false,
        };
        let report = settings_readiness_report(&Settings::default(), &platform);

        assert!(!report.dictation_available);
        assert_eq!(
            report.items,
            vec![
                ReadinessItem::missing("microphone", "Microphone permission", true),
                ReadinessItem::missing("text_insertion", "Text insertion permission", true),
                ReadinessItem::missing("hotkey", "Hotkey", true),
                ReadinessItem::missing("local_model", "Local model", true),
                ReadinessItem::ready("launch_at_login", "Launch at login", false),
            ]
        );
    }

    #[test]
    fn settings_readiness_report_allows_dictation_when_required_items_are_ready() {
        let report = settings_readiness_report(&configured_settings(), &FakePlatform::all_ready());

        assert!(report.dictation_available);
        assert!(report
            .items
            .iter()
            .filter(|item| item.required)
            .all(|item| item.ready));
    }

    #[test]
    fn activation_mode_persists_as_stable_lowercase_strings() {
        let settings = Settings {
            activation_mode: ActivationMode::Hold,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"activation_mode\":\"hold\""), "got: {json}");

        let toggled = Settings {
            activation_mode: ActivationMode::Toggle,
            ..Settings::default()
        };
        let json = serde_json::to_string(&toggled).unwrap();
        assert!(
            json.contains("\"activation_mode\":\"toggle\""),
            "got: {json}"
        );
    }

    #[test]
    fn app_state_defaults_to_settings_hidden() {
        let state = AppState::default();
        assert!(!state.settings_visible);
    }

    #[test]
    fn tray_menu_has_settings_item() {
        let items = build_tray_menu_items();
        assert!(items.iter().any(|(id, _)| *id == "settings"));
    }

    #[test]
    fn tray_menu_has_quit_item() {
        let items = build_tray_menu_items();
        assert!(items.iter().any(|(id, _)| *id == "quit"));
    }

    #[test]
    fn settings_item_label_matches_spec() {
        let items = build_tray_menu_items();
        let label = items.iter().find(|(id, _)| *id == "settings").unwrap().1;
        assert_eq!(label, "Settings\u{2026}");
    }

    #[test]
    fn quit_item_label_matches_spec() {
        let items = build_tray_menu_items();
        let label = items.iter().find(|(id, _)| *id == "quit").unwrap().1;
        assert_eq!(label, "Quit Slugtale");
    }
}
