mod app_shell;
pub use app_shell::*;

mod window_label;
pub use window_label::*;

mod app_update;
pub use app_update::*;

mod diagnostics;
pub use diagnostics::*;

mod app_files;
pub use app_files::*;

mod recording_feedback;
pub use recording_feedback::*;

mod audio_capture;
pub use audio_capture::*;

mod text_insertion;
pub use text_insertion::*;

mod platform_execution;
pub use platform_execution::*;

mod json_file;

mod settings;
pub use settings::*;

mod usage;
pub use usage::*;

mod typing_baseline;
pub use typing_baseline::*;

mod dictation_bar;
pub use dictation_bar::*;

mod local_model;
pub use local_model::*;

mod permission_setup;
pub use permission_setup::*;

mod transcription_engine;
pub use transcription_engine::*;

mod engine_catalogue;
pub use engine_catalogue::*;

mod asr;
pub use asr::*;

mod parakeet;
pub use parakeet::*;
mod phonon;
pub use phonon::PhononProvider;

mod apple_speech;
pub use apple_speech::*;

mod second_opinion;
pub use second_opinion::*;

mod second_opinion_transport;

mod segmentation;
pub use segmentation::*;

mod dictation_workflow;
pub use dictation_workflow::*;

mod dictation_runtime;
pub use dictation_runtime::*;

mod dictation_host;
pub use dictation_host::*;

mod transcript_cleanup;
pub use transcript_cleanup::*;

mod readiness;
pub use readiness::*;

mod hotkey;
pub use hotkey::*;

mod global_key;
pub use global_key::*;

mod dictation_control;
pub use dictation_control::*;

mod wake_word;
pub use wake_word::*;

mod listen_loop;
pub use listen_loop::*;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{
    accessibility_trusted, activate_app, frontmost_app_pid, locale_week_start, notify,
    open_accessibility_settings, request_microphone_access, MacosInsertionRescue,
    MacosMicrophonePermissionSetup, MacosPlatform, MacosTextInsertion,
    MacosTextInsertionPermissionSetup,
};

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::{
    activate_app, frontmost_app_pid, locale_week_start, notify, open_microphone_settings,
    WindowsInsertionRescue, WindowsMicrophonePermissionSetup, WindowsPlatform,
    WindowsTextInsertion, WindowsTextInsertionPermissionSetup,
};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{
    activate_app, detect_session, frontmost_app_pid, locale_week_start, notify,
    open_microphone_settings, DisplayServerSession, LinuxInsertionRescue,
    LinuxMicrophonePermissionSetup, LinuxPlatform, LinuxTextInsertion,
    LinuxTextInsertionPermissionSetup,
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
