//! Linux implementation of the Platform Adapter seams (ADR-0021, ADR-0023, PRD
//! slugtale-8ul). This module mirrors `macos.rs` and `windows.rs`: it fills the
//! same `PlatformReadiness`, `TextInsertion`, `InsertionRescue`,
//! permission-setup, and focus-targeting seams so the core Dictation Workflow
//! runs unchanged on Linux.
//!
//! Phase 1 (this module) targets X11, validated on Linux Mint Cinnamon
//! (slugtale-8ul.1–.6, .11). Every seam maps to an X11 mechanism:
//!
//! * 8ul.2 — `LinuxPlatform` readiness. Non-sandboxed Linux apps have no OS
//!   microphone or input-synthesis permission gate, so `microphone_granted()`
//!   reports whether cpal can see an input device and `insertion_granted()` is
//!   true on X11. A Wayland session is detected and reported as unsupported
//!   until Phase 2 lands.
//! * 8ul.3 — `LinuxTextInsertionSystem` via the enigo crate: clipboard-free
//!   insertion through `text()` (XTEST + keymap remapping) and a
//!   clipboard-paste fallback (arboard write + synthesized Ctrl+V).
//! * 8ul.4 — `LinuxInsertionRescueSystem`: arboard clipboard copy (held on a
//!   detached thread so the X11 selection survives — PRD risk R3) and a
//!   notify-rust D-Bus desktop notification.
//! * 8ul.5 — focus targeting: `frontmost_app_pid` reads `_NET_ACTIVE_WINDOW`
//!   and `activate_app` re-activates that window via an EWMH client message.
//!   The seam's unit is an X11 window id carried through the pid-shaped API.
//! * 8ul.6 — audible feedback: the XDG sound theme via `canberra-gtk-play`,
//!   matching the afplay no-bundled-assets precedent.
//!
//! The adapter is selected from `CurrentPlatform` in `main.rs`, which also
//! wires permission commands and the transcription insertion/rescue workflow on
//! Linux (8ul.11).

use crate::{
    ClipboardInsertionRescue, DictationSound, FinalTranscription, InsertionRescue,
    InsertionRescueError, InsertionRescueOutcome, InsertionRescueSystem, MicrophonePermissionSetup,
    PlatformReadiness, TextInsertion, TextInsertionError, TextInsertionOutcome,
    TextInsertionPermissionSetup, TextInsertionPipeline, TextInsertionSystem, WeekStart,
};
use cpal::traits::HostTrait;

/// Which display server the current session runs (ADR-0023). Phase 1 supports
/// X11; a Wayland or unknown session is reported as not-yet-supported so the
/// readiness copy can steer the user to an X11 session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServerSession {
    X11,
    Wayland,
    Unknown,
}

impl DisplayServerSession {
    /// Whether Slugtale's X11 Platform Adapter can drive this session. Only X11
    /// is supported in Phase 1; Wayland text synthesis and hotkeys arrive in
    /// Phase 2 (slugtale-8ul.12/.13).
    pub fn is_supported(self) -> bool {
        matches!(self, DisplayServerSession::X11)
    }
}

/// Classify the session from the environment: `XDG_SESSION_TYPE` is the
/// authoritative signal on Mint/systemd; `WAYLAND_DISPLAY` and `DISPLAY` are the
/// fallbacks when it is unset (e.g. a bare login shell). Split from
/// [`detect_session`] so the parsing is unit-testable without touching the
/// process environment.
fn session_from_env(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    x_display: Option<&str>,
) -> DisplayServerSession {
    if let Some(session_type) = session_type {
        match session_type.trim().to_ascii_lowercase().as_str() {
            "x11" => return DisplayServerSession::X11,
            "wayland" => return DisplayServerSession::Wayland,
            _ => {}
        }
    }

    if wayland_display.is_some_and(|value| !value.is_empty()) {
        return DisplayServerSession::Wayland;
    }
    if x_display.is_some_and(|value| !value.is_empty()) {
        return DisplayServerSession::X11;
    }

    DisplayServerSession::Unknown
}

/// The display server backing the current session, read from the environment.
pub fn detect_session() -> DisplayServerSession {
    let env = |key: &str| std::env::var(key).ok();
    session_from_env(
        env("XDG_SESSION_TYPE").as_deref(),
        env("WAYLAND_DISPLAY").as_deref(),
        env("DISPLAY").as_deref(),
    )
}

pub struct LinuxPlatform {
    session: DisplayServerSession,
}

impl LinuxPlatform {
    pub fn new() -> Self {
        Self {
            session: detect_session(),
        }
    }

    /// The display server backing this session, so the readiness UI can explain
    /// a Wayland session is not yet supported (ADR-0023).
    pub fn session(&self) -> DisplayServerSession {
        self.session
    }
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformReadiness for LinuxPlatform {
    fn microphone_granted(&self) -> bool {
        // Non-sandboxed Linux apps have no per-process microphone consent gate,
        // so "granted" means an input device is actually present. A real capture
        // failure still surfaces through the audio-capture path, not here.
        input_device_present()
    }

    fn insertion_granted(&self) -> bool {
        // X11 permits synthesized input with no trust gate. On Wayland,
        // synthesized input needs a portal grant Slugtale does not yet request
        // (Phase 2), so insertion is reported as not available there.
        self.session.is_supported()
    }
}

/// Whether cpal can see any input device — the Linux stand-in for microphone
/// permission (there is no OS consent gate to read). Prefers the default device
/// (the common case) and falls back to enumerating all input devices, so a host
/// with capture hardware but no configured default still reports granted.
fn input_device_present() -> bool {
    let host = cpal::default_host();
    if host.default_input_device().is_some() {
        return true;
    }
    host.input_devices()
        .map(|mut devices| devices.next().is_some())
        .unwrap_or(false)
}

pub struct LinuxTextInsertion {
    pipeline: TextInsertionPipeline<LinuxTextInsertionSystem>,
}

impl Default for LinuxTextInsertion {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxTextInsertion {
    pub fn new() -> Self {
        Self {
            pipeline: TextInsertionPipeline::new(LinuxTextInsertionSystem),
        }
    }
}

impl TextInsertion for LinuxTextInsertion {
    fn insert(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<TextInsertionOutcome, TextInsertionError> {
        self.pipeline.insert(transcription)
    }
}

struct LinuxTextInsertionSystem;

impl TextInsertionSystem for LinuxTextInsertionSystem {
    fn insert_clipboard_free(&self, text: &str) -> Result<(), TextInsertionError> {
        // The analog of the macOS CGEventKeyboardSetUnicodeString path: enigo's
        // `text` synthesizes the string through XTEST, remapping an unused
        // keycode for glyphs the current layout cannot produce (ADR-0009, PRD
        // risk R1). A failure here falls through to the clipboard paste.
        enigo_type_text(text).map_err(TextInsertionError::new)
    }

    fn insert_from_clipboard(&self, text: &str) -> Result<(), TextInsertionError> {
        // Put the text on the clipboard, then synthesize Ctrl+V. The clipboard
        // instance is held alive across the paste so our process can still serve
        // the X11 selection when the target app requests it (X11 selection
        // ownership; see the rescue path for the resident-hold variant).
        paste_via_clipboard(text).map_err(TextInsertionError::new)
    }
}

/// A fresh enigo handle on the X11 (XTEST) backend, or a formatted error.
fn new_enigo() -> Result<enigo::Enigo, String> {
    use enigo::{Enigo, Settings};
    Enigo::new(&Settings::default()).map_err(|error| format!("enigo init failed: {error}"))
}

/// Type `text` into the focused control by synthesizing keystrokes with enigo's
/// X11 (XTEST) backend.
fn enigo_type_text(text: &str) -> Result<(), String> {
    use enigo::Keyboard;

    if text.is_empty() {
        return Ok(());
    }
    let mut enigo = new_enigo()?;
    enigo
        .text(text)
        .map_err(|error| format!("enigo text synthesis failed: {error}"))
}

/// Place `text` on the clipboard and paste it with a synthesized Ctrl+V. The
/// `Clipboard` is kept alive across the paste and a short settle delay so this
/// process still owns and can serve the X11 selection when the target requests
/// it; dropping it earlier would abandon the selection before the paste lands.
fn paste_via_clipboard(text: &str) -> Result<(), String> {
    use arboard::Clipboard;

    let mut clipboard =
        Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text.to_owned())
        .map_err(|error| format!("could not set clipboard text: {error}"))?;

    // Let the clipboard ownership settle before the paste, mirroring the macOS
    // and Windows fallbacks.
    std::thread::sleep(std::time::Duration::from_millis(30));
    synthesize_paste()?;
    // Keep serving the selection briefly so the target app can read it before
    // `clipboard` is dropped and ownership is released.
    std::thread::sleep(std::time::Duration::from_millis(80));
    drop(clipboard);
    Ok(())
}

/// Synthesize Ctrl+V through enigo to paste the current clipboard contents.
fn synthesize_paste() -> Result<(), String> {
    use enigo::{Direction, Key, Keyboard};

    let mut enigo = new_enigo()?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|error| format!("could not press Ctrl: {error}"))?;
    let paste = enigo.key(Key::Unicode('v'), Direction::Click);
    // Always release Ctrl, even if the V click failed, so we never strand the
    // modifier held down in the user's session.
    let release = enigo.key(Key::Control, Direction::Release);
    paste.map_err(|error| format!("could not press V: {error}"))?;
    release.map_err(|error| format!("could not release Ctrl: {error}"))
}

pub struct LinuxInsertionRescue {
    rescue: ClipboardInsertionRescue<LinuxInsertionRescueSystem>,
}

impl Default for LinuxInsertionRescue {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxInsertionRescue {
    pub fn new() -> Self {
        Self {
            rescue: ClipboardInsertionRescue::new(LinuxInsertionRescueSystem),
        }
    }
}

impl InsertionRescue for LinuxInsertionRescue {
    fn rescue(
        &self,
        transcription: &FinalTranscription,
    ) -> Result<InsertionRescueOutcome, InsertionRescueError> {
        self.rescue.rescue(transcription)
    }
}

struct LinuxInsertionRescueSystem;

impl InsertionRescueSystem for LinuxInsertionRescueSystem {
    fn copy_to_clipboard(&self, text: &str) -> Result<(), InsertionRescueError> {
        hold_clipboard_selection(text).map_err(InsertionRescueError::new)
    }

    fn notify_user(&self, title: &str, body: &str) -> Result<(), InsertionRescueError> {
        notify_user(title, body).map_err(InsertionRescueError::new)
    }
}

/// Copy `text` to the clipboard and keep serving the X11 selection on a detached
/// thread until another app takes ownership (PRD risk R3). A plain `set_text`
/// releases the selection as soon as the `Clipboard` drops, so the rescued
/// transcription would vanish before the user could paste it. `set().wait()`
/// blocks the helper thread serving selection requests, so the text stays
/// pasteable for as long as Slugtale (a Resident App) runs. It is still lost if
/// Slugtale quits with no clipboard manager running — Cinnamon ships none by
/// default.
fn hold_clipboard_selection(text: &str) -> Result<(), String> {
    use arboard::{Clipboard, SetExtLinux};

    // Prove the clipboard is reachable on this thread before detaching, so a
    // failure is reported to the caller rather than swallowed by the thread.
    let mut probe = Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    probe
        .set_text(text.to_owned())
        .map_err(|error| format!("could not set clipboard text: {error}"))?;
    drop(probe);

    let owned = text.to_owned();
    std::thread::Builder::new()
        .name("slugtale-clipboard".to_string())
        .spawn(move || match Clipboard::new() {
            Ok(mut clipboard) => {
                // Blocks serving the selection until another app claims the
                // clipboard; then the thread exits.
                if let Err(error) = clipboard.set().wait().text(owned) {
                    eprintln!("slugtale: clipboard selection hold ended: {error}");
                }
            }
            Err(error) => eprintln!("slugtale: could not hold clipboard selection: {error}"),
        })
        .map_err(|error| format!("could not spawn clipboard holder: {error}"))?;
    Ok(())
}

/// Show a user-facing desktop notification over D-Bus
/// (org.freedesktop.Notifications, native on Cinnamon) via notify-rust. `Ok`
/// means the notification was dispatched to the daemon.
fn notify_user(title: &str, body: &str) -> Result<(), String> {
    use notify_rust::Notification;

    Notification::new()
        .summary(title)
        .body(body)
        .appname("Slugtale")
        .show()
        .map(|_| ())
        .map_err(|error| format!("could not show notification: {error}"))
}

/// Play the audible dictation cue through the XDG sound theme with
/// `canberra-gtk-play`, so v1 ships no bundled audio assets (the Linux analog of
/// the afplay arm in recording_feedback.rs). The helper is spawned detached so
/// the recording lifecycle never blocks on audio.
pub(crate) fn play_dictation_sound(sound: DictationSound) -> std::io::Result<()> {
    // Freedesktop sound-theme event ids: a soft "message" chime for the start
    // edge and "complete" for the stop edge — the closest theme pairing to the
    // macOS Tink/Pop cues.
    let (event_id, ogg) = match sound {
        DictationSound::Start => ("message", "message-new-instant"),
        DictationSound::Stop => ("complete", "complete"),
    };

    let spawn_null = |mut command: std::process::Command| {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };

    // canberra-gtk-play resolves the active XDG sound theme; if it is not
    // installed, fall back to paplay against the freedesktop stereo theme file.
    // Like the macOS afplay arm, audible feedback is best-effort — a missing
    // player must never fail the dictation lifecycle.
    let mut canberra = std::process::Command::new("canberra-gtk-play");
    canberra.arg("-i").arg(event_id);
    if spawn_null(canberra).is_ok() {
        return Ok(());
    }

    let mut paplay = std::process::Command::new("paplay");
    paplay.arg(format!(
        "/usr/share/sounds/freedesktop/stereo/{ogg}.oga"
    ));
    let _ = spawn_null(paplay);
    Ok(())
}

pub struct LinuxMicrophonePermissionSetup;

impl MicrophonePermissionSetup for LinuxMicrophonePermissionSetup {
    fn request_microphone_access(&self) -> Result<(), String> {
        // No per-process microphone consent gate exists on non-sandboxed Linux;
        // readiness reports presence of an input device instead. Nothing to
        // request.
        Ok(())
    }

    fn open_microphone_settings(&self) -> Result<(), String> {
        // Guidance-only: there is no permission page to route to. The sound
        // settings panel is where a user checks their input device, so open it
        // best-effort and treat its absence as a no-op rather than an error.
        open_sound_settings();
        Ok(())
    }
}

pub struct LinuxTextInsertionPermissionSetup;

impl TextInsertionPermissionSetup for LinuxTextInsertionPermissionSetup {
    fn request_text_insertion_access(&self) -> Result<bool, String> {
        // X11 has no accessibility-trust gate — synthesized input is always
        // permitted — so the request trivially succeeds. (On Wayland this maps
        // to a portal grant in Phase 2.)
        Ok(detect_session().is_supported())
    }

    fn open_text_insertion_settings(&self) -> Result<(), String> {
        // No settings page exists for a gate that does not exist on X11. This is
        // an intentional no-op; readiness reports insertion granted on X11.
        Ok(())
    }
}

/// Best-effort launch of the desktop sound settings. Cinnamon exposes
/// `cinnamon-settings sound`; fall back to the XDG sound URI. Failure to launch
/// is ignored — this is guidance, not a gate.
fn open_sound_settings() {
    let _ = std::process::Command::new("cinnamon-settings")
        .arg("sound")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .or_else(|_| {
            std::process::Command::new("xdg-open")
                .arg("settings://sound")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        });
}

/// The active X11 window id, captured at record start so insertion can
/// re-target it (parallels the macOS `frontmost_app_pid`). The seam is
/// pid-shaped across platforms; on X11 the meaningful unit is the window id,
/// which is carried through the `i32` unchanged.
pub fn frontmost_app_pid() -> Option<i32> {
    match active_window_id() {
        Ok(Some(window)) => Some(window as i32),
        Ok(None) => None,
        Err(error) => {
            eprintln!("slugtale: could not read active window: {error}");
            None
        }
    }
}

/// Bring the window captured at record start back to the foreground before
/// synthesized input, mirroring the macOS `activate_app`. `pid` is the X11
/// window id captured by [`frontmost_app_pid`]. Returns whether the activation
/// request was sent; a failure lets the caller proceed and fall through to the
/// clipboard rescue like any other delivery failure (PRD risk on foreground
/// stealing).
pub fn activate_app(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    match activate_window_id(pid as u32) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("slugtale: could not activate window {pid}: {error}");
            false
        }
    }
}

/// Connect to the X server and resolve the root window and `_NET_ACTIVE_WINDOW`
/// atom — the shared preamble both the read and the re-activation of the active
/// window need.
fn active_window_context() -> Result<(x11rb::rust_connection::RustConnection, u32, u32), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;

    let (conn, screen_num) = x11rb::connect(None).map_err(|error| error.to_string())?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or_else(|| "no X11 screen".to_string())?
        .root;
    let net_active_window = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?
        .atom;
    Ok((conn, root, net_active_window))
}

/// Read `_NET_ACTIVE_WINDOW` from the root window via x11rb (EWMH).
fn active_window_id() -> Result<Option<u32>, String> {
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let (conn, root, net_active_window) = active_window_context()?;
    let reply = conn
        .get_property(false, root, net_active_window, AtomEnum::WINDOW, 0, 1)
        .map_err(|error| error.to_string())?
        .reply()
        .map_err(|error| error.to_string())?;
    let window = reply
        .value32()
        .and_then(|mut values| values.next())
        .filter(|&window| window != 0);
    Ok(window)
}

/// Re-activate `window` with an EWMH `_NET_ACTIVE_WINDOW` client message sent to
/// the root, the request a window manager honors to raise and focus a window
/// (the X11 analog of `SetForegroundWindow` / `activateWithOptions`).
fn activate_window_id(window: u32) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ClientMessageEvent, ConnectionExt, EventMask, CLIENT_MESSAGE_EVENT,
    };

    let (conn, root, net_active_window) = active_window_context()?;

    // data[0] = source indication. We use 2 ("pager or similar"), not 1
    // (a normal application), on purpose: EWMH window managers — Muffin on
    // Cinnamon included — apply focus-stealing prevention to source 1 and will
    // reject a request that carries a stale/zero timestamp (data[1]), silently
    // dropping our re-target so keystrokes land in the wrong window. Source 2 is
    // the trusted-pager path WMs honor unconditionally, which is exactly the
    // deliberate refocus we want after dictation (the tactic wmctrl uses). The
    // remaining fields (timestamp, requestor) are left zero as EWMH permits for
    // source 2.
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: net_active_window,
        data: [2, 0, 0, 0, 0].into(),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|error| error.to_string())?;
    conn.flush().map_err(|error| error.to_string())?;
    Ok(())
}

/// Show a user-facing notification. Exposed for the Tauri layer; parallels the
/// macOS/Windows `notify` free function.
pub fn notify(title: &str, body: &str) -> Result<(), String> {
    notify_user(title, body)
}

/// Deep-link the user to where they can check their microphone input device.
/// Exposed for the Tauri `open_microphone_settings` command; guidance-only on
/// Linux (there is no permission page).
pub fn open_microphone_settings() -> Result<(), String> {
    open_sound_settings();
    Ok(())
}

/// Which day the user's Linux locale calls the first of the week, for the Usage
/// pane's "this week" (ADR-0025).
///
/// The authoritative answer is glibc's `_NL_TIME_FIRST_WEEKDAY`, which needs
/// libc — a dependency this adapter does not otherwise carry, for one integer.
/// So this reads the locale environment instead and asks only the question that
/// actually changes the answer: is this one of the regions that starts its week
/// on Sunday? Everywhere else, including an unset locale, gets the ISO week,
/// which is what the overwhelming majority of Linux locales use anyway.
pub fn locale_week_start() -> WeekStart {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_TIME"))
        .or_else(|_| std::env::var("LANG"))
        .ok();
    week_start_from_locale(locale.as_deref())
}

/// The regions whose locales start the week on Sunday. Territory codes rather
/// than languages, because `es_ES` starts on Monday while `es_MX` starts on
/// Sunday — the language says nothing about the week.
const SUNDAY_START_TERRITORIES: [&str; 16] = [
    "US", "CA", "MX", "BR", "AR", "CO", "PE", "VE", "JP", "KR", "TW", "PH", "IL", "ZA", "IN", "CN",
];

fn week_start_from_locale(locale: Option<&str>) -> WeekStart {
    let Some(locale) = locale else {
        return WeekStart::default();
    };
    // `en_US.UTF-8@euro` — the territory is between the underscore and whatever
    // codeset or modifier follows it.
    let Some(rest) = locale.split('_').nth(1) else {
        return WeekStart::default();
    };
    let territory = rest
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();

    if SUNDAY_START_TERRITORIES.contains(&territory.as_str()) {
        WeekStart::Sunday
    } else {
        WeekStart::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_week_starts_on_sunday_only_where_the_territory_says_so() {
        assert_eq!(week_start_from_locale(Some("en_US.UTF-8")), WeekStart::Sunday);
        assert_eq!(week_start_from_locale(Some("en_GB.UTF-8")), WeekStart::Monday);
        assert_eq!(week_start_from_locale(Some("de_DE.UTF-8")), WeekStart::Monday);
    }

    #[test]
    fn the_language_alone_does_not_decide_the_week() {
        // The case a language-only guess gets wrong: Spain starts on Monday and
        // Mexico on Sunday, and both speak Spanish.
        assert_eq!(week_start_from_locale(Some("es_ES.UTF-8")), WeekStart::Monday);
        assert_eq!(week_start_from_locale(Some("es_MX.UTF-8")), WeekStart::Sunday);
    }

    #[test]
    fn an_absent_or_shapeless_locale_falls_back_to_the_iso_week() {
        assert_eq!(week_start_from_locale(None), WeekStart::Monday);
        assert_eq!(week_start_from_locale(Some("C")), WeekStart::Monday);
        assert_eq!(week_start_from_locale(Some("POSIX")), WeekStart::Monday);
        assert_eq!(week_start_from_locale(Some("")), WeekStart::Monday);
    }

    #[test]
    fn codesets_and_modifiers_are_stripped_before_the_territory_is_read() {
        assert_eq!(week_start_from_locale(Some("en_US")), WeekStart::Sunday);
        assert_eq!(week_start_from_locale(Some("en_us.utf8")), WeekStart::Sunday);
        assert_eq!(
            week_start_from_locale(Some("ca_ES.UTF-8@valencia")),
            WeekStart::Monday
        );
    }

    #[test]
    fn xdg_session_type_x11_is_authoritative() {
        assert_eq!(
            session_from_env(Some("x11"), Some("wayland-0"), Some(":0")),
            DisplayServerSession::X11
        );
    }

    #[test]
    fn xdg_session_type_wayland_is_authoritative() {
        assert_eq!(
            session_from_env(Some("wayland"), None, Some(":0")),
            DisplayServerSession::Wayland
        );
    }

    #[test]
    fn session_type_is_case_insensitive_and_trimmed() {
        assert_eq!(
            session_from_env(Some("  X11 "), None, None),
            DisplayServerSession::X11
        );
    }

    #[test]
    fn falls_back_to_wayland_display_when_session_type_unset() {
        assert_eq!(
            session_from_env(None, Some("wayland-0"), Some(":0")),
            DisplayServerSession::Wayland
        );
    }

    #[test]
    fn falls_back_to_x_display_when_only_display_is_set() {
        assert_eq!(
            session_from_env(None, None, Some(":0")),
            DisplayServerSession::X11
        );
    }

    #[test]
    fn empty_display_values_do_not_count_as_a_session() {
        assert_eq!(
            session_from_env(None, Some(""), Some("")),
            DisplayServerSession::Unknown
        );
    }

    #[test]
    fn unknown_session_type_falls_through_to_display_probes() {
        assert_eq!(
            session_from_env(Some("tty"), None, Some(":0")),
            DisplayServerSession::X11
        );
    }

    #[test]
    fn only_x11_is_a_supported_session() {
        assert!(DisplayServerSession::X11.is_supported());
        assert!(!DisplayServerSession::Wayland.is_supported());
        assert!(!DisplayServerSession::Unknown.is_supported());
    }
}
