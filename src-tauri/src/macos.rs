use crate::{
    ClipboardInsertionRescue, FinalTranscription, InsertionRescue, InsertionRescueError,
    InsertionRescueOutcome, InsertionRescueSystem, MicrophonePermissionSetup, PlatformReadiness,
    TextInsertion, TextInsertionError, TextInsertionOutcome, TextInsertionPermissionSetup,
    TextInsertionPipeline, TextInsertionSystem,
};
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use std::ffi::c_void;
use std::io::Write;
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

pub struct MacosPlatform;

impl MacosPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacosPlatform {
    fn default() -> Self {
        Self::new()
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

pub fn request_microphone_access() -> Result<(), String> {
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
        Some(app) => app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows),
        None => false,
    }
}
