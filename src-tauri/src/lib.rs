use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

pub fn build_tray_menu_items() -> Vec<(&'static str, &'static str)> {
    vec![
        ("settings", "Settings\u{2026}"),
        ("quit", "Quit Slugtale"),
    ]
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
