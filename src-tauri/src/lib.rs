use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

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
        assert!(!dev_runner.contains("\"--sign\",\n    \"-\""));
        assert!(dev_runner.contains("SLUGTALE_SIGN_IDENTITY"));
        assert!(dev_runner.contains("Slugtale Dev"));
    }
    #[test]
    fn developer_run_has_a_recovery_path_for_stale_macos_text_insertion_grants() {
        let package_json = std::fs::read_to_string("../package.json").expect("package.json exists");
        let package_json: serde_json::Value = serde_json::from_str(&package_json).unwrap();

        assert_eq!(
            package_json["scripts"]["macos:reset-permissions"],
            "node scripts/reset-dev-permissions.js"
        );

        let recovery_script = std::fs::read_to_string("../scripts/reset-dev-permissions.js")
            .expect("dev permissions recovery script exists");

        assert!(recovery_script.contains("tccutil"));
        assert!(recovery_script.contains("Accessibility"));
        assert!(recovery_script.contains("com.slugtale.desktop"));
        assert!(recovery_script.contains("--all-accessibility"));
        assert!(recovery_script.contains("npm run dev"));
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
