use serde::{Deserialize, Serialize};

/// The behavior assigned to a hotkey when controlling dictation (ADR-0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

/// The Transcription Speed Profile (CONTEXT.md): a global user preference that
/// trades transcription accuracy against speed for every future dictation. Each
/// profile maps to an underlying Beam Search value in the local model runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedProfile {
    /// Fastest transcription, greedy decoding with no beam search.
    Fast,
    /// Balanced accuracy and speed. The default for new users.
    Balanced,
    /// Most accurate transcription, widest beam search and slowest.
    Accurate,
}

impl Default for SpeedProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

/// Where the Dictation Bar sits on the active display. All three options ride
/// the bottom edge; the orb is small enough that a corner no longer covers the
/// line being dictated into (slugtale-z7a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarPosition {
    /// Today's placement, and the default.
    BottomCenter,
    BottomLeft,
    BottomRight,
}

impl Default for BarPosition {
    fn default() -> Self {
        Self::BottomCenter
    }
}

/// The colour the Dictation Bar paints its orb in. A fixed palette rather than a
/// user-supplied hex: the accent sits on a dark translucent pill where arbitrary
/// colours can be illegible, and an enum never reaches CSS as interpolated text.
/// Each entry maps to a hex in the bar frontend, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
    /// Today's recording-dot red, and the default.
    Red,
    Amber,
    Green,
    Blue,
    Violet,
    Graphite,
}

impl Default for AccentColor {
    fn default() -> Self {
        Self::Red
    }
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
    /// The Transcription Speed Profile applied to all future transcriptions.
    /// Older Settings Files predate this field, so it defaults to Balanced.
    #[serde(default)]
    pub speed_profile: SpeedProfile,
    /// Where the Dictation Bar appears. Older Settings Files predate this field.
    #[serde(default)]
    pub bar_position: BarPosition,
    /// The colour the Dictation Bar orb paints in. Older Settings Files predate
    /// this field.
    #[serde(default)]
    pub accent_color: AccentColor,
    /// Which Transcription Engine runs first on every dictation. Older Settings
    /// Files predate this field and load as Whisper, which is what they used.
    #[serde(default)]
    pub primary_engine: crate::TranscriptionEngine,
    /// Whether Slugtale may ask a second local engine for another opinion when
    /// the first result looks uncertain (slugtale-vjs.3). Off by default, and
    /// Off reproduces the single-engine behaviour exactly.
    #[serde(default)]
    pub second_opinion: crate::SecondOpinionMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: None,
            activation_mode: ActivationMode::Toggle,
            launch_at_login: false,
            diagnostic_logging: false,
            model: None,
            speed_profile: SpeedProfile::default(),
            bar_position: BarPosition::default(),
            accent_color: AccentColor::default(),
            primary_engine: crate::TranscriptionEngine::default(),
            second_opinion: crate::SecondOpinionMode::default(),
        }
    }
}

/// Update which Transcription Engine leads and whether a second local engine
/// may be asked for another opinion (slugtale-vjs.3, slugtale-vjs.4).
///
/// This does not check that the chosen engine can actually run. Availability
/// depends on installed assets and on the machine, both of which can change
/// after the choice is made, so it is resolved when a dictation starts rather
/// than frozen into the Settings File.
pub fn apply_engine_settings(
    settings: &mut Settings,
    primary_engine: crate::TranscriptionEngine,
    second_opinion: crate::SecondOpinionMode,
) {
    settings.primary_engine = primary_engine;
    settings.second_opinion = second_opinion;
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

/// Update the Transcription Speed Profile stored in the Settings File. The user
/// sets this once in the Transcription section of settings and it persists across
/// restarts, applying to all future transcriptions (CONTEXT.md).
pub fn apply_transcription_settings(settings: &mut Settings, speed_profile: SpeedProfile) {
    settings.speed_profile = speed_profile;
}

/// Update the Dictation Bar's appearance: where it sits on screen and which
/// accent it paints. Both apply to the bar currently on screen as well as to
/// future dictations, so the user can judge the choice while making it.
pub fn apply_dictation_bar_settings(
    settings: &mut Settings,
    bar_position: BarPosition,
    accent_color: AccentColor,
) {
    settings.bar_position = bar_position;
    settings.accent_color = accent_color;
}

/// Update the launch-at-login preference stored in the Settings File. The stored
/// bool records the user's intent; registering the app as an OS login item is a
/// platform concern handled at the Tauri layer (ADR-0021).
pub fn apply_launch_at_login_settings(settings: &mut Settings, enabled: bool) {
    settings.launch_at_login = enabled;
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
        assert_eq!(settings.speed_profile, SpeedProfile::Balanced);
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
            speed_profile: SpeedProfile::Accurate,
            bar_position: BarPosition::BottomLeft,
            accent_color: AccentColor::Green,
            primary_engine: crate::TranscriptionEngine::Parakeet,
            second_opinion: crate::SecondOpinionMode::Automatic,
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
    fn transcription_speed_profile_defaults_to_balanced() {
        assert_eq!(SpeedProfile::default(), SpeedProfile::Balanced);
        assert_eq!(Settings::default().speed_profile, SpeedProfile::Balanced);
    }
    #[test]
    fn apply_transcription_settings_stores_selected_profile() {
        let mut settings = Settings::default();

        apply_transcription_settings(&mut settings, SpeedProfile::Fast);
        assert_eq!(settings.speed_profile, SpeedProfile::Fast);

        apply_transcription_settings(&mut settings, SpeedProfile::Accurate);
        assert_eq!(settings.speed_profile, SpeedProfile::Accurate);
    }
    #[test]
    fn apply_launch_at_login_settings_stores_choice() {
        let mut settings = Settings::default();
        assert!(!settings.launch_at_login);

        apply_launch_at_login_settings(&mut settings, true);
        assert!(settings.launch_at_login);

        apply_launch_at_login_settings(&mut settings, false);
        assert!(!settings.launch_at_login);
    }
    #[test]
    fn speed_profile_persists_as_stable_lowercase_strings() {
        for (profile, token) in [
            (SpeedProfile::Fast, "\"speed_profile\":\"fast\""),
            (SpeedProfile::Balanced, "\"speed_profile\":\"balanced\""),
            (SpeedProfile::Accurate, "\"speed_profile\":\"accurate\""),
        ] {
            let settings = Settings {
                speed_profile: profile,
                ..Settings::default()
            };
            let json = serde_json::to_string(&settings).unwrap();
            assert!(json.contains(token), "got: {json}");
        }
    }
    #[test]
    fn settings_file_without_speed_profile_loads_as_balanced() {
        // Settings Files written before the Transcription Speed Profile existed
        // omit the field; loading must fall back to the default rather than fail.
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.speed_profile, SpeedProfile::Balanced);
    }
    #[test]
    fn dictation_bar_appearance_defaults_match_todays_bar() {
        // Today's bar sits bottom-centre and paints a #ff5a52 recording dot, so
        // existing users see no change until they choose otherwise.
        assert_eq!(BarPosition::default(), BarPosition::BottomCenter);
        assert_eq!(AccentColor::default(), AccentColor::Red);
        assert_eq!(Settings::default().bar_position, BarPosition::BottomCenter);
        assert_eq!(Settings::default().accent_color, AccentColor::Red);
    }
    #[test]
    fn apply_dictation_bar_settings_stores_position_and_accent() {
        let mut settings = Settings::default();

        apply_dictation_bar_settings(&mut settings, BarPosition::BottomRight, AccentColor::Violet);

        assert_eq!(settings.bar_position, BarPosition::BottomRight);
        assert_eq!(settings.accent_color, AccentColor::Violet);
    }
    #[test]
    fn dictation_bar_appearance_persists_as_stable_strings() {
        for (position, token) in [
            (BarPosition::BottomCenter, "\"bar_position\":\"bottom-center\""),
            (BarPosition::BottomLeft, "\"bar_position\":\"bottom-left\""),
            (BarPosition::BottomRight, "\"bar_position\":\"bottom-right\""),
        ] {
            let settings = Settings {
                bar_position: position,
                ..Settings::default()
            };
            let json = serde_json::to_string(&settings).unwrap();
            assert!(json.contains(token), "got: {json}");
        }

        for (accent, token) in [
            (AccentColor::Red, "\"accent_color\":\"red\""),
            (AccentColor::Amber, "\"accent_color\":\"amber\""),
            (AccentColor::Green, "\"accent_color\":\"green\""),
            (AccentColor::Blue, "\"accent_color\":\"blue\""),
            (AccentColor::Violet, "\"accent_color\":\"violet\""),
            (AccentColor::Graphite, "\"accent_color\":\"graphite\""),
        ] {
            let settings = Settings {
                accent_color: accent,
                ..Settings::default()
            };
            let json = serde_json::to_string(&settings).unwrap();
            assert!(json.contains(token), "got: {json}");
        }
    }
    #[test]
    fn settings_file_without_dictation_bar_appearance_loads_as_defaults() {
        // Settings Files written before the Dictation Bar gained an accent and a
        // position omit both fields; loading must fall back rather than fail.
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-bar-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"fast"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.speed_profile, SpeedProfile::Fast);
        assert_eq!(loaded.bar_position, BarPosition::BottomCenter);
        assert_eq!(loaded.accent_color, AccentColor::Red);
    }
    #[test]
    fn a_settings_file_written_before_engine_choice_keeps_todays_behaviour() {
        // The whole promise of adding engines: an existing user who never opens
        // Settings sees no change at all — one engine, the one they had.
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-engine-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"balanced"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.primary_engine, crate::TranscriptionEngine::Whisper);
        assert_eq!(loaded.second_opinion, crate::SecondOpinionMode::Off);
    }

    #[test]
    fn apply_engine_settings_stores_the_primary_engine_and_second_opinion_mode() {
        let mut settings = Settings::default();

        apply_engine_settings(
            &mut settings,
            crate::TranscriptionEngine::Parakeet,
            crate::SecondOpinionMode::Automatic,
        );

        assert_eq!(settings.primary_engine, crate::TranscriptionEngine::Parakeet);
        assert_eq!(settings.second_opinion, crate::SecondOpinionMode::Automatic);
    }

    #[test]
    fn engine_choices_persist_as_stable_strings() {
        let settings = Settings {
            primary_engine: crate::TranscriptionEngine::AppleSpeech,
            second_opinion: crate::SecondOpinionMode::Automatic,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();

        assert!(json.contains("\"primary_engine\":\"apple-speech\""), "got: {json}");
        assert!(json.contains("\"second_opinion\":\"automatic\""), "got: {json}");
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
