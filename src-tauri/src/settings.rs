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

/// Update the user-configurable hotkey preferences that live in the Settings
/// File. Empty input clears the hotkey so Dictation Readiness reflects that the
/// user has not configured one.
pub fn apply_hotkey_settings(
    settings: &mut Settings,
    hotkey: Option<String>,
    activation_mode: ActivationMode,
) {
    settings.hotkey = hotkey.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    settings.activation_mode = activation_mode;
}

/// Write the Settings File as human-readable JSON so it can be inspected
/// during development (ADR-0018).
pub fn save_settings(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));

    std::fs::write(&temp_path, json)?;
    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

/// Load the Settings File, falling back to defaults when it is missing or
/// unreadable (e.g. first run).
pub fn load_settings(path: &std::path::Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
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
        let path =
            std::env::temp_dir().join(format!("slugtale-settings-{}.json", std::process::id()));
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
    #[test]
    fn hotkey_settings_store_trimmed_hotkey_and_activation_mode() {
        let mut settings = Settings::default();

        apply_hotkey_settings(
            &mut settings,
            Some("  Cmd+Shift+D  ".to_string()),
            ActivationMode::Hold,
        );

        assert_eq!(settings.hotkey, Some("Cmd+Shift+D".to_string()));
        assert_eq!(settings.activation_mode, ActivationMode::Hold);
    }
    #[test]
    fn blank_hotkey_setting_clears_configured_hotkey() {
        let mut settings = Settings {
            hotkey: Some("Cmd+Shift+D".to_string()),
            activation_mode: ActivationMode::Hold,
            ..Settings::default()
        };

        apply_hotkey_settings(
            &mut settings,
            Some("   ".to_string()),
            ActivationMode::Toggle,
        );

        assert_eq!(settings.hotkey, None);
        assert_eq!(settings.activation_mode, ActivationMode::Toggle);
    }
    #[test]
    fn activation_mode_persists_as_stable_lowercase_strings() {
        let settings = Settings {
            activation_mode: ActivationMode::Hold,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"activation_mode\":\"hold\""), "got: {json}");

        let toggled = Settings {
            activation_mode: ActivationMode::Toggle,
            ..Settings::default()
        };
        let json = serde_json::to_string(&toggled).unwrap();
        assert!(
            json.contains("\"activation_mode\":\"toggle\""),
            "got: {json}"
        );
    }
}
