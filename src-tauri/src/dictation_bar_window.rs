//! The Dictation Bar's window choreography: showing, hiding, positioning, and
//! the render-state protocol that keeps the bar from flashing stale state.
//! The geometry itself lives in `slugtale_lib::dictation_bar`; this module
//! supplies the live monitor and window reads around it.

use tauri::{Emitter, Manager};

use super::dictation_host::DictationPhase;

/// The Dictation Bar's user-chosen appearance, pushed to the bar window so it can
/// paint its accent and align its orb to the edge it was sent to.
#[derive(Clone, serde::Serialize)]
pub(super) struct DictationBarAppearance {
    position: slugtale_lib::BarPosition,
    accent: slugtale_lib::AccentColor,
}

impl DictationBarAppearance {
    fn from_settings(settings: &slugtale_lib::Settings) -> Self {
        Self {
            position: settings.bar_position,
            accent: settings.accent_color,
        }
    }
}

pub(super) fn show_dictation_bar(
    app: &tauri::AppHandle,
    phase: DictationPhase,
    settings: &slugtale_lib::Settings,
) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let appearance = DictationBarAppearance::from_settings(settings);
        let bar_display = settings.bar_display.clone();
        push_dictation_bar_render_state(&window, phase, &appearance);
        // Placing the bar reads monitor geometry, and those reads block until the
        // main thread answers them. The global-key worker calls this while holding
        // the hotkey registration lock, and the main thread takes that same lock on
        // the next key transition — doing the work inline deadlocks both threads and
        // freezes the tray. Hand the window work to the main thread instead of
        // waiting on it (slugtale-1n4).
        let _ = app.run_on_main_thread(move || {
            position_dictation_bar(&window, appearance.position, &bar_display);
            // Start click-through: at rest the orb covers a seventh of the window,
            // and the pointer is somewhere else entirely. The bar takes input back
            // only when the hit test says the pointer is genuinely over the paint.
            let _ = window.set_ignore_cursor_events(true);
            let _ = window.show();
            if slugtale_lib::dictation_bar_should_take_focus() {
                let _ = window.set_focus();
            }
        });
    }
}

/// The bar's render-state protocol, written once (slugtale-s2g): every fact the
/// frontend renders is pushed before the window can appear, so the bar never
/// flashes a stale "recording" pill when it reappears for transcription, nor an
/// old accent or edge alignment. Phase first, appearance second, visibility last.
fn push_dictation_bar_render_state(
    window: &tauri::WebviewWindow,
    phase: DictationPhase,
    appearance: &DictationBarAppearance,
) {
    let _ = window.emit("dictation-phase", phase.as_str());
    let _ = window.emit("dictation-appearance", appearance.clone());
    // The bar polls for the pointer only while it is on screen; the webview
    // stays alive between dictations and has no other way to know.
    let _ = window.emit("dictation-visibility", true);
}

pub(super) fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let _ = window.hide();
        let _ = window.emit("dictation-visibility", false);
    }
}

/// Read the usable work area for the selected Dictation Bar display, in the
/// form the pure geometry wants. A disconnected named display falls back to the
/// main display so the bar never gets stranded off-screen.
fn dictation_bar_monitor(
    window: &tauri::WebviewWindow,
    display: &slugtale_lib::BarDisplay,
) -> Option<slugtale_lib::MonitorGeometry> {
    let primary = window.primary_monitor().ok().flatten();
    let monitor = match display {
        slugtale_lib::BarDisplay::Primary => primary,
        slugtale_lib::BarDisplay::Monitor(name) => window
            .available_monitors()
            .ok()
            .and_then(|monitors| {
                monitors
                    .into_iter()
                    .find(|monitor| monitor.name() == Some(name))
            })
            .or(primary),
    };

    let monitor = monitor?;

    let work_area = monitor.work_area();

    Some(slugtale_lib::MonitorGeometry {
        origin_x: work_area.position.x,
        origin_y: work_area.position.y,
        width: work_area.size.width,
        height: work_area.size.height,
        scale_factor: monitor.scale_factor(),
    })
}

/// Place the Dictation Bar along the bottom edge of the selected display's work
/// area, at the corner the user chose. The geometry itself lives in lib.rs;
/// this only supplies the live monitor and window reads.
fn position_dictation_bar(
    window: &tauri::WebviewWindow,
    position: slugtale_lib::BarPosition,
    display: &slugtale_lib::BarDisplay,
) {
    let Some(monitor) = dictation_bar_monitor(window, display) else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };

    let (x, y) = slugtale_lib::dictation_bar_origin(&monitor, size.width, size.height, position);
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Push a saved appearance change to a bar that is already on screen, so the user
/// sees the choice they just made instead of waiting for the next dictation.
/// Does nothing visible when the bar is hidden — showing it re-sends both.
pub(super) fn apply_dictation_bar_appearance(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) {
    let Some(window) = app.get_webview_window("dictation-bar") else {
        return;
    };
    let appearance = DictationBarAppearance::from_settings(settings);
    let _ = window.emit("dictation-appearance", appearance.clone());

    if !window.is_visible().unwrap_or(false) {
        return;
    }
    // Repositioning reads monitor geometry, which blocks on the main thread;
    // hand it over rather than waiting on it from here (slugtale-1n4).
    let bar_display = settings.bar_display.clone();
    let _ = app.run_on_main_thread(move || {
        position_dictation_bar(&window, appearance.position, &bar_display);
    });
}
