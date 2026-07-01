//! Windows implementation of the Platform Adapter seams (ADR-0021, PRD
//! slugtale-5pc). This module mirrors `macos.rs`: it fills the same
//! `PlatformReadiness`, `TextInsertion`, `InsertionRescue`, permission-setup,
//! and focus-targeting seams so the core Dictation Workflow runs unchanged on
//! Windows.
//!
//! This is the scaffold from issue slugtale-5pc.1: the trait impls and free
//! functions exist and compile, but their bodies are `todo!()` placeholders the
//! follow-on Windows issues replace with real Win32 behaviour:
//!
//! * 5pc.2 — `WindowsPlatform` readiness (mic ConsentStore read; insertion is
//!   effectively always granted, since Windows has no synthesized-input trust
//!   gate — the analogous failure is UIPI against an elevated target).
//! * 5pc.3 — `WindowsTextInsertionSystem` (SendInput Unicode + Ctrl+V paste).
//! * 5pc.4 — `WindowsInsertionRescueSystem` (clipboard copy + notification).
//! * 5pc.5 — focus targeting (`frontmost_app_pid`/`activate_app` via
//!   `GetForegroundWindow`/`SetForegroundWindow`) and its wiring in main.rs.
//! * 5pc.6 — audible feedback (not a trait seam; `PlaySoundW`).
//! * 5pc.7 — permission setup (`ms-settings:` deep link).
//!
//! Nothing here is wired into `main.rs` yet; issue 5pc.11 selects this adapter
//! from `CurrentPlatform`.

use crate::{
    ClipboardInsertionRescue, FinalTranscription, InsertionRescue, InsertionRescueError,
    InsertionRescueOutcome, InsertionRescueSystem, MicrophonePermissionSetup, PlatformReadiness,
    TextInsertion, TextInsertionError, TextInsertionOutcome, TextInsertionPermissionSetup,
    TextInsertionPipeline, TextInsertionSystem,
};

const UNIMPLEMENTED: &str = "Windows Platform Adapter is not implemented yet (PRD slugtale-5pc)";

pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformReadiness for WindowsPlatform {
    fn microphone_granted(&self) -> bool {
        // 5pc.2: read HKCU\...\ConsentStore\microphone\Value.
        todo!("{UNIMPLEMENTED}: microphone_granted")
    }

    fn insertion_granted(&self) -> bool {
        // 5pc.2: Windows has no synthesized-input permission gate, so this is
        // effectively always true (UIPI against elevated targets is the
        // analogous silent-drop path, handled by the clipboard rescue).
        todo!("{UNIMPLEMENTED}: insertion_granted")
    }
}

pub struct WindowsTextInsertion {
    pipeline: TextInsertionPipeline<WindowsTextInsertionSystem>,
}

impl Default for WindowsTextInsertion {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsTextInsertion {
    pub fn new() -> Self {
        Self {
            pipeline: TextInsertionPipeline::new(WindowsTextInsertionSystem),
        }
    }
}

impl TextInsertion for WindowsTextInsertion {
    fn insert(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<TextInsertionOutcome, TextInsertionError> {
        self.pipeline.insert(transcription)
    }
}

struct WindowsTextInsertionSystem;

impl TextInsertionSystem for WindowsTextInsertionSystem {
    fn insert_clipboard_free(&self, _text: &str) -> Result<(), TextInsertionError> {
        // 5pc.3: SendInput with KEYEVENTF_UNICODE over the UTF-16 code units.
        todo!("{UNIMPLEMENTED}: insert_clipboard_free")
    }

    fn insert_from_clipboard(&self, _text: &str) -> Result<(), TextInsertionError> {
        // 5pc.3: set CF_UNICODETEXT then SendInput Ctrl+V.
        todo!("{UNIMPLEMENTED}: insert_from_clipboard")
    }
}

pub struct WindowsInsertionRescue {
    rescue: ClipboardInsertionRescue<WindowsInsertionRescueSystem>,
}

impl Default for WindowsInsertionRescue {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsInsertionRescue {
    pub fn new() -> Self {
        Self {
            rescue: ClipboardInsertionRescue::new(WindowsInsertionRescueSystem),
        }
    }
}

impl InsertionRescue for WindowsInsertionRescue {
    fn rescue(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
        self.rescue.rescue(transcription)
    }
}

struct WindowsInsertionRescueSystem;

impl InsertionRescueSystem for WindowsInsertionRescueSystem {
    fn copy_to_clipboard(&self, _text: &str) -> Result<(), InsertionRescueError> {
        // 5pc.4: set CF_UNICODETEXT on the clipboard.
        todo!("{UNIMPLEMENTED}: copy_to_clipboard")
    }

    fn notify_user(&self, _title: &str, _body: &str) -> Result<(), InsertionRescueError> {
        // 5pc.4: WinRT toast (with a balloon/MessageBox fallback for dev-run).
        todo!("{UNIMPLEMENTED}: notify_user")
    }
}

pub struct WindowsMicrophonePermissionSetup;

impl MicrophonePermissionSetup for WindowsMicrophonePermissionSetup {
    fn request_microphone_access(&self) -> Result<(), String> {
        // 5pc.7: no in-app prompt on Windows; deep-link to Settings instead.
        todo!("{UNIMPLEMENTED}: request_microphone_access")
    }

    fn open_microphone_settings(&self) -> Result<(), String> {
        open_microphone_settings()
    }
}

pub struct WindowsTextInsertionPermissionSetup;

impl TextInsertionPermissionSetup for WindowsTextInsertionPermissionSetup {
    fn request_text_insertion_access(&self) -> Result<bool, String> {
        // 5pc.7 / OQ-1: Windows has no insertion-trust gate to request.
        todo!("{UNIMPLEMENTED}: request_text_insertion_access")
    }

    fn open_text_insertion_settings(&self) -> Result<(), String> {
        // 5pc.7 / OQ-1: no accessibility-settings deep link exists on Windows.
        todo!("{UNIMPLEMENTED}: open_text_insertion_settings")
    }
}

/// Deep-link to the Windows microphone privacy page. 5pc.7 launches
/// `ms-settings:privacy-microphone`.
pub fn open_microphone_settings() -> Result<(), String> {
    todo!("{UNIMPLEMENTED}: open_microphone_settings")
}

/// The process id of the foreground window's owning app — captured at record
/// start so insertion can re-target it (parallels the macOS `frontmost_app_pid`).
/// 5pc.5 implements this via `GetForegroundWindow` + `GetWindowThreadProcessId`.
pub fn frontmost_app_pid() -> Option<i32> {
    todo!("{UNIMPLEMENTED}: frontmost_app_pid")
}

/// Bring the app with `pid` back to the foreground before synthesized input.
/// 5pc.5 implements this via `SetForegroundWindow`.
pub fn activate_app(_pid: i32) -> bool {
    todo!("{UNIMPLEMENTED}: activate_app")
}

/// Show a user-facing notification. Exposed for the Tauri layer; 5pc.4 provides
/// the real toast/fallback implementation.
pub fn notify(_title: &str, _body: &str) -> Result<(), String> {
    todo!("{UNIMPLEMENTED}: notify")
}
