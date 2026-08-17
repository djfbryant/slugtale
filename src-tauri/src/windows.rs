//! Windows implementation of the Platform Adapter seams (ADR-0021, PRD
//! slugtale-5pc). This module mirrors `macos.rs`: it fills the same
//! `PlatformReadiness`, `TextInsertion`, `InsertionRescue`, permission-setup,
//! and focus-targeting seams so the core Dictation Workflow runs unchanged on
//! Windows.
//!
//! This started as the scaffold from issue slugtale-5pc.1; the follow-on
//! Windows issues have since filled every body, so the adapter surface is
//! complete:
//!
//! * 5pc.2 — `WindowsPlatform` readiness (implemented: both microphone
//!   ConsentStore toggles are read — the global one and the `NonPackaged` one
//!   that gates unpackaged desktop apps — and both must allow; insertion is
//!   effectively always granted, since Windows has no synthesized-input trust
//!   gate — the analogous failure is UIPI against an elevated target).
//! * 5pc.3 — `WindowsTextInsertionSystem` (implemented: SendInput with
//!   KEYEVENTF_UNICODE for clipboard-free insertion, and CF_UNICODETEXT +
//!   SendInput Ctrl+V for the clipboard-paste fallback). Note the Unicode path
//!   does not carry line breaks; see slugtale-z81.
//! * 5pc.4 — `WindowsInsertionRescueSystem` (implemented: CF_UNICODETEXT
//!   clipboard copy shared with the paste fallback — both take the global
//!   clipboard lock with a short retry, since ordinary contention is not an
//!   error — and a detached-thread MessageBox notification, the OQ-2 fallback
//!   for unpackaged dev-run builds, which cannot post WinRT toasts without an
//!   AUMID/shortcut).
//! * 5pc.5 — focus targeting (implemented: `frontmost_app_pid` via
//!   `GetForegroundWindow` + `GetWindowThreadProcessId`, `activate_app` via
//!   `EnumWindows` + `SetForegroundWindow`, both wired in main.rs alongside
//!   the macOS paths).
//! * 5pc.6 — audible feedback (implemented: `PlaySoundW` with system event
//!   aliases, called from the recording_feedback.rs Windows arm).
//! * 5pc.7 — permission setup (implemented: mic deep-links to
//!   `ms-settings:privacy-microphone` from the setup workflow's open phase,
//!   since unpackaged apps have no prompt API; insertion permission is
//!   trivially granted per OQ-1 — there is no gate, so request returns true
//!   and open-settings is a documented no-op).
//!
//! The adapter is selected from `CurrentPlatform` in `main.rs`, which also
//! wires permission commands and the transcription insertion/rescue workflow
//! on Windows (5pc.11).

use crate::{
    ClipboardInsertionRescue, DictationSound, FinalTranscription, InsertionRescue,
    InsertionRescueError, InsertionRescueOutcome, InsertionRescueSystem, MicrophonePermissionSetup,
    PlatformReadiness, TextInsertion, TextInsertionError, TextInsertionOutcome,
    TextInsertionPermissionSetup, TextInsertionPipeline, TextInsertionSystem, WeekStart,
};
use std::ptr;
use windows_sys::Win32::Foundation::{GlobalFree, BOOL, ERROR_SUCCESS, HANDLE, HWND, LPARAM};
use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK};
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VK_CONTROL, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
    SW_RESTORE,
};

const MICROPHONE_CONSENT_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
/// The "let desktop apps access your microphone" toggle, which is a *separate*
/// switch from the global one above. Slugtale is an unpackaged Win32 app, so
/// the global key alone can read `Allow` while desktop apps are blocked, and
/// readiness has to consult this one too.
///
/// Not the whole story: Windows also records *per-app* consent under this key,
/// in a subkey named for the executable's path. A user who allows desktop apps
/// broadly but denies Slugtale specifically still reads `Allow` from both
/// values here. Reading the per-app entry needs the exact path-mangling scheme
/// confirmed on a real machine, so it is tracked separately (slugtale-q7a) and
/// folded into the 5pc.9 hardware validation rather than guessed at.
const MICROPHONE_CONSENT_NON_PACKAGED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone\NonPackaged";

const MICROPHONE_CONSENT_VALUE: &str = "Value";
const MICROPHONE_ALLOWED: &str = "Allow";

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
        microphone_access_granted(
            microphone_consent_value().ok().as_deref(),
            non_packaged_microphone_consent_value().ok().as_deref(),
        )
    }

    fn insertion_granted(&self) -> bool {
        // Windows has no Accessibility-equivalent trust gate for synthesized
        // input. Elevated targets can still reject input through UIPI, but that
        // is a delivery failure for the insertion pipeline/rescue rather than a
        // permission-readiness failure.
        true
    }
}

fn microphone_consent_value() -> Result<String, String> {
    read_hkcu_string(MICROPHONE_CONSENT_KEY, MICROPHONE_CONSENT_VALUE)
}

fn non_packaged_microphone_consent_value() -> Result<String, String> {
    read_hkcu_string(
        MICROPHONE_CONSENT_NON_PACKAGED_KEY,
        MICROPHONE_CONSENT_VALUE,
    )
}

fn microphone_consent_allows(value: &str) -> bool {
    value.eq_ignore_ascii_case(MICROPHONE_ALLOWED)
}

/// Decide microphone readiness from the two ConsentStore toggles that gate an
/// unpackaged desktop app. Kept free of Win32 so the policy is unit-testable and
/// the registry reads stay in the thin wrappers above (ADR-0021).
///
/// Both toggles must allow access. `non_packaged` is `None` on Windows builds
/// predating the split desktop-apps switch, where the global value is the whole
/// answer; treating an absent key as a denial there would report every such
/// machine as never-ready.
fn microphone_access_granted(global: Option<&str>, non_packaged: Option<&str>) -> bool {
    let global_allows = global.is_some_and(microphone_consent_allows);
    let non_packaged_allows = non_packaged.map(microphone_consent_allows).unwrap_or(true);
    global_allows && non_packaged_allows
}

fn read_hkcu_string(subkey: &str, value_name: &str) -> Result<String, String> {
    // Kept before the wide-encoding shadows them, so a failure names the key it
    // actually tried to read rather than a hardcoded guess.
    let key_description = format!("HKCU\\{subkey}\\{value_name}");
    let subkey = wide_null(subkey);
    let value_name = wide_null(value_name);
    let mut byte_len = 0u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut byte_len,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(format!(
            "could not read {key_description}: RegGetValueW returned {status}"
        ));
    }

    if byte_len == 0 {
        return Ok(String::new());
    }

    let mut buffer = vec![0u16; byte_len.div_ceil(2) as usize];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value_name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut byte_len,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(format!(
            "could not read {key_description}: RegGetValueW returned {status}"
        ));
    }

    let unit_len = (byte_len / 2) as usize;
    let value_units = &buffer[..unit_len.min(buffer.len())];
    let value_units = value_units
        .split(|unit| *unit == 0)
        .next()
        .unwrap_or(value_units);
    String::from_utf16(value_units)
        .map_err(|error| format!("could not decode {key_description}: {error}"))
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
    fn insert_clipboard_free(&self, text: &str) -> Result<(), TextInsertionError> {
        // The direct analog of the macOS CGEventKeyboardSetUnicodeString path:
        // inject each UTF-16 code unit as a synthesized key press with
        // KEYEVENTF_UNICODE. SendInput can be silently blocked by UIPI when the
        // foreground target runs at a higher integrity level; reporting the
        // short insert as an error lets the pipeline fall through to the
        // clipboard paste and, failing that, the rescue.
        send_unicode_text(text).map_err(TextInsertionError::new)
    }

    fn insert_from_clipboard(&self, text: &str) -> Result<(), TextInsertionError> {
        // Set CF_UNICODETEXT then synthesize Ctrl+V. The paste keystroke is also
        // a synthesized event, so the same UIPI failure mode falls through to the
        // rescue. The brief pause mirrors macos.rs: it lets the clipboard settle
        // before the target receives the paste.
        set_clipboard_text(text).map_err(TextInsertionError::new)?;
        std::thread::sleep(std::time::Duration::from_millis(30));
        send_paste_shortcut().map_err(TextInsertionError::new)
    }
}

/// One synthesized keyboard event carrying a single UTF-16 code unit
/// (`KEYEVENTF_UNICODE`), for the key-down (`key_up == false`) or key-up half.
fn unicode_key_event(code_unit: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    keyboard_input(0, code_unit, flags)
}

/// One synthesized virtual-key event (e.g. Ctrl, V), for the key-down or key-up
/// half.
fn virtual_key_event(virtual_key: u16, key_up: bool) -> INPUT {
    let flags = if key_up { KEYEVENTF_KEYUP } else { 0 };
    keyboard_input(virtual_key, 0, flags)
}

fn keyboard_input(virtual_key: u16, scan_code: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: scan_code,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Inject a batch of synthesized keyboard events in one `SendInput` call.
/// `SendInput` returns the count actually inserted; a short count means the
/// events were blocked (typically UIPI against a higher-integrity target), which
/// we surface as an error so the caller can fall back.
fn send_keyboard_events(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput injected {sent} of {} keyboard events; input may be blocked (UIPI)",
            inputs.len()
        ))
    }
}

/// Type `text` into the focused control by synthesizing a Unicode key press per
/// UTF-16 code unit. Surrogate pairs are sent as two consecutive units, as
/// Windows expects.
fn send_unicode_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        inputs.push(unicode_key_event(unit, false));
        inputs.push(unicode_key_event(unit, true));
    }
    send_keyboard_events(&inputs)
}

/// Synthesize Ctrl+V to paste the current clipboard contents into the focused
/// control.
fn send_paste_shortcut() -> Result<(), String> {
    let inputs = [
        virtual_key_event(VK_CONTROL, false),
        virtual_key_event(VK_V, false),
        virtual_key_event(VK_V, true),
        virtual_key_event(VK_CONTROL, true),
    ];
    send_keyboard_events(&inputs)
}

/// How many times to reach for the clipboard lock, and how long to wait between
/// tries. The Windows clipboard is a single global lock that any running app can
/// hold, so a first-attempt failure is ordinary contention rather than a real
/// error — clipboard managers and browsers grab it constantly. macOS has no
/// equivalent failure mode, which is why the `pbcopy` path this mirrors needs no
/// retry. Ten tries at 10ms bounds the wait at ~100ms, far below the pause a
/// user would notice on the insertion-rescue path.
const CLIPBOARD_OPEN_ATTEMPTS: u32 = 10;
const CLIPBOARD_OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

/// Take the global clipboard lock, retrying briefly while another app holds it.
fn open_clipboard_with_retry() -> Result<(), String> {
    for attempt in 1..=CLIPBOARD_OPEN_ATTEMPTS {
        if unsafe { OpenClipboard(ptr::null_mut::<std::ffi::c_void>() as HWND) } != 0 {
            return Ok(());
        }
        if attempt < CLIPBOARD_OPEN_ATTEMPTS {
            std::thread::sleep(CLIPBOARD_OPEN_RETRY_DELAY);
        }
    }
    Err(format!(
        "could not open the Windows clipboard after {CLIPBOARD_OPEN_ATTEMPTS} attempts; \
         another app is holding it"
    ))
}

/// Place `text` on the clipboard as `CF_UNICODETEXT`. On success the system owns
/// the moveable global memory; on failure it is freed here. Shared with the
/// insertion rescue path (5pc.4).
fn set_clipboard_text(text: &str) -> Result<(), String> {
    // UTF-16 with the terminating NUL that CF_UNICODETEXT requires.
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = units.len() * std::mem::size_of::<u16>();

    open_clipboard_with_retry()?;

    let result = (|| {
        if unsafe { EmptyClipboard() } == 0 {
            return Err("could not empty the Windows clipboard".to_string());
        }

        let hmem = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) };
        if hmem.is_null() {
            return Err("could not allocate clipboard memory".to_string());
        }

        let locked = unsafe { GlobalLock(hmem) };
        if locked.is_null() {
            unsafe { GlobalFree(hmem) };
            return Err("could not lock clipboard memory".to_string());
        }
        unsafe {
            ptr::copy_nonoverlapping(units.as_ptr(), locked.cast::<u16>(), units.len());
            GlobalUnlock(hmem);
        }

        // On success SetClipboardData takes ownership of hmem; only free it when
        // the call fails so we do not leak the block.
        if unsafe { SetClipboardData(CF_UNICODETEXT as u32, hmem as HANDLE) }.is_null() {
            unsafe { GlobalFree(hmem) };
            return Err("could not set clipboard text".to_string());
        }
        Ok(())
    })();

    unsafe { CloseClipboard() };
    result
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
    fn copy_to_clipboard(&self, text: &str) -> Result<(), InsertionRescueError> {
        set_clipboard_text(text).map_err(InsertionRescueError::new)
    }

    fn notify_user(&self, title: &str, body: &str) -> Result<(), InsertionRescueError> {
        notify_user(title, body).map_err(InsertionRescueError::new)
    }
}

/// Show a user-facing notification via `MessageBoxW` on a detached thread.
///
/// This is the OQ-2 fallback: unpackaged dev-run builds cannot post WinRT
/// toasts without an AUMID/shortcut, and a message box needs no registration.
/// The box is modal only to its own throwaway thread, so the dictation
/// workflow never blocks on the user dismissing it. Unlike the macOS
/// osascript path, `Ok` here means the box was dispatched, not shown — a
/// `MessageBoxW` failure after detach can only be logged. `MB_SETFOREGROUND`
/// does pull focus from the paste target, but a tray-resident app has no
/// other guaranteed-visible surface for the rescue alert.
fn notify_user(title: &str, body: &str) -> Result<(), String> {
    let title = wide_null(title);
    let body = wide_null(body);
    std::thread::Builder::new()
        .name("slugtale-notify".to_string())
        .spawn(move || {
            let shown = unsafe {
                MessageBoxW(
                    ptr::null_mut(),
                    body.as_ptr(),
                    title.as_ptr(),
                    MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
                )
            };
            if shown == 0 {
                eprintln!("slugtale: MessageBoxW failed to show the insertion-rescue notification");
            }
        })
        .map_err(|error| format!("could not show notification: {error}"))?;
    Ok(())
}

/// Play the audible dictation cue through winmm using system event aliases, so
/// v1 ships no bundled audio assets (the Windows analog of the afplay arm in
/// recording_feedback.rs). SND_ASYNC returns immediately so the recording
/// lifecycle never blocks on audio.
pub(crate) fn play_dictation_sound(sound: DictationSound) -> std::io::Result<()> {
    // Asterisk is the softer "information" chime for the start edge; Default is
    // the plain system ding for the stop edge — the closest asset-free pairing
    // to the macOS Tink/Pop cues.
    let alias = match sound {
        DictationSound::Start => "SystemAsterisk",
        DictationSound::Stop => "SystemDefault",
    };
    let alias = wide_null(alias);
    let played = unsafe { PlaySoundW(alias.as_ptr(), ptr::null_mut(), SND_ALIAS | SND_ASYNC) };
    if played == 0 {
        return Err(std::io::Error::other(
            "PlaySoundW could not play the system sound",
        ));
    }
    Ok(())
}

pub struct WindowsMicrophonePermissionSetup;

impl MicrophonePermissionSetup for WindowsMicrophonePermissionSetup {
    fn request_microphone_access(&self) -> Result<(), String> {
        // Windows has no per-process prompt API for unpackaged desktop apps —
        // mic consent is the global ConsentStore toggle read by readiness.
        // `run_microphone_permission_setup` invokes the open phase next, which
        // performs the single Settings deep link; launching it here too would
        // open the page twice.
        Ok(())
    }

    fn open_microphone_settings(&self) -> Result<(), String> {
        open_microphone_settings()
    }
}

pub struct WindowsTextInsertionPermissionSetup;

impl TextInsertionPermissionSetup for WindowsTextInsertionPermissionSetup {
    fn request_text_insertion_access(&self) -> Result<bool, String> {
        // OQ-1: Windows has no accessibility-trust gate — synthesized input is
        // always permitted (UIPI against elevated targets is a delivery
        // failure, not a permission), so the request trivially succeeds.
        Ok(true)
    }

    fn open_text_insertion_settings(&self) -> Result<(), String> {
        // OQ-1: no settings page exists for a gate that does not exist. This is
        // an intentional no-op; readiness always reports insertion granted, so
        // the settings UI never needs to route users here (UI copy is
        // reconciled in 5pc.12).
        Ok(())
    }
}

/// Deep-link to the Windows microphone privacy page, where the global "let
/// desktop apps access your microphone" toggle lives.
pub fn open_microphone_settings() -> Result<(), String> {
    open_settings_uri("ms-settings:privacy-microphone")
}

/// Launch an `ms-settings:` URI. `explorer` resolves the URI through the shell
/// without flashing a console window, following the `open_path` precedent in
/// local_model.rs; the spawned helper returns immediately.
fn open_settings_uri(uri: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(uri)
        .spawn()
        .map_err(|error| format!("could not open Windows Settings: {error}"))?;
    Ok(())
}

/// The process id of the foreground window's owning app — captured at record
/// start so insertion can re-target it (parallels the macOS `frontmost_app_pid`).
pub fn frontmost_app_pid() -> Option<i32> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    (pid != 0).then(|| pid as i32)
}

struct ActivateTarget {
    pid: u32,
    activated: bool,
}

/// Bring the app with `pid` back to the foreground before synthesized input.
/// The pid-based seam matches macOS; the pid's window is re-found here because
/// an HWND captured at record start could die while the user dictates. Like
/// the macOS `activateWithOptions` path this works at app granularity — for a
/// multi-window app it foregrounds the process's topmost visible window, not
/// necessarily the exact window that had focus at record start.
///
/// Foreground-lock (PRD risk R3): the hotkey press grants Slugtale
/// foreground-set rights, but transcription can take seconds and the right can
/// lapse if the user clicks elsewhere meanwhile. A failed activation returns
/// false and the caller proceeds; a misdirected insert then falls through the
/// pipeline to the clipboard rescue like any other delivery failure.
pub fn activate_app(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    let mut target = ActivateTarget {
        pid: pid as u32,
        activated: false,
    };
    unsafe {
        EnumWindows(
            Some(activate_first_visible_window),
            &mut target as *mut ActivateTarget as LPARAM,
        );
    }
    target.activated
}

/// `EnumWindows` callback: activate the first visible top-level window owned by
/// the target pid, restoring it first if minimized. Returns FALSE to stop
/// enumerating once that window is found, whether or not activation succeeded —
/// one attempt, like the macOS `activateWithOptions` call.
unsafe extern "system" fn activate_first_visible_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let target = unsafe { &mut *(lparam as *mut ActivateTarget) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != target.pid || unsafe { IsWindowVisible(hwnd) } == 0 {
        return 1;
    }
    if unsafe { IsIconic(hwnd) } != 0 {
        unsafe { ShowWindow(hwnd, SW_RESTORE) };
    }
    target.activated = unsafe { SetForegroundWindow(hwnd) } != 0;
    0
}

/// Show a user-facing notification. Exposed for the Tauri layer; parallels the
/// macOS `notify` free function.
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    notify_user(title, body)
}

/// Which day the user's Windows locale calls the first of the week, for the
/// Usage pane's "this week" (ADR-0025).
///
/// `LOCALE_IFIRSTDAYOFWEEK` counts from Monday = 0, unlike Cocoa's Sunday = 1,
/// so the two platform adapters cannot share a mapping. It is returned as a
/// decimal string, which is why this asks for the text and parses it rather
/// than using the `LOCALE_RETURN_NUMBER` form.
pub fn locale_week_start() -> WeekStart {
    let mut buffer = [0u16; 8];
    // A null locale name is Win32's LOCALE_NAME_USER_DEFAULT.
    let written = unsafe {
        GetLocaleInfoEx(
            ptr::null::<u16>() as PCWSTR,
            LOCALE_IFIRSTDAYOFWEEK,
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    if written <= 0 {
        return WeekStart::default();
    }

    // The count includes the terminating null.
    let text = String::from_utf16_lossy(&buffer[..(written as usize).saturating_sub(1)]);
    week_start_from_first_day_of_week(text.trim().parse::<i64>().ok())
}

/// The mapping from Windows' Monday-based weekday to the two weeks Usage
/// supports. Split out from the framework call so it can be tested without a
/// live locale.
fn week_start_from_first_day_of_week(first_day: Option<i64>) -> WeekStart {
    match first_day {
        Some(6) => WeekStart::Sunday,
        _ => WeekStart::Monday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sunday_start_locale_is_the_only_thing_that_moves_the_week_off_monday() {
        // Windows counts from Monday = 0, so Sunday is 6 — the opposite end from
        // Cocoa's Sunday = 1. Getting this backwards would put a US user's week
        // a day out, so it is worth pinning.
        assert_eq!(week_start_from_first_day_of_week(Some(6)), WeekStart::Sunday);
        assert_eq!(week_start_from_first_day_of_week(Some(0)), WeekStart::Monday);
        assert_eq!(week_start_from_first_day_of_week(Some(5)), WeekStart::Monday);
        // A locale that will not answer must not silently become Sunday.
        assert_eq!(week_start_from_first_day_of_week(None), WeekStart::Monday);
    }

    #[test]
    fn the_live_locale_answers_with_one_of_the_two_supported_weeks() {
        assert!(matches!(
            locale_week_start(),
            WeekStart::Sunday | WeekStart::Monday
        ));
    }

    #[test]
    fn microphone_is_granted_when_both_consent_toggles_allow() {
        assert!(microphone_access_granted(Some("Allow"), Some("Allow")));
    }

    #[test]
    fn blocking_desktop_apps_denies_the_microphone_even_when_the_global_toggle_allows() {
        // The case the global-only read got wrong: readiness reported ready
        // while WASAPI capture would return nothing.
        assert!(!microphone_access_granted(Some("Allow"), Some("Deny")));
    }

    #[test]
    fn blocking_the_global_toggle_denies_the_microphone() {
        assert!(!microphone_access_granted(Some("Deny"), Some("Allow")));
    }

    #[test]
    fn an_absent_desktop_apps_toggle_falls_back_to_the_global_answer() {
        // Windows builds predating the split switch have no NonPackaged key;
        // treating that as a denial would report them as never-ready.
        assert!(microphone_access_granted(Some("Allow"), None));
        assert!(!microphone_access_granted(Some("Deny"), None));
    }

    #[test]
    fn an_unreadable_global_toggle_denies_the_microphone() {
        assert!(!microphone_access_granted(None, Some("Allow")));
        assert!(!microphone_access_granted(None, None));
    }

    #[test]
    fn consent_values_are_read_case_insensitively() {
        assert!(microphone_access_granted(Some("allow"), Some("ALLOW")));
    }

    #[test]
    fn an_unrecognised_consent_value_is_not_an_allowance() {
        assert!(!microphone_access_granted(Some(""), Some("Allow")));
        assert!(!microphone_access_granted(Some("Prompt"), Some("Allow")));
    }
}
