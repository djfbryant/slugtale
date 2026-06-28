use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};

use serde::{Deserialize, Serialize};

/// The behavior assigned to a hotkey when controlling dictation (ADR-0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

/// The local non-secret Settings File (ADR-0018): user preferences such as
/// hotkey, activation mode, model choice, launch-at-login, and diagnostic logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub hotkey: Option<String>,
    pub activation_mode: ActivationMode,
    pub launch_at_login: bool,
    pub diagnostic_logging: bool,
    pub model: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: None,
            activation_mode: ActivationMode::Toggle,
            launch_at_login: false,
            diagnostic_logging: false,
            model: None,
        }
    }
}

/// Write the Settings File as human-readable JSON so it can be inspected
/// during development (ADR-0018).
pub fn save_settings(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, json)
}

/// Load the Settings File, falling back to defaults when it is missing or
/// unreadable (e.g. first run).
pub fn load_settings(path: &std::path::Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Platform Adapter boundary (ADR-0021) for the OS-specific facts that gate
/// dictation: microphone permission, text insertion permission, and whether the
/// local model is present on disk.
pub trait PlatformReadiness {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
    fn local_model_present(&self) -> bool;
}

/// Dictation Readiness (ADR-0013): dictation is only available once microphone
/// permission, text insertion permission, a configured hotkey, and a local model
/// are all ready.
pub fn dictation_ready(settings: &Settings, platform: &dyn PlatformReadiness) -> bool {
    settings.hotkey.is_some()
        && platform.microphone_granted()
        && platform.insertion_granted()
        && platform.local_model_present()
}

#[cfg(target_os = "macos")]
pub use macos::MacosPlatform;

/// macOS implementation of the [`PlatformReadiness`] adapter (ADR-0021). Resolves
/// the OS-specific dictation gates from live system state: microphone permission
/// via AVFoundation, text insertion permission via the Accessibility API, and the
/// local model by checking the model file on disk.
#[cfg(target_os = "macos")]
mod macos {
    use super::PlatformReadiness;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
    use std::path::PathBuf;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

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
}

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
    fn fresh_settings_default_to_unconfigured_and_opt_out() {
        let settings = Settings::default();
        assert_eq!(settings.hotkey, None);
        assert_eq!(settings.activation_mode, ActivationMode::Toggle);
        assert!(!settings.launch_at_login);
        assert!(!settings.diagnostic_logging);
        assert_eq!(settings.model, None);
    }

    #[test]
    fn settings_round_trip_through_saved_file() {
        let path = std::env::temp_dir().join(format!("slugtale-settings-{}.json", std::process::id()));
        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            activation_mode: ActivationMode::Hold,
            launch_at_login: true,
            diagnostic_logging: true,
            model: Some("whisper-base.en".to_string()),
        };

        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn loading_missing_settings_file_returns_defaults() {
        let path = std::env::temp_dir().join("slugtale-settings-does-not-exist.json");
        std::fs::remove_file(&path).ok();

        assert_eq!(load_settings(&path), Settings::default());
    }

    struct FakePlatform {
        microphone: bool,
        insertion: bool,
        model: bool,
    }

    impl FakePlatform {
        fn all_ready() -> Self {
            Self { microphone: true, insertion: true, model: true }
        }
    }

    impl PlatformReadiness for FakePlatform {
        fn microphone_granted(&self) -> bool {
            self.microphone
        }
        fn insertion_granted(&self) -> bool {
            self.insertion
        }
        fn local_model_present(&self) -> bool {
            self.model
        }
    }

    fn configured_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn dictation_is_not_ready_when_nothing_is_ready() {
        let platform = FakePlatform { microphone: false, insertion: false, model: false };
        assert!(!dictation_ready(&Settings::default(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_microphone_permission() {
        let platform = FakePlatform { microphone: false, ..FakePlatform::all_ready() };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_insertion_permission() {
        let platform = FakePlatform { insertion: false, ..FakePlatform::all_ready() };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_not_ready_without_configured_hotkey() {
        let settings = Settings { hotkey: None, ..Settings::default() };
        assert!(!dictation_ready(&settings, &FakePlatform::all_ready()));
    }

    #[test]
    fn dictation_is_not_ready_without_local_model() {
        let platform = FakePlatform { model: false, ..FakePlatform::all_ready() };
        assert!(!dictation_ready(&configured_settings(), &platform));
    }

    #[test]
    fn dictation_is_ready_when_all_requirements_are_met() {
        assert!(dictation_ready(&configured_settings(), &FakePlatform::all_ready()));
    }

    #[test]
    fn activation_mode_persists_as_stable_lowercase_strings() {
        let settings = Settings { activation_mode: ActivationMode::Hold, ..Settings::default() };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"activation_mode\":\"hold\""), "got: {json}");

        let toggled = Settings { activation_mode: ActivationMode::Toggle, ..Settings::default() };
        let json = serde_json::to_string(&toggled).unwrap();
        assert!(json.contains("\"activation_mode\":\"toggle\""), "got: {json}");
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
