
use tauri::Emitter;

use slugtale_lib::{DictationPhase, WindowLabel};

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
    if let Some(window) = WindowLabel::DictationBar.window(app) {
        let appearance = DictationBarAppearance::from_settings(settings);
        let bar_display = settings.bar_display.clone();
        push_dictation_bar_render_state(&window, phase, &appearance);
        let _ = app.run_on_main_thread(move || {
            position_dictation_bar(&window, appearance.position, &bar_display);
            let _ = window.set_ignore_cursor_events(true);
            let _ = window.show();
            if WindowLabel::DictationBar.takes_focus() {
                let _ = window.set_focus();
            }
        });
    }
}

fn push_dictation_bar_render_state(
    window: &tauri::WebviewWindow,
    phase: DictationPhase,
    appearance: &DictationBarAppearance,
) {
    let _ = window.emit("dictation-phase", phase.as_str());
    let _ = window.emit("dictation-appearance", appearance.clone());
    let _ = window.emit("dictation-visibility", true);
}

pub(super) fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = WindowLabel::DictationBar.window(app) {
        let _ = window.hide();
        let _ = window.emit("dictation-visibility", false);
    }
}

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
    let Some(window) = WindowLabel::DictationBar.window(app) else {
        return;
    };
    let appearance = DictationBarAppearance::from_settings(settings);
    let _ = window.emit("dictation-appearance", appearance.clone());

    if !window.is_visible().unwrap_or(false) {
        return;
    }
    let bar_display = settings.bar_display.clone();
    let _ = app.run_on_main_thread(move || {
        position_dictation_bar(&window, appearance.position, &bar_display);
    });
}
