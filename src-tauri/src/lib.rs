use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

use serde::{Deserialize, Serialize};

/// The Local Diagnostic Log domain (ADR-0019). Extracted into its own module;
/// re-exported so existing `slugtale_lib::*` call sites keep compiling.
mod diagnostics;
pub use diagnostics::*;

mod recording_feedback;
pub use recording_feedback::*;

/// Audio Capture (CONTEXT.md): microphone recording and the perceptual voice
/// level the dictation waveform renders. Extracted into its own module; the
/// `AudioRecorder` trait stays the test seam and `cpal` an impl detail behind
/// `CpalAudioRecorder`. Re-exported so existing `slugtale_lib::*` call sites keep
/// compiling.
mod audio_capture;
pub use audio_capture::*;

/// Text Insertion and Insertion Rescue (CONTEXT.md): the clipboard-free
/// insertion pipeline and the clipboard rescue that preserves a transcription
/// when insertion fails. The `*System` traits stay the platform-adapter seam.
/// Re-exported so existing `slugtale_lib::*` call sites keep compiling.
mod text_insertion;
pub use text_insertion::*;

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

/// Update the user-configurable hotkey preferences that live in the Settings
/// File. Empty input clears the hotkey so Dictation Readiness reflects that the
/// user has not configured one.
pub fn apply_hotkey_settings(
    settings: &mut Settings,
    hotkey: Option<String>,
    activation_mode: ActivationMode,
) {
    settings.hotkey = hotkey.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    settings.activation_mode = activation_mode;
}

/// Write the Settings File as human-readable JSON so it can be inspected
/// during development (ADR-0018).
pub fn save_settings(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));

    std::fs::write(&temp_path, json)?;
    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
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

pub trait MicrophonePermissionSetup {
    fn request_microphone_access(&self) -> Result<(), String>;
    fn open_microphone_settings(&self) -> Result<(), String>;
}

pub fn run_microphone_permission_setup(
    system: &dyn MicrophonePermissionSetup,
) -> Result<(), String> {
    let request_result = system.request_microphone_access();
    let open_result = system.open_microphone_settings();

    open_result?;
    request_result
}

pub trait TextInsertionPermissionSetup {
    fn request_text_insertion_access(&self) -> Result<bool, String>;
    fn open_text_insertion_settings(&self) -> Result<(), String>;
}

pub fn run_text_insertion_permission_setup(
    system: &dyn TextInsertionPermissionSetup,
) -> Result<bool, String> {
    let trusted = system.request_text_insertion_access()?;
    if !trusted {
        system.open_text_insertion_settings()?;
    }
    Ok(trusted)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationWorkflowError {
    Transcription(AsrError),
    InsertionRescue(InsertionRescueError),
}

impl std::fmt::Display for DictationWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transcription(error) => write!(f, "{error}"),
            Self::InsertionRescue(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DictationWorkflowError {}

pub struct DictationWorkflow<'a> {
    runtime: &'a dyn AsrRuntime,
    text_insertion: &'a dyn TextInsertion,
    insertion_rescue: &'a dyn InsertionRescue,
}

impl<'a> DictationWorkflow<'a> {
    pub fn new(
        runtime: &'a dyn AsrRuntime,
        text_insertion: &'a dyn TextInsertion,
        insertion_rescue: &'a dyn InsertionRescue,
    ) -> Self {
        Self {
            runtime,
            text_insertion,
            insertion_rescue,
        }
    }

    pub fn complete(
        &self,
        audio: CapturedAudio,
    ) -> Result<FinalTranscription, DictationWorkflowError> {
        let transcription = transcribe_captured_audio(self.runtime, audio)
            .map_err(DictationWorkflowError::Transcription)?;
        let transcription = clean_final_transcription(transcription);
        if self.text_insertion.insert(&transcription).is_err() {
            self.insertion_rescue
                .rescue(&transcription)
                .map_err(DictationWorkflowError::InsertionRescue)?;
        }
        Ok(transcription)
    }
}

/// Apply deterministic Transcript Cleanup before insertion without rewriting
/// meaning or adding generated text.
pub fn clean_final_transcription(transcription: FinalTranscription) -> FinalTranscription {
    let normalized = transcription
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return FinalTranscription { text: normalized };
    };
    let text = if first.is_lowercase() {
        format!("{}{}", first.to_uppercase(), chars.as_str())
    } else {
        normalized
    };

    FinalTranscription { text }
}

pub struct LocalWhisperRuntime {
    model_path: std::path::PathBuf,
    // The loaded model is expensive to read and parse, so it is initialized once
    // and reused across transcriptions rather than rebuilt on every call.
    #[cfg(feature = "local-whisper-runtime")]
    context: std::sync::OnceLock<whisper_rs::WhisperContext>,
}

impl LocalWhisperRuntime {
    pub fn new(model_path: std::path::PathBuf) -> Self {
        Self {
            model_path,
            #[cfg(feature = "local-whisper-runtime")]
            context: std::sync::OnceLock::new(),
        }
    }

    pub fn model_path(&self) -> &std::path::Path {
        &self.model_path
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl LocalWhisperRuntime {
    /// Return the loaded Whisper context, reading the model file from disk only
    /// on the first call and caching it for subsequent transcriptions.
    fn context(&self) -> Result<&whisper_rs::WhisperContext, AsrError> {
        if let Some(context) = self.context.get() {
            return Ok(context);
        }

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

        // If another thread won the race to initialize, keep the stored context.
        let _ = self.context.set(context);
        Ok(self.context.get().expect("context was just initialized"))
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        let context = self.context()?;
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
    hotkey_down: bool,
}

impl DictationLifecycle {
    pub fn new(mode: ActivationMode) -> Self {
        Self {
            mode,
            dictating: false,
            hotkey_down: false,
        }
    }

    pub fn is_dictating(&self) -> bool {
        self.dictating
    }

    pub fn on_hotkey(&mut self, input: HotkeyInput) -> Option<DictationEvent> {
        match input {
            HotkeyInput::Pressed if self.hotkey_down => None,
            HotkeyInput::Pressed if self.mode == ActivationMode::Toggle && self.dictating => {
                self.hotkey_down = true;
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Pressed => {
                self.hotkey_down = true;
                self.dictating = true;
                Some(DictationEvent::Start)
            }
            HotkeyInput::Released if self.mode == ActivationMode::Hold && self.dictating => {
                self.hotkey_down = false;
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Released => {
                self.hotkey_down = false;
                None
            }
        }
    }

    pub fn cancel(&mut self) -> Option<DictationEvent> {
        if self.dictating {
            self.hotkey_down = false;
            self.dictating = false;
            Some(DictationEvent::Cancel)
        } else {
            None
        }
    }
}

/// Consumer for dictation lifecycle events emitted by a Platform Adapter.
pub trait DictationEventSink {
    fn emit(&mut self, event: DictationEvent);
}

impl<F> DictationEventSink for F
where
    F: FnMut(DictationEvent),
{
    fn emit(&mut self, event: DictationEvent) {
        self(event);
    }
}

/// Adapter-facing bridge from OS hotkey transitions into the dictation pipeline.
/// The Platform Adapter owns hotkey registration; this bridge owns the
/// lifecycle state so hold and toggle modes stay consistent across callbacks.
pub struct HotkeyDictationAdapter<S> {
    lifecycle: DictationLifecycle,
    sink: S,
}

impl<S> HotkeyDictationAdapter<S>
where
    S: DictationEventSink,
{
    pub fn new(mode: ActivationMode, sink: S) -> Self {
        Self {
            lifecycle: DictationLifecycle::new(mode),
            sink,
        }
    }

    pub fn on_hotkey(&mut self, input: HotkeyInput) {
        if let Some(event) = self.lifecycle.on_hotkey(input) {
            self.sink.emit(event);
        }
    }

    pub fn cancel(&mut self) {
        if let Some(event) = self.lifecycle.cancel() {
            self.sink.emit(event);
        }
    }

    pub fn is_dictating(&self) -> bool {
        self.lifecycle.is_dictating()
    }
}

#[cfg(target_os = "macos")]
pub use macos::{
    accessibility_trusted, activate_app, frontmost_app_pid, notify, open_accessibility_settings,
    MacosInsertionRescue, MacosMicrophonePermissionSetup, MacosPlatform, MacosTextInsertion,
    MacosTextInsertionPermissionSetup,
};

/// macOS implementation of the [`PlatformReadiness`] adapter (ADR-0021). Resolves
/// the OS-specific dictation gates from live system state: microphone permission
/// via AVFoundation, text insertion permission via the Accessibility API, and the
/// local model by checking the model file on disk.
#[cfg(target_os = "macos")]
mod macos {
    use super::MicrophonePermissionSetup;
    use super::{
        ClipboardInsertionRescue, FinalTranscription, InsertionRescue, InsertionRescueError,
        InsertionRescueOutcome, InsertionRescueSystem, PlatformReadiness, TextInsertion,
        TextInsertionError, TextInsertionOutcome, TextInsertionPermissionSetup,
        TextInsertionPipeline, TextInsertionSystem,
    };
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use std::ffi::c_void;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::ptr;
    use std::time::Duration;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(
            event: *mut c_void,
            string_length: usize,
            unicode_string: *const u16,
        );
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPost(tap: u32, event: *mut c_void);
        fn CFRelease(object: *const c_void);
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            num_values: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        static kCFBooleanTrue: *const c_void;
    }

    const K_CG_HID_EVENT_TAP: u32 = 0;
    const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
    const MACOS_V_KEY_CODE: u16 = 9;

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

    pub struct MacosTextInsertion {
        pipeline: TextInsertionPipeline<MacosTextInsertionSystem>,
    }

    impl Default for MacosTextInsertion {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MacosTextInsertion {
        pub fn new() -> Self {
            Self {
                pipeline: TextInsertionPipeline::new(MacosTextInsertionSystem),
            }
        }
    }

    impl TextInsertion for MacosTextInsertion {
        fn insert(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<TextInsertionOutcome, TextInsertionError> {
            self.pipeline.insert(transcription)
        }
    }

    struct MacosTextInsertionSystem;

    impl TextInsertionSystem for MacosTextInsertionSystem {
        fn insert_clipboard_free(&self, text: &str) -> Result<(), TextInsertionError> {
            // CGEventPost is silently dropped when the process is not trusted for
            // Accessibility, and the call itself returns no delivery status. Without
            // this gate `post_unicode_text` would report success for events that
            // never reached any app, so the clipboard fallback and rescue would be
            // skipped (slugtale-iy2, slugtale-avo). Refusing here lets the pipeline
            // fall through to clipboard paste and, failing that, the rescue.
            if !accessibility_trusted() {
                return Err(TextInsertionError::new(ACCESSIBILITY_NOT_TRUSTED));
            }
            post_unicode_text(text).map_err(TextInsertionError::new)
        }

        fn insert_from_clipboard(&self, text: &str) -> Result<(), TextInsertionError> {
            // The Cmd+V paste is also a synthesized event, so it needs the same
            // Accessibility trust; bail early to reach the clipboard rescue.
            if !accessibility_trusted() {
                return Err(TextInsertionError::new(ACCESSIBILITY_NOT_TRUSTED));
            }
            copy_text_to_clipboard(text).map_err(TextInsertionError::new)?;
            std::thread::sleep(Duration::from_millis(30));
            post_command_v().map_err(TextInsertionError::new)
        }
    }

    const ACCESSIBILITY_NOT_TRUSTED: &str =
        "Slugtale is not trusted for Accessibility, so synthesized keystrokes are dropped";

    /// Whether this process may post synthesized keyboard events. macOS gates this
    /// behind the Accessibility privacy list; an untrusted process can still create
    /// a `CGEvent` but `CGEventPost` silently discards it. The dev binary path
    /// changes between builds, so a previously granted entry goes stale and must be
    /// re-granted (slugtale-avo).
    pub fn accessibility_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn request_accessibility_trust_prompt() -> bool {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                ptr::null(),
                ptr::null(),
            );

            if options.is_null() {
                return AXIsProcessTrusted();
            }

            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            trusted
        }
    }

    pub struct MacosTextInsertionPermissionSetup;

    impl TextInsertionPermissionSetup for MacosTextInsertionPermissionSetup {
        fn request_text_insertion_access(&self) -> Result<bool, String> {
            Ok(request_accessibility_trust_prompt())
        }

        fn open_text_insertion_settings(&self) -> Result<(), String> {
            open_accessibility_settings()
        }
    }

    pub struct MacosInsertionRescue {
        rescue: ClipboardInsertionRescue<MacosInsertionRescueSystem>,
    }

    impl Default for MacosInsertionRescue {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MacosInsertionRescue {
        pub fn new() -> Self {
            Self {
                rescue: ClipboardInsertionRescue::new(MacosInsertionRescueSystem),
            }
        }
    }

    impl InsertionRescue for MacosInsertionRescue {
        fn rescue(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
            self.rescue.rescue(transcription)
        }
    }

    struct MacosInsertionRescueSystem;

    impl InsertionRescueSystem for MacosInsertionRescueSystem {
        fn copy_to_clipboard(&self, text: &str) -> Result<(), InsertionRescueError> {
            copy_text_to_clipboard(text).map_err(InsertionRescueError::new)
        }

        fn notify_user(&self, title: &str, body: &str) -> Result<(), InsertionRescueError> {
            notify_user(title, body).map_err(InsertionRescueError::new)
        }
    }

    fn post_unicode_text(text: &str) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }

        let units = text.encode_utf16().collect::<Vec<_>>();
        let key_down = create_keyboard_event(0, true)?;
        unsafe {
            CGEventKeyboardSetUnicodeString(key_down, units.len(), units.as_ptr());
            CGEventPost(K_CG_HID_EVENT_TAP, key_down);
            CFRelease(key_down.cast_const());
        }

        let key_up = create_keyboard_event(0, false)?;
        unsafe {
            CGEventKeyboardSetUnicodeString(key_up, units.len(), units.as_ptr());
            CGEventPost(K_CG_HID_EVENT_TAP, key_up);
            CFRelease(key_up.cast_const());
        }

        Ok(())
    }

    fn post_command_v() -> Result<(), String> {
        post_key_with_flags(MACOS_V_KEY_CODE, K_CG_EVENT_FLAG_MASK_COMMAND)
    }

    fn post_key_with_flags(virtual_key: u16, flags: u64) -> Result<(), String> {
        let key_down = create_keyboard_event(virtual_key, true)?;
        unsafe {
            CGEventSetFlags(key_down, flags);
            CGEventPost(K_CG_HID_EVENT_TAP, key_down);
            CFRelease(key_down.cast_const());
        }

        let key_up = create_keyboard_event(virtual_key, false)?;
        unsafe {
            CGEventSetFlags(key_up, flags);
            CGEventPost(K_CG_HID_EVENT_TAP, key_up);
            CFRelease(key_up.cast_const());
        }

        Ok(())
    }

    fn create_keyboard_event(virtual_key: u16, key_down: bool) -> Result<*mut c_void, String> {
        let event = unsafe { CGEventCreateKeyboardEvent(ptr::null(), virtual_key, key_down) };
        if event.is_null() {
            Err("could not create macOS keyboard event; check Accessibility permission".to_string())
        } else {
            Ok(event)
        }
    }

    fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start pbcopy: {error}"))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "could not open pbcopy stdin".to_string())?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("could not write transcription to clipboard: {error}"))?;
        drop(stdin);

        let status = child
            .wait()
            .map_err(|error| format!("could not finish pbcopy: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("pbcopy exited with status {status}"))
        }
    }

    fn notify_user(title: &str, body: &str) -> Result<(), String> {
        let script = format!(
            "display notification {} with title {}",
            applescript_string(body),
            applescript_string(title)
        );
        let status = Command::new("osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map_err(|error| format!("could not show insertion failure notification: {error}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("osascript exited with status {status}"))
        }
    }

    fn applescript_string(value: &str) -> String {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// Show a user-facing notification (used to guide the user to grant
    /// Accessibility when insertion can't reach the focused app). Exposed for the
    /// Tauri layer; failures are non-fatal and surfaced to the caller.
    pub fn notify(title: &str, body: &str) -> Result<(), String> {
        notify_user(title, body)
    }

    fn open_system_settings_url(url: &str) -> Result<(), String> {
        Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|error| format!("could not open System Settings: {error}"))?;
        Ok(())
    }

    pub fn open_accessibility_settings() -> Result<(), String> {
        open_system_settings_url(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        )
    }

    pub fn open_microphone_settings() -> Result<(), String> {
        open_system_settings_url(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        )
    }

    pub struct MacosMicrophonePermissionSetup;

    impl MicrophonePermissionSetup for MacosMicrophonePermissionSetup {
        fn request_microphone_access(&self) -> Result<(), String> {
            request_microphone_access()
        }

        fn open_microphone_settings(&self) -> Result<(), String> {
            open_microphone_settings()
        }
    }

    fn request_microphone_access() -> Result<(), String> {
        unsafe {
            let audio = AVMediaTypeAudio.expect("AVMediaTypeAudio constant is always present");
            match AVCaptureDevice::authorizationStatusForMediaType(audio) {
                AVAuthorizationStatus::Authorized
                | AVAuthorizationStatus::Denied
                | AVAuthorizationStatus::Restricted => return Ok(()),
                AVAuthorizationStatus::NotDetermined => {}
                _ => {}
            }

            let block: RcBlock<dyn Fn(Bool)> = RcBlock::new(|_granted: Bool| {});
            AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &block);
        }

        Ok(())
    }

    /// The process id of the frontmost application — the app that owns the text
    /// field the user is dictating into. Captured at recording start so insertion
    /// can re-target it even if focus drifts to Slugtale's own UI (slugtale-squ).
    pub fn frontmost_app_pid() -> Option<i32> {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
    }

    /// Bring the app with `pid` back to the front so synthesized keystrokes land in
    /// its focused field. Mirrors FluidVoice's `activateApp(pid:)` reactivation step
    /// before pasting. Returns whether a running app was found to activate.
    pub fn activate_app(pid: i32) -> bool {
        match NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            Some(app) => {
                app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
            }
            None => false,
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

pub fn dictation_bar_should_take_focus() -> bool {
    false
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
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn toggle_mode_ignores_repeated_press_until_key_release() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Pressed), None);
        assert!(lifecycle.is_dictating());
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
    fn hotkey_adapter_forwards_hold_mode_events_to_dictation_sink() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut adapter = HotkeyDictationAdapter::new(ActivationMode::Hold, |event| {
            events.borrow_mut().push(event);
        });

        adapter.on_hotkey(HotkeyInput::Pressed);
        adapter.on_hotkey(HotkeyInput::Released);

        assert_eq!(
            *events.borrow(),
            vec![DictationEvent::Start, DictationEvent::Stop]
        );
        assert!(!adapter.is_dictating());
    }

    #[test]
    fn hotkey_adapter_forwards_toggle_mode_events_to_dictation_sink() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut adapter = HotkeyDictationAdapter::new(ActivationMode::Toggle, |event| {
            events.borrow_mut().push(event);
        });

        adapter.on_hotkey(HotkeyInput::Pressed);
        adapter.on_hotkey(HotkeyInput::Released);
        adapter.on_hotkey(HotkeyInput::Pressed);

        assert_eq!(
            *events.borrow(),
            vec![DictationEvent::Start, DictationEvent::Stop]
        );
        assert!(!adapter.is_dictating());
    }

    #[test]
    fn hotkey_adapter_forwards_cancel_and_returns_to_idle() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut adapter = HotkeyDictationAdapter::new(ActivationMode::Hold, |event| {
            events.borrow_mut().push(event);
        });

        adapter.on_hotkey(HotkeyInput::Pressed);
        adapter.cancel();
        adapter.on_hotkey(HotkeyInput::Released);

        assert_eq!(
            *events.borrow(),
            vec![DictationEvent::Start, DictationEvent::Cancel]
        );
        assert!(!adapter.is_dictating());
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
    fn dictation_bar_preserves_the_active_text_target_focus() {
        assert!(!dictation_bar_should_take_focus());
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
    fn hotkey_settings_store_trimmed_hotkey_and_activation_mode() {
        let mut settings = Settings::default();

        apply_hotkey_settings(
            &mut settings,
            Some("  Cmd+Shift+D  ".to_string()),
            ActivationMode::Hold,
        );

        assert_eq!(settings.hotkey, Some("Cmd+Shift+D".to_string()));
        assert_eq!(settings.activation_mode, ActivationMode::Hold);
    }

    #[test]
    fn blank_hotkey_setting_clears_configured_hotkey() {
        let mut settings = Settings {
            hotkey: Some("Cmd+Shift+D".to_string()),
            activation_mode: ActivationMode::Hold,
            ..Settings::default()
        };

        apply_hotkey_settings(
            &mut settings,
            Some("   ".to_string()),
            ActivationMode::Toggle,
        );

        assert_eq!(settings.hotkey, None);
        assert_eq!(settings.activation_mode, ActivationMode::Toggle);
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
    fn dictation_workflow_cleans_final_transcription_before_immediate_insertion() {
        let runtime = FakeAsrRuntime::new("  hello   from slugtale  ");
        let insertion = FakeTextInsertion::default();
        let rescue = FakeInsertionRescue::default();
        let workflow = DictationWorkflow::new(&runtime, &insertion, &rescue);

        let transcription = workflow
            .complete(CapturedAudio::mono_16khz(vec![0.0, 0.25]))
            .unwrap();

        assert_eq!(transcription.text, "Hello from slugtale");
        assert_eq!(
            insertion.inserted.borrow().as_slice(),
            &["Hello from slugtale"]
        );
    }

    #[test]
    fn dictation_workflow_rescues_cleaned_transcription_when_insertion_fails() {
        let runtime = FakeAsrRuntime::new("  rescue   this transcription ");
        let insertion = FakeTextInsertion::fails();
        let rescue = FakeInsertionRescue::default();
        let workflow = DictationWorkflow::new(&runtime, &insertion, &rescue);

        let transcription = workflow
            .complete(CapturedAudio::mono_16khz(vec![0.0]))
            .unwrap();

        assert_eq!(transcription.text, "Rescue this transcription");
        assert_eq!(
            rescue.rescued.borrow().as_slice(),
            &["Rescue this transcription"]
        );
    }

    #[test]
    fn clean_final_transcription_trims_and_normalizes_repeated_spaces() {
        let transcription = clean_final_transcription(FinalTranscription {
            text: "  hello   from    slugtale  ".to_string(),
        });

        assert_eq!(transcription.text, "Hello from slugtale");
    }

    #[test]
    fn clean_final_transcription_handles_empty_and_non_lowercase_starts() {
        let cases = [
            ("   ", ""),
            ("Already clean", "Already clean"),
            ("123 start recording", "123 start recording"),
            ("? question", "? question"),
        ];

        for (input, expected) in cases {
            let transcription = clean_final_transcription(FinalTranscription {
                text: input.to_string(),
            });

            assert_eq!(transcription.text, expected);
        }
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

    #[test]
    fn developer_run_app_declares_why_it_needs_microphone_access() {
        let plist = std::fs::read_to_string("Info.plist").expect("src-tauri/Info.plist exists");

        assert!(plist.contains("<key>NSMicrophoneUsageDescription</key>"));
        assert!(plist.contains("dictation"));
    }

    #[test]
    fn developer_run_app_builds_a_macos_bundle_for_privacy_identity() {
        let config = std::fs::read_to_string("tauri.conf.json").expect("tauri.conf.json exists");
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();

        assert_eq!(config["identifier"], "com.slugtale.desktop");
        assert_eq!(config["bundle"]["active"], true);
        assert_eq!(config["bundle"]["macOS"]["infoPlist"], "Info.plist");

        let package_json = std::fs::read_to_string("../package.json").expect("package.json exists");
        let package_json: serde_json::Value = serde_json::from_str(&package_json).unwrap();

        assert_eq!(package_json["scripts"]["dev"], "node scripts/run-dev.js");

        let dev_runner =
            std::fs::read_to_string("../scripts/run-dev.js").expect("dev runner exists");
        assert!(dev_runner.contains("\"--bundles\""));
        assert!(dev_runner.contains("\"app\""));
        assert!(dev_runner.contains("Slugtale.app"));
        assert!(dev_runner.contains("\"codesign\""));
        assert!(dev_runner.contains("\"--identifier\""));
        assert!(dev_runner.contains("com.slugtale.desktop"));
    }

    #[test]
    fn microphone_permission_setup_requests_access_before_opening_settings() {
        let system = FakeMicrophonePermissionSetup::default();

        run_microphone_permission_setup(&system).unwrap();

        assert_eq!(
            system.events.borrow().as_slice(),
            &["request_microphone_access", "open_microphone_settings"]
        );
    }

    #[test]
    fn text_insertion_permission_setup_opens_settings_only_when_not_trusted() {
        let system = FakeTextInsertionPermissionSetup::untrusted();

        let trusted = run_text_insertion_permission_setup(&system).unwrap();

        assert!(!trusted);
        assert_eq!(
            system.events.borrow().as_slice(),
            &[
                "request_text_insertion_access",
                "open_text_insertion_settings"
            ]
        );
    }

    #[test]
    fn text_insertion_permission_setup_does_not_reopen_settings_when_trusted() {
        let system = FakeTextInsertionPermissionSetup::trusted();

        let trusted = run_text_insertion_permission_setup(&system).unwrap();

        assert!(trusted);
        assert_eq!(
            system.events.borrow().as_slice(),
            &["request_text_insertion_access"]
        );
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

    #[derive(Default)]
    struct FakeMicrophonePermissionSetup {
        events: std::cell::RefCell<Vec<&'static str>>,
    }

    impl MicrophonePermissionSetup for FakeMicrophonePermissionSetup {
        fn request_microphone_access(&self) -> Result<(), String> {
            self.events.borrow_mut().push("request_microphone_access");
            Ok(())
        }

        fn open_microphone_settings(&self) -> Result<(), String> {
            self.events.borrow_mut().push("open_microphone_settings");
            Ok(())
        }
    }

    struct FakeTextInsertionPermissionSetup {
        events: std::cell::RefCell<Vec<&'static str>>,
        trusted: bool,
    }

    impl FakeTextInsertionPermissionSetup {
        fn trusted() -> Self {
            Self {
                events: std::cell::RefCell::new(Vec::new()),
                trusted: true,
            }
        }

        fn untrusted() -> Self {
            Self {
                events: std::cell::RefCell::new(Vec::new()),
                trusted: false,
            }
        }
    }

    impl TextInsertionPermissionSetup for FakeTextInsertionPermissionSetup {
        fn request_text_insertion_access(&self) -> Result<bool, String> {
            self.events
                .borrow_mut()
                .push("request_text_insertion_access");
            Ok(self.trusted)
        }

        fn open_text_insertion_settings(&self) -> Result<(), String> {
            self.events
                .borrow_mut()
                .push("open_text_insertion_settings");
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTextInsertion {
        inserted: std::cell::RefCell<Vec<String>>,
        fails: bool,
    }

    impl FakeTextInsertion {
        fn fails() -> Self {
            Self {
                inserted: std::cell::RefCell::new(Vec::new()),
                fails: true,
            }
        }
    }

    impl TextInsertion for FakeTextInsertion {
        fn insert(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<TextInsertionOutcome, TextInsertionError> {
            self.inserted.borrow_mut().push(transcription.text.clone());
            if self.fails {
                Err(TextInsertionError::new("fake insertion failure"))
            } else {
                Ok(TextInsertionOutcome::ClipboardFree)
            }
        }
    }

    #[derive(Default)]
    struct FakeInsertionRescue {
        rescued: std::cell::RefCell<Vec<String>>,
    }

    impl InsertionRescue for FakeInsertionRescue {
        fn rescue(
            &self,
            transcription: &FinalTranscription,
        ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
            self.rescued.borrow_mut().push(transcription.text.clone());
            Ok(InsertionRescueOutcome::CopiedToClipboardAndNotified)
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
