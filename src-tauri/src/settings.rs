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

/// Which display hosts the Dictation Bar. The main display is the safe default
/// for existing settings files. A named display is matched when a dictation
/// starts; if it has been disconnected, the bar falls back to the main display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarDisplay {
    Primary,
    Monitor(String),
}

impl Default for BarDisplay {
    fn default() -> Self {
        Self::Primary
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
    /// Which display hosts the Dictation Bar. Older Settings Files predate this
    /// field and therefore use the main display.
    #[serde(default)]
    pub bar_display: BarDisplay,
    /// Which Transcription Engine runs first on every dictation. Older Settings
    /// Files predate this field and load as Whisper, which is what they used.
    #[serde(default)]
    pub primary_engine: crate::TranscriptionEngine,
    /// Whether Slugtale may ask a second local engine for another opinion when
    /// the first result looks uncertain (slugtale-vjs.3). Off by default, and
    /// Off reproduces the single-engine behaviour exactly.
    #[serde(default)]
    pub second_opinion: crate::SecondOpinionMode,
    /// Whether Slugtale may write Daily Usage Records at all (ADR-0025). Off by
    /// default and off for every Settings File written before Usage existed:
    /// nothing reaches the Usage File until the user asks for it, and dictations
    /// before that are gone rather than backfilled.
    #[serde(default)]
    pub store_usage: bool,
    /// The Typing Baseline that Time Saved is measured against (ADR-0025). It
    /// lives here rather than in the Usage File so that opting out of storing
    /// counts does not throw away a measurement the user sat through.
    #[serde(default)]
    pub typing_baseline: crate::TypingBaseline,
    /// How much local Transcript Cleanup runs before insertion (slugtale-kyc).
    /// Older Settings Files predate this field and therefore keep Basic, which
    /// is what they always got.
    #[serde(default)]
    pub transcript_cleanup: crate::TranscriptCleanupMode,
    /// Whether the experimental always-listening wake phrase may start a
    /// dictation without the hotkey (slugtale-e95). Off by default and off for
    /// every Settings File written before Voice Activation existed: the
    /// microphone is only ever held open by an explicit user choice.
    #[serde(default)]
    pub voice_activation_enabled: bool,
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
            bar_display: BarDisplay::default(),
            primary_engine: crate::TranscriptionEngine::default(),
            second_opinion: crate::SecondOpinionMode::default(),
            store_usage: false,
            typing_baseline: crate::TypingBaseline::default(),
            transcript_cleanup: crate::TranscriptCleanupMode::default(),
            voice_activation_enabled: false,
        }
    }
}

/// Update whether Slugtale stores Daily Usage Records (ADR-0025).
///
/// This only records the choice. Deleting the Usage File when the user opts out
/// is the caller's job, because the file's location is a platform concern and
/// the Settings File must not know where it lives.
pub fn apply_usage_settings(settings: &mut Settings, store_usage: bool) {
    settings.store_usage = store_usage;
}

/// The one transactional settings save: apply the change to a copy of
/// `current`, perform the external side effect against the changed value,
/// persist — and roll the side effect back onto `current` when persisting
/// fails, so the saved file and the outside world can never disagree.
///
/// Every settings save with an external side effect (hotkey registration,
/// launch at login, the Voice Activation worker) goes through here; hand
/// copies of this dance had already drifted.
pub fn apply_and_persist(
    current: &Settings,
    apply: impl FnOnce(&mut Settings),
    side_effect: impl Fn(&Settings) -> Result<(), String>,
    persist: impl FnOnce(&Settings) -> Result<(), String>,
) -> Result<Settings, String> {
    let mut settings = current.clone();
    apply(&mut settings);
    side_effect(&settings)?;
    if let Err(error) = persist(&settings) {
        let _ = side_effect(current);
        return Err(error);
    }
    Ok(settings)
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

/// Update the Transcript Cleanup mode stored in the Settings File (slugtale-kyc).
/// Applies to every future Dictation Segment; nothing about it needs the model
/// reloaded or the platform asked.
pub fn apply_transcript_cleanup_settings(
    settings: &mut Settings,
    transcript_cleanup: crate::TranscriptCleanupMode,
) {
    settings.transcript_cleanup = transcript_cleanup;
}

/// Update the Dictation Bar's appearance: where it sits on screen and which
/// accent it paints. Both apply to the bar currently on screen as well as to
/// future dictations, so the user can judge the choice while making it.
pub fn apply_dictation_bar_settings(
    settings: &mut Settings,
    bar_position: BarPosition,
    accent_color: AccentColor,
    bar_display: BarDisplay,
) {
    settings.bar_position = bar_position;
    settings.accent_color = accent_color;
    settings.bar_display = bar_display;
}

/// Update the launch-at-login preference stored in the Settings File. The stored
/// bool records the user's intent; registering the app as an OS login item is a
/// platform concern handled at the Tauri layer (ADR-0021).
pub fn apply_launch_at_login_settings(settings: &mut Settings, enabled: bool) {
    settings.launch_at_login = enabled;
}

/// Update whether the always-listening wake phrase may start a dictation
/// (slugtale-e95). This only records the choice; starting or stopping the
/// listener that holds the microphone open is the Tauri tier's job, because
/// its lifetime depends on platform audio support.
pub fn apply_voice_activation_settings(settings: &mut Settings, enabled: bool) {
    settings.voice_activation_enabled = enabled;
}

/// Write the Settings File as human-readable JSON so it can be inspected
/// during development (ADR-0018).
pub fn save_settings(path: &std::path::Path, settings: &Settings) -> std::io::Result<()> {
    crate::json_file::save(path, settings)
}

/// Load the Settings File, falling back to defaults when it is missing or
/// unreadable and quarantining JSON that cannot represent Settings.
pub fn load_settings(path: &std::path::Path) -> Settings {
    crate::json_file::load_or_default(path)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    #[test]
    fn a_failed_persist_rolls_the_side_effect_back_onto_current() {
        let current = Settings::default();
        let effects = RefCell::new(Vec::new());

        let result = apply_and_persist(
            &current,
            |settings| settings.store_usage = true,
            |settings| {
                effects
                    .borrow_mut()
                    .push(format!("effect:{}", settings.store_usage));
                Ok(())
            },
            |_| Err("disk full".to_string()),
        );

        assert_eq!(result.unwrap_err(), "disk full");
        // Applied against the changed value, then rolled back onto current.
        assert_eq!(
            *effects.borrow(),
            ["effect:true".to_string(), "effect:false".to_string()]
        );
    }

    #[test]
    fn a_successful_persist_keeps_the_applied_value() {
        let current = Settings::default();

        let saved = RefCell::new(None);
        let result = apply_and_persist(
            &current,
            |settings| settings.voice_activation_enabled = true,
            |_| Ok(()),
            |settings| {
                *saved.borrow_mut() = Some(settings.voice_activation_enabled);
                Ok(())
            },
        );

        assert!(result.unwrap().voice_activation_enabled);
        assert_eq!(saved.borrow().map(|enabled| enabled), Some(true));
    }

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
        assert!(!settings.store_usage);
        assert_eq!(settings.typing_baseline, crate::TypingBaseline::default());
    }

    #[test]
    fn a_settings_file_written_before_usage_existed_stores_nothing_and_has_no_baseline() {
        // The opt-in only means something if an upgrade is silent: an existing
        // user who never opens the Usage pane keeps writing no Usage File.
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-usage-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"balanced"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert!(!loaded.store_usage);
        assert_eq!(loaded.typing_baseline.effective_wpm(), None);
    }

    #[test]
    fn apply_usage_settings_stores_the_opt_in_choice() {
        let mut settings = Settings::default();

        apply_usage_settings(&mut settings, true);
        assert!(settings.store_usage);

        apply_usage_settings(&mut settings, false);
        assert!(!settings.store_usage);
    }

    #[test]
    fn opting_out_of_usage_leaves_the_typing_baseline_alone() {
        // The Typing Baseline lives in the Settings File precisely so that the
        // toggle cannot take it: the challenges were the user's time to spend.
        let mut settings = Settings::default();
        crate::apply_typed_estimate(&mut settings.typing_baseline, Some(48)).unwrap();
        apply_usage_settings(&mut settings, true);

        apply_usage_settings(&mut settings, false);

        assert_eq!(settings.typing_baseline.effective_wpm(), Some(48));
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
            bar_display: BarDisplay::Monitor("Studio Display".to_string()),
            primary_engine: crate::TranscriptionEngine::Parakeet,
            second_opinion: crate::SecondOpinionMode::Automatic,
            store_usage: true,
            transcript_cleanup: crate::TranscriptCleanupMode::CleanDictationWithPauseBreaks,
            voice_activation_enabled: true,
            typing_baseline: crate::TypingBaseline {
                challenges: vec![crate::TypingChallengeResult {
                    passage_index: 0,
                    words_per_minute: 58,
                }],
                typed_estimate: Some(45),
            },
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
    fn malformed_settings_are_recovered_until_a_normal_save_replaces_them() {
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-malformed-{}.json",
            std::process::id()
        ));
        let quarantine = path.with_file_name(format!(
            "{}.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&quarantine).ok();
        let malformed = b"{ not json";
        std::fs::write(&path, malformed).unwrap();

        let loaded = load_settings(&path);

        assert_eq!(loaded, Settings::default());
        assert_eq!(std::fs::read(&path).unwrap(), malformed);
        assert_eq!(std::fs::read(&quarantine).unwrap(), malformed);
        save_settings(&path, &Settings::default()).unwrap();
        assert_eq!(load_settings(&path), Settings::default());
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&quarantine).ok();
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
    fn transcript_cleanup_defaults_to_basic_and_can_be_disabled() {
        assert_eq!(
            Settings::default().transcript_cleanup,
            crate::TranscriptCleanupMode::Basic
        );

        let mut settings = Settings::default();
        apply_transcript_cleanup_settings(
            &mut settings,
            crate::TranscriptCleanupMode::CleanDictation,
        );
        assert_eq!(
            settings.transcript_cleanup,
            crate::TranscriptCleanupMode::CleanDictation
        );

        apply_transcript_cleanup_settings(
            &mut settings,
            crate::TranscriptCleanupMode::CleanDictationWithPauseBreaks,
        );
        assert_eq!(
            settings.transcript_cleanup,
            crate::TranscriptCleanupMode::CleanDictationWithPauseBreaks
        );

        // The whole point of the setting: turning it off restores exactly the
        // behaviour users had before filler cleanup existed.
        apply_transcript_cleanup_settings(&mut settings, crate::TranscriptCleanupMode::Basic);
        assert_eq!(
            settings.transcript_cleanup,
            crate::TranscriptCleanupMode::Basic
        );
    }

    #[test]
    fn a_settings_file_written_before_transcript_cleanup_loads_as_basic() {
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-cleanup-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"balanced"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(
            loaded.transcript_cleanup,
            crate::TranscriptCleanupMode::Basic
        );
    }

    #[test]
    fn transcript_cleanup_persists_as_a_stable_kebab_string() {
        let settings = Settings {
            transcript_cleanup: crate::TranscriptCleanupMode::CleanDictationWithPauseBreaks,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains("\"transcript_cleanup\":\"clean-dictation-with-pause-breaks\""),
            "got: {json}"
        );
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
    fn voice_activation_defaults_to_off() {
        // The whole point of the opt-in: a fresh install never holds the
        // microphone open without an explicit user choice.
        assert!(!Settings::default().voice_activation_enabled);
    }

    #[test]
    fn a_settings_file_written_before_voice_activation_loads_as_disabled() {
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-voice-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"balanced"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert!(!loaded.voice_activation_enabled);
    }

    #[test]
    fn apply_voice_activation_settings_stores_the_opt_in_choice() {
        let mut settings = Settings::default();

        apply_voice_activation_settings(&mut settings, true);
        assert!(settings.voice_activation_enabled);

        apply_voice_activation_settings(&mut settings, false);
        assert!(!settings.voice_activation_enabled);
    }

    #[test]
    fn voice_activation_persists_as_a_plain_bool() {
        let mut settings = Settings::default();
        apply_voice_activation_settings(&mut settings, true);

        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains("\"voice_activation_enabled\":true"),
            "got: {json}"
        );

        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-voice-roundtrip-{}.json",
            std::process::id()
        ));
        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path);
        std::fs::remove_file(&path).ok();
        assert!(loaded.voice_activation_enabled);
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
        assert_eq!(Settings::default().bar_display, BarDisplay::Primary);
    }
    #[test]
    fn apply_dictation_bar_settings_stores_position_and_accent() {
        let mut settings = Settings::default();

        apply_dictation_bar_settings(
            &mut settings,
            BarPosition::BottomRight,
            AccentColor::Violet,
            BarDisplay::Monitor("Studio Display".to_string()),
        );

        assert_eq!(settings.bar_position, BarPosition::BottomRight);
        assert_eq!(settings.accent_color, AccentColor::Violet);
        assert_eq!(
            settings.bar_display,
            BarDisplay::Monitor("Studio Display".to_string())
        );
    }
    #[test]
    fn dictation_bar_appearance_persists_as_stable_strings() {
        for (position, token) in [
            (
                BarPosition::BottomCenter,
                "\"bar_position\":\"bottom-center\"",
            ),
            (BarPosition::BottomLeft, "\"bar_position\":\"bottom-left\""),
            (
                BarPosition::BottomRight,
                "\"bar_position\":\"bottom-right\"",
            ),
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
        // Settings Files written before the Dictation Bar gained an accent,
        // position, and display omit those fields; loading must fall back rather
        // than fail.
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
        assert_eq!(loaded.bar_display, BarDisplay::Primary);
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

        assert_eq!(
            settings.primary_engine,
            crate::TranscriptionEngine::Parakeet
        );
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

        assert!(
            json.contains("\"primary_engine\":\"apple-speech\""),
            "got: {json}"
        );
        assert!(
            json.contains("\"second_opinion\":\"automatic\""),
            "got: {json}"
        );
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
