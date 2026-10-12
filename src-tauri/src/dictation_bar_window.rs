
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

struct MainThread(());

fn hand_to_main_thread(app: &tauri::AppHandle, work: impl FnOnce(&MainThread) + Send + 'static) {
    let _ = app.run_on_main_thread(move || work(&MainThread(())));
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
        hand_to_main_thread(app, move |main_thread| {
            position_dictation_bar(main_thread, &window, appearance.position, &bar_display);
            let _ = window.set_ignore_cursor_events(true);
            let _ = window.show();
            if WindowLabel::DictationBar.takes_focus() {
                let _ = window.set_focus();
            }
        });
    }
}

enum RenderFact {
    Phase(DictationPhase),
    Appearance(DictationBarAppearance),
    Visible(bool),
}

impl RenderFact {
    fn push(&self, window: &tauri::WebviewWindow) {
        let _ = match self {
            RenderFact::Phase(phase) => window.emit("dictation-phase", phase.as_str()),
            RenderFact::Appearance(appearance) => {
                window.emit("dictation-appearance", appearance.clone())
            }
            RenderFact::Visible(visible) => window.emit("dictation-visibility", *visible),
        };
    }
}

fn render_state_in_push_order(
    phase: DictationPhase,
    appearance: &DictationBarAppearance,
) -> [RenderFact; 3] {
    [
        RenderFact::Phase(phase),
        RenderFact::Appearance(appearance.clone()),
        RenderFact::Visible(true),
    ]
}

fn push_dictation_bar_render_state(
    window: &tauri::WebviewWindow,
    phase: DictationPhase,
    appearance: &DictationBarAppearance,
) {
    for fact in render_state_in_push_order(phase, appearance) {
        fact.push(window);
    }
}

pub(super) fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = WindowLabel::DictationBar.window(app) {
        let _ = window.hide();
        RenderFact::Visible(false).push(&window);
    }
}

fn dictation_bar_monitor(
    _main_thread: &MainThread,
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
    main_thread: &MainThread,
    window: &tauri::WebviewWindow,
    position: slugtale_lib::BarPosition,
    display: &slugtale_lib::BarDisplay,
) {
    let Some(monitor) = dictation_bar_monitor(main_thread, window, display) else {
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
    RenderFact::Appearance(appearance.clone()).push(&window);

    if !window.is_visible().unwrap_or(false) {
        return;
    }
    let bar_display = settings.bar_display.clone();
    hand_to_main_thread(app, move |main_thread| {
        position_dictation_bar(main_thread, &window, appearance.position, &bar_display);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_state_is_pushed_phase_then_appearance_then_visibility() {
        let appearance = DictationBarAppearance {
            position: slugtale_lib::BarPosition::default(),
            accent: slugtale_lib::AccentColor::default(),
        };

        let facts = render_state_in_push_order(DictationPhase::Transcribing, &appearance);

        assert!(matches!(
            facts,
            [
                RenderFact::Phase(DictationPhase::Transcribing),
                RenderFact::Appearance(_),
                RenderFact::Visible(true)
            ]
        ));
    }
}
