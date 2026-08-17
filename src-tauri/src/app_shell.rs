use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

pub const REAUTHORIZE_PERMISSIONS_ARGUMENT: &str = "--reauthorize-permissions";

pub fn permission_reauthorization_requested<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| arg.as_ref() == REAUTHORIZE_PERMISSIONS_ARGUMENT)
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
    fn installed_app_reauthorization_mode_is_selected_by_its_launch_argument() {
        assert!(permission_reauthorization_requested([
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
            "--reauthorize-permissions",
        ]));
        assert!(!permission_reauthorization_requested([
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
        ]));
    }

    #[test]
    fn settings_window_hides_instead_of_closing() {
        assert!(hides_on_close("settings"));
    }

    #[test]
    fn unknown_windows_are_allowed_to_close() {
        assert!(!hides_on_close("dictation-bar"));
    }

    #[test]
    fn the_typing_challenge_window_is_destroyed_rather_than_hidden() {
        // It is created on demand and most users never open it, so keeping a
        // live webview around for the life of the app would be a cost with no
        // benefit. Closing it also has to actually end the run in progress, not
        // park a half-typed passage behind a hidden window (ADR-0025).
        assert!(!hides_on_close("typing-challenge"));
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
        assert!(
            !dev_runner.contains("run(\"open\", [\"-n\", appPath])"),
            "developer runs must not force a second Slugtale instance"
        );
        assert!(dev_runner.contains("run(\"open\", [appPath])"));
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
