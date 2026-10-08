//! Dictation lifecycle commands: the recording surface adapter, the activation
//! entry point shared by the Hotkey and Voice Activation, Stop/Cancel from the
//! Dictation Bar, and the bar's pointer hit-test and display choice.

use std::sync::Arc;

use tauri::{Emitter, Manager};

use slugtale_lib::{
    BoxedDiagnosticSink, DictationEffects, DictationHost, DictationParts, DictationPhase,
    WindowLabel,
};

use crate::dictation_bar_window::{hide_dictation_bar, show_dictation_bar};
use crate::hotkey_registration::{request_escape_registration, HotkeyRegistrationState};

use super::settings::{build_activation_snapshot_for, report_not_ready};
use super::{app_files, dictation_input_is_inert, load_current_settings, record_diagnostic_event};

/// Drive the recording surface (ADR-0014) from a dictation lifecycle event:
/// play the start/stop sound and show or hide the Dictation Bar. The bar's Stop
/// and Cancel controls route here; the global hotkey lifecycle routes the
/// configured activation hotkey and Escape here while preserving text-target
/// focus.
#[tauri::command]
pub(crate) fn dictation_event(app: tauri::AppHandle, event: String) -> Result<(), String> {
    match event.as_str() {
        "start" => {
            let event = slugtale_lib::parse_dictation_ui_event("start")?;
            dictation_host(&app).handle_dictation_event(event)
        }
        "stop" => stop_active_dictation(&app),
        "cancel" => cancel_active_dictation(&app),
        other => Err(slugtale_lib::parse_dictation_ui_event(other).unwrap_err()),
    }
}

/// Stop from the Dictation Bar and reset the shared control at the same time.
/// Voice Activation can then trigger again without a stale active state.
fn stop_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.stop(),
        slugtale_lib::DictationEvent::Stop,
    )
}

/// Cancel through the same lifecycle bridge used by the global Escape handler
/// so a click on the Dictation Bar cannot leave toggle/hold state believing a
/// discarded dictation is still active.
fn cancel_active_dictation(app: &tauri::AppHandle) -> Result<(), String> {
    end_active_dictation(
        app,
        |control| control.cancel(),
        slugtale_lib::DictationEvent::Cancel,
    )
}

/// End the active dictation through the shared lifecycle bridge, disarming bare
/// Escape while the registration lock is held. When no lifecycle answered — no
/// registration yet, or nothing active — the fallback event still runs so a
/// leftover Dictation Bar never outlives its dictation.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn end_active_dictation(
    app: &tauri::AppHandle,
    end: impl FnOnce(&mut slugtale_lib::DictationControl) -> Option<slugtale_lib::DictationEvent>,
    fallback: slugtale_lib::DictationEvent,
) -> Result<(), String> {
    let event = {
        let state = app.state::<HotkeyRegistrationState>();
        let mut registration = state
            .0
            .lock()
            .map_err(|_| "hotkey registration mutex poisoned".to_string())?;
        let event = end(&mut registration.control);
        if event.is_some() {
            // Disarm failures only matter when the worker is gone entirely,
            // which means the app is shutting down; dropping the request is
            // then the honest outcome.
            let _ = request_escape_registration(&registration, false);
        }
        event
    };

    match event {
        Some(event) => dictation_host(app).handle_dictation_event(event),
        None => dictation_host(app).handle_dictation_event(fallback),
    }
}

/// Begin a dictation from any activation input — a Hotkey press or a Voice
/// Activation wake phrase — through one readiness-gated sequence.
///
/// The sequence itself is not here: it belongs to the core Dictation Control
/// ([`slugtale_lib::begin_activation`]), which owns the order — transition, arm
/// Escape, record — and the rollback when a step fails. This adapter supplies
/// the host that sequence reaches: the shared control behind its registration
/// lock, the caller's Escape arming, and the dictation host that records. The
/// hotkey worker and Voice Activation differ only in how they arm Escape, which
/// is exactly what `arm_escape` is.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn begin_dictation(
    app: &tauri::AppHandle,
    input: slugtale_lib::DictationInput,
    arm_escape: &mut dyn FnMut(bool) -> Result<(), String>,
) -> Result<(), String> {
    // The Typing Challenge rule runs before any readiness snapshot is paid for
    // or lifecycle state moves: the user is typing a passage, so this input
    // does nothing at all and releasing it later resumes nothing. One rule,
    // consulted by the hotkey worker as well.
    if dictation_input_is_inert(app) {
        return Ok(());
    }

    let activation = build_activation_snapshot_for(app, input);
    if !activation.dictation_available() {
        report_not_ready(app, &activation.report);
    }

    slugtale_lib::begin_activation(&mut TauriBeginHost { app, arm_escape }, activation)
}

/// The `BeginHost` the Tauri tier supplies: the shared control, the caller's
/// Escape arming, and the dictation host that starts the recording.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct TauriBeginHost<'a> {
    app: &'a tauri::AppHandle,
    arm_escape: &'a mut dyn FnMut(bool) -> Result<(), String>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl slugtale_lib::BeginHost for TauriBeginHost<'_> {
    fn with_control<R>(
        &mut self,
        step: &mut dyn FnMut(&mut slugtale_lib::DictationControl) -> R,
    ) -> Option<R> {
        // The guard lives no longer than this call, so recording, transcription
        // and window work never run with the shared registration lock held —
        // they must not be able to stall the shortcut handler on the main
        // thread (slugtale-pil).
        let state = self.app.state::<HotkeyRegistrationState>();
        let mut registration = state.0.lock().ok()?;
        Some(step(&mut registration.control))
    }

    fn set_escape(&mut self, armed: bool) -> Result<(), String> {
        (self.arm_escape)(armed)
    }

    fn start(
        &mut self,
        event: slugtale_lib::DictationEvent,
        activation: slugtale_lib::DictationActivation,
    ) -> Result<(), String> {
        dictation_host(self.app).handle_dictation_event_with(event, Some(activation))
    }
}

/// The Tauri adapter for the dictation lifecycle's two ports: the bar window,
/// Settings reads, diagnostics, and failure notifications for the effects role,
/// and the app's own log, engines, and Text Insertion for the parts role. Both
/// are reached through the one AppHandle.
pub(crate) struct TauriSurface {
    pub(crate) app: tauri::AppHandle,
}

impl DictationEffects for TauriSurface {
    fn settings(&self) -> slugtale_lib::Settings {
        load_current_settings(&self.app)
    }

    fn record_diagnostic_event(&self, event: slugtale_lib::DiagnosticEvent) {
        record_diagnostic_event(&self.app, event);
    }

    fn show_dictation_bar(&self, phase: DictationPhase, settings: &slugtale_lib::Settings) {
        show_dictation_bar(&self.app, phase, settings);
    }

    fn hide_dictation_bar(&self) {
        hide_dictation_bar(&self.app);
    }

    fn emit_dictation_audio_level(&self, level: f32) {
        if let Some(window) = WindowLabel::DictationBar.window(&self.app) {
            let _ = window.emit("dictation-audio-level", level.clamp(0.0, 1.0));
        }
    }

    fn notify_capture_failure(&self, error: &str) {
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        let _ = slugtale_lib::notify("Slugtale could not capture audio", error);
    }

    fn play_dictation_sound(&self, sound: slugtale_lib::DictationSound) {
        let _ = slugtale_lib::play_dictation_sound(sound);
    }
}

impl DictationParts for TauriSurface {
    /// The app's one Local Diagnostic Log, handed on with its sink's type
    /// erased: the lifecycle names a log, and this module is the only place that
    /// knows it is a file.
    fn diagnostic_log(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> slugtale_lib::ErasedDiagnosticLog {
        app_files(&self.app)
            .diagnostic_log(settings.diagnostic_logging)
            .erased()
    }

    fn dictation_stack(
        &self,
        settings: &slugtale_lib::Settings,
    ) -> Result<slugtale_lib::DictationStack<BoxedDiagnosticSink>, String> {
        let diagnostic_log = self.diagnostic_log(settings);
        self.app
            .state::<slugtale_lib::TranscriptionEngineCatalogue>()
            .dictation_stack(settings, diagnostic_log)
            .map_err(|error| error.to_string())
    }

    fn prepared_insertion(
        &self,
        target_pid: Option<i32>,
    ) -> Result<slugtale_lib::PreparedInsertion, String> {
        slugtale_lib::prepare_text_insertion(target_pid)
    }
}

/// The app's one dictation lifecycle host, managed by setup before any
/// activation input can arrive.
pub(crate) fn dictation_host(app: &tauri::AppHandle) -> Arc<DictationHost> {
    app.state::<Arc<DictationHost>>().inner().clone()
}

/// Hand the pointer to whichever of Slugtale and the app underneath it is
/// actually over, and tell the bar which one that is.
///
/// The bar window is permanently sized for the expanded pill because a Tauri
/// window cannot grow on hover, so while collapsed most of it is transparent —
/// and a transparent window still swallows clicks. The frontend polls this while
/// the bar is visible: it cannot detect the pointer itself, because a window
/// ignoring cursor events receives no mouse events to detect it with.
#[tauri::command]
pub(crate) fn dictation_bar_pointer_over(
    app: tauri::AppHandle,
    expanded: bool,
) -> Result<bool, String> {
    let Some(window) = WindowLabel::DictationBar.window(&app) else {
        return Ok(false);
    };

    let position = load_current_settings(&app).bar_position;
    let scale_factor = window.scale_factor().map_err(|error| error.to_string())?;
    let origin = window.outer_position().map_err(|error| error.to_string())?;
    let pointer = app.cursor_position().map_err(|error| error.to_string())?;

    let over = slugtale_lib::pointer_is_over_dictation_bar(
        (pointer.x, pointer.y),
        (origin.x, origin.y),
        scale_factor,
        position,
        expanded,
    );
    window
        .set_ignore_cursor_events(!over)
        .map_err(|error| error.to_string())?;

    Ok(over)
}

/// One selectable display in the Settings UI. The stable monitor name is stored
/// in the Settings File; its label adds resolution so similarly named displays
/// remain distinguishable.
#[derive(serde::Serialize)]
pub(crate) struct DictationBarDisplayOption {
    value: slugtale_lib::BarDisplay,
    label: String,
}

/// Return the displays that can host the Dictation Bar right now. Displays with
/// no stable name cannot be selected safely across app launches, but the main
/// display is always available as the fallback choice.
#[tauri::command]
pub(crate) fn dictation_bar_displays(app: tauri::AppHandle) -> Vec<DictationBarDisplayOption> {
    let primary = app.primary_monitor().ok().flatten();
    let primary_label = primary
        .as_ref()
        .and_then(|monitor| monitor.name())
        .map(|name| slugtale_lib::primary_display_label(Some(name)))
        .unwrap_or_else(|| slugtale_lib::primary_display_label(None));
    let mut displays = vec![DictationBarDisplayOption {
        value: slugtale_lib::BarDisplay::Primary,
        label: primary_label,
    }];

    let monitors = app.available_monitors().unwrap_or_default();
    for monitor in monitors {
        if primary.as_ref().is_some_and(|primary| {
            monitor.position() == primary.position() && monitor.size() == primary.size()
        }) {
            continue;
        }
        let Some(name) = monitor.name().cloned() else {
            continue;
        };
        let size = monitor.size();
        displays.push(DictationBarDisplayOption {
            value: slugtale_lib::BarDisplay::Monitor(name.clone()),
            label: slugtale_lib::secondary_display_label(&name, size.width, size.height),
        });
    }

    displays
}
