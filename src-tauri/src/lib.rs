/// Tauri app-shell helpers for Slugtale's resident tray/settings surface
/// (ADR-0007, ADR-0008). Re-exported so existing `slugtale_lib::*` call sites
/// keep compiling.
mod app_shell;
pub use app_shell::*;

mod app_update;
pub use app_update::*;

/// The Local Diagnostic Log domain (ADR-0019). Extracted into its own module;
/// re-exported so existing `slugtale_lib::*` call sites keep compiling.
mod diagnostics;
pub use diagnostics::*;

/// File locations (ADR-0018): the one module that knows where the Settings
/// File, Usage File, Local Diagnostic Log, and model directory live.
mod app_files;
pub use app_files::*;

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

mod platform_execution;
pub use platform_execution::*;

mod json_file;

mod settings;
pub use settings::*;

/// Usage (CONTEXT.md, ADR-0025): the opt-in Daily Usage Records and the Time
/// Saved derived from them. Aggregates only — no transcription, no audio, no
/// text target — which is what keeps this outside Dictation History (ADR-0002).
mod usage;
pub use usage::*;

/// The Typing Baseline and the three Typing Challenges that measure it
/// (CONTEXT.md, ADR-0025). Lives in the Settings File so it survives turning
/// Usage off.
mod typing_baseline;
pub use typing_baseline::*;

/// Dictation Bar geometry: where the bar sits on screen and which part of its
/// transparent window actually paints (slugtale-z7a).
mod dictation_bar;
pub use dictation_bar::*;

mod local_model;
pub use local_model::*;

mod permission_setup;
pub use permission_setup::*;

/// The Transcription Engine boundary (CONTEXT.md): the seam every local speech
/// recognizer sits behind, and the non-content vocabulary Settings and the
/// Second Opinion router share (slugtale-vjs).
mod transcription_engine;
pub use transcription_engine::*;

mod engine_catalogue;
pub use engine_catalogue::*;

mod asr;
pub use asr::*;

/// NVIDIA Parakeet TDT v2 0.6B as a Transcription Engine (slugtale-vjs.1).
/// Portable: the ONNX artefacts run on macOS, Windows, and Linux, so the
/// provider is compiled everywhere and only its inference is feature-gated.
mod parakeet;
pub use parakeet::*;

/// Apple SpeechTranscriber as a Transcription Engine (slugtale-vjs.2). The
/// provider type exists on every platform so Settings can explain why the
/// engine is unavailable; only the macOS build can actually transcribe.
mod apple_speech;
pub use apple_speech::*;

/// Second Opinion routing (slugtale-vjs.3): the fixed, inspectable rules that
/// decide when a second local Transcription Engine is worth asking, and which
/// of the two complete transcripts to insert.
mod second_opinion;
pub use second_opinion::*;

/// Dictation Segments and the Segment Pause that ends one (CONTEXT.md,
/// ADR-0015): the rule that decides when the speech so far is worth inserting
/// while the microphone is still running.
mod segmentation;
pub use segmentation::*;

mod dictation_workflow;
pub use dictation_workflow::*;

mod dictation_segments;
pub use dictation_segments::*;

/// The Dictation Runtime (CONTEXT.md, ADR-0026): the module that coordinates
/// ordered Dictation Segment execution — the segment channel, the single
/// worker that preserves spoken order, and the Counted Segment handoff — with
/// everything OS-touching behind the `DictationRuntimeHost` adapter.
mod dictation_runtime;
pub use dictation_runtime::*;

/// The dictation lifecycle host (slugtale-30i): everything between an
/// activation input saying "start" and the Dictation Runtime receiving the
/// captured audio. It owns the recording-feedback state machine, the focus
/// target, the audio capture session, and the runtime handle, and reaches the
/// rest of the app only through the `DictationSurface` port — implemented by
/// the Tauri shell in the binary tier and by a fake in tests.
mod dictation_host;
pub use dictation_host::*;

/// Transcript Cleanup (slugtale-m4h): the deterministic, entirely local passes
/// that run between transcription and insertion — whitespace normalization in
/// every mode, plus conservative filler-word removal when enabled.
mod transcript_cleanup;
pub use transcript_cleanup::*;

mod readiness;
pub use readiness::*;

mod hotkey;
pub use hotkey::*;

/// Bare-Escape arming: the one arbiter that owns whether Escape is global, so
/// there is never an active but uncancellable dictation and never a duplicate
/// OS registration.
mod global_key;
pub use global_key::*;

/// Dictation Control (CONTEXT.md): the begin/rollback activation policy shared
/// by the Hotkey, Voice Activation, and Dictation Bar inputs. The lifecycle's
/// transitions live in `hotkey`; this module decides when a begin request may
/// run one and how to undo it.
mod dictation_control;
pub use dictation_control::*;

/// Voice Activation spike (slugtale-e95): scoring transcripts against the wake
/// phrase and the detection state machine. Pure logic, compiled everywhere;
/// the always-on listener that feeds it is feature-gated in the Tauri tier.
mod wake_word;
pub use wake_word::*;

/// The Voice Activation listen loop (slugtale-e95): the always-listening state
/// machine — dictation suppression, engine readiness, capture retry,
/// report-once microphone problems, and the wake trigger — with every OS touch
/// behind the `WakeListener` port so all platforms test and run it unchanged.
mod listen_loop;
pub use listen_loop::*;

/// macOS implementation of platform adapters (ADR-0021). Resolves OS-specific
/// dictation gates, text insertion, insertion rescue, permission setup, and
/// focused-app activation from live system state.
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{
    accessibility_trusted, activate_app, frontmost_app_pid, locale_week_start, notify,
    open_accessibility_settings, request_microphone_access, MacosInsertionRescue,
    MacosMicrophonePermissionSetup, MacosPlatform, MacosTextInsertion,
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
    activate_app, frontmost_app_pid, locale_week_start, notify, open_microphone_settings,
    WindowsInsertionRescue, WindowsMicrophonePermissionSetup, WindowsPlatform,
    WindowsTextInsertion, WindowsTextInsertionPermissionSetup,
};

/// Linux implementation of platform adapters (ADR-0021, ADR-0023, PRD
/// slugtale-8ul). Mirrors the macOS/Windows adapter surface so the core
/// Dictation Workflow runs unchanged on Linux. Phase 1 targets X11 (Mint
/// Cinnamon); Wayland support is phased second.
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    activate_app, detect_session, frontmost_app_pid, locale_week_start, notify,
    open_microphone_settings, DisplayServerSession, LinuxInsertionRescue,
    LinuxMicrophonePermissionSetup, LinuxPlatform, LinuxTextInsertion,
    LinuxTextInsertionPermissionSetup, PortalAccelerator, PortalAcceleratorError,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_bar_uses_the_platform_work_area_with_a_small_edge_gap() {
        let work_area = MonitorGeometry {
            origin_x: -1280,
            origin_y: 24,
            width: 1280,
            height: 1000,
            scale_factor: 1.0,
        };

        let (x, y) = dictation_bar_origin(&work_area, 248, 76, BarPosition::BottomRight);

        assert_eq!(x, -1280 + 1280 - 248 - 8);
        assert_eq!(y, 24 + 1000 - 76 - 8);

        let primary_work_area = MonitorGeometry {
            origin_x: 0,
            origin_y: 0,
            width: 1440,
            height: 900,
            scale_factor: 1.0,
        };
        let (center_x, center_y) =
            dictation_bar_origin(&primary_work_area, 248, 76, BarPosition::BottomCenter);
        let (left_x, left_y) =
            dictation_bar_origin(&primary_work_area, 248, 76, BarPosition::BottomLeft);
        assert_eq!(center_x, (1440 - 248) / 2);
        assert_eq!(center_y, 900 - 76 - 8);
        assert_eq!(left_x, 8);
        assert_eq!(left_y, center_y);
        assert_eq!(900 - (center_y + 76) + BAR_GUTTER_PT as i32, 24);

        let retina_work_area = MonitorGeometry {
            width: 2880,
            height: 1800,
            scale_factor: 2.0,
            ..primary_work_area
        };
        assert_eq!(
            dictation_bar_origin(&retina_work_area, 496, 152, BarPosition::BottomLeft),
            (16, 1800 - 152 - 16)
        );
    }
}
