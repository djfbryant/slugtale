/// Tauri app-shell helpers for Slugtale's resident tray/settings surface
/// (ADR-0007, ADR-0008). Re-exported so existing `slugtale_lib::*` call sites
/// keep compiling.
mod app_shell;
pub use app_shell::*;

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

mod settings;
pub use settings::*;

mod local_model;
pub use local_model::*;

mod permission_setup;
pub use permission_setup::*;

mod asr;
pub use asr::*;

mod dictation_workflow;
pub use dictation_workflow::*;

mod readiness;
pub use readiness::*;

mod hotkey;
pub use hotkey::*;

/// macOS implementation of platform adapters (ADR-0021). Resolves OS-specific
/// dictation gates, text insertion, insertion rescue, permission setup, and
/// focused-app activation from live system state.
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{
    accessibility_trusted, activate_app, frontmost_app_pid, notify, open_accessibility_settings,
    MacosInsertionRescue, MacosMicrophonePermissionSetup, MacosPlatform, MacosTextInsertion,
    MacosTextInsertionPermissionSetup,
};

/// Windows implementation of platform adapters (ADR-0021, PRD slugtale-5pc).
/// Mirrors the macOS adapter surface so the core Dictation Workflow runs
/// unchanged on Windows. Scaffold from slugtale-5pc.1; behaviour filled by the
/// follow-on Windows issues.
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::{
    activate_app, frontmost_app_pid, notify, open_microphone_settings, WindowsInsertionRescue,
    WindowsMicrophonePermissionSetup, WindowsPlatform, WindowsTextInsertion,
    WindowsTextInsertionPermissionSetup,
};

/// Linux implementation of platform adapters (ADR-0021, ADR-0023, PRD
/// slugtale-8ul). Mirrors the macOS/Windows adapter surface so the core
/// Dictation Workflow runs unchanged on Linux. Phase 1 targets X11 (Mint
/// Cinnamon); Wayland support is phased second.
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    activate_app, detect_session, frontmost_app_pid, notify, open_microphone_settings,
    DisplayServerSession, LinuxInsertionRescue, LinuxMicrophonePermissionSetup, LinuxPlatform,
    LinuxTextInsertion, LinuxTextInsertionPermissionSetup,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_escape_press_cancels_an_active_dictation() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut adapter = HotkeyDictationAdapter::new(ActivationMode::Toggle, |event| {
            events.borrow_mut().push(event);
        });

        assert!(adapter.on_global_key(DictationKey::Hotkey, HotkeyInput::Pressed));
        assert!(!adapter.on_global_key(DictationKey::Escape, HotkeyInput::Pressed));
        assert!(!adapter.on_global_key(DictationKey::Escape, HotkeyInput::Released));

        assert_eq!(
            *events.borrow(),
            vec![DictationEvent::Start, DictationEvent::Cancel]
        );
        assert!(!adapter.is_dictating());
    }

    #[test]
    fn global_escape_press_while_idle_does_nothing() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut adapter = HotkeyDictationAdapter::new(ActivationMode::Toggle, |event| {
            events.borrow_mut().push(event);
        });

        assert!(!adapter.on_global_key(DictationKey::Escape, HotkeyInput::Pressed));

        assert!(events.borrow().is_empty());
        assert!(!adapter.is_dictating());
    }
}
