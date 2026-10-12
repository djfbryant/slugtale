use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivationMode {
    Hold,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedProfile {
    Fast,
    Balanced,
    Accurate,
}

impl Default for SpeedProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

pub const DEFAULT_SEGMENT_PAUSE_SECS: i64 = 5;
pub const MIN_SEGMENT_PAUSE_SECS: i64 = 2;
pub const MAX_SEGMENT_PAUSE_SECS: i64 = 10;

fn default_segment_pause_secs() -> i64 {
    DEFAULT_SEGMENT_PAUSE_SECS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BarPosition {
    BottomCenter,
    BottomLeft,
    BottomRight,
}

impl Default for BarPosition {
    fn default() -> Self {
        Self::BottomCenter
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentColor {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub hotkey: Option<String>,
    pub activation_mode: ActivationMode,
    pub launch_at_login: bool,
    pub diagnostic_logging: bool,
    pub model: Option<String>,
    #[serde(default)]
    pub speed_profile: SpeedProfile,
    #[serde(default = "default_segment_pause_secs")]
    pub segment_pause_secs: i64,
    #[serde(default)]
    pub bar_position: BarPosition,
    #[serde(default)]
    pub accent_color: AccentColor,
    #[serde(default)]
    pub bar_display: BarDisplay,
    #[serde(default)]
    pub primary_engine: crate::TranscriptionEngine,
    #[serde(default)]
    pub second_opinion: crate::SecondOpinionMode,
    #[serde(default)]
    pub store_usage: bool,
    #[serde(default)]
    pub typing_baseline: crate::TypingBaseline,
    #[serde(default)]
    pub transcript_cleanup: crate::TranscriptCleanupMode,
    #[serde(default)]
    pub voice_activation_enabled: bool,
    /// Whether a dictation records from the built-in microphone when the
    /// default one is Bluetooth (see `choose_input_device`). On by default and
    /// on for every Settings File written before the choice existed: a
    /// Bluetooth headset takes seconds to open its microphone and drops the
    /// user's audio to call quality while it is open.
    #[serde(default = "prefer_built_in_microphone_by_default")]
    pub prefer_built_in_microphone: bool,
}

fn prefer_built_in_microphone_by_default() -> bool {
    true
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
            segment_pause_secs: DEFAULT_SEGMENT_PAUSE_SECS,
            bar_position: BarPosition::default(),
            accent_color: AccentColor::default(),
            bar_display: BarDisplay::default(),
            primary_engine: crate::TranscriptionEngine::default(),
            second_opinion: crate::SecondOpinionMode::default(),
            store_usage: false,
            typing_baseline: crate::TypingBaseline::default(),
            transcript_cleanup: crate::TranscriptCleanupMode::default(),
            voice_activation_enabled: false,
            prefer_built_in_microphone: prefer_built_in_microphone_by_default(),
        }
    }
}

pub fn apply_usage_settings(settings: &mut Settings, store_usage: bool) {
    settings.store_usage = store_usage;
}

pub fn apply_microphone_settings(settings: &mut Settings, prefer_built_in_microphone: bool) {
    settings.prefer_built_in_microphone = prefer_built_in_microphone;
}

/// The one transactional settings save: apply the change to a copy of
/// `current`, perform the external side effect against the changed value,
/// persist — and roll the side effect back onto `current` when persisting
/// fails, so the saved file and the outside world can never disagree.
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

pub fn apply_transcription_settings(settings: &mut Settings, speed_profile: SpeedProfile) {
    settings.speed_profile = speed_profile;
}

/// Update the Segment Pause length stored in the Settings File. A value outside
/// the accepted range is an error rather than a silent clamp, so the user sees
/// that the choice was not saved and nothing is written.
pub fn apply_segment_pause_settings(
    settings: &mut Settings,
    segment_pause_secs: i64,
) -> Result<(), String> {
    if !(MIN_SEGMENT_PAUSE_SECS..=MAX_SEGMENT_PAUSE_SECS).contains(&segment_pause_secs) {
        return Err(format!(
            "Segment pause must be between {MIN_SEGMENT_PAUSE_SECS} and {MAX_SEGMENT_PAUSE_SECS} seconds."
        ));
    }
    settings.segment_pause_secs = segment_pause_secs;
    Ok(())
}

/// The Segment Pause the stored setting yields, clamped into the accepted range.
/// The clamp covers Settings Files edited by hand; a value saved through the app
/// is already in range. Opening silence never flushes a segment at any length.
pub fn segment_pause_duration(settings: &Settings) -> std::time::Duration {
    let secs = settings
        .segment_pause_secs
        .clamp(MIN_SEGMENT_PAUSE_SECS, MAX_SEGMENT_PAUSE_SECS);
    std::time::Duration::from_secs(secs as u64)
}

#[cfg(test)]
pub fn save_segment_pause(
    current: &Settings,
    segment_pause_secs: i64,
    adopt_pause: impl FnOnce(std::time::Duration),
    persist: impl FnOnce(&Settings) -> Result<(), String>,
) -> Result<Settings, String> {
    let mut proposed = current.clone();
    apply_segment_pause_settings(&mut proposed, segment_pause_secs)?;
    persist(&proposed)?;
    adopt_pause(segment_pause_duration(&proposed));
    Ok(proposed)
}

pub fn apply_transcript_cleanup_settings(
    settings: &mut Settings,
    transcript_cleanup: crate::TranscriptCleanupMode,
) {
    settings.transcript_cleanup = transcript_cleanup;
}

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

pub fn apply_launch_at_login_settings(settings: &mut Settings, enabled: bool) {
    settings.launch_at_login = enabled;
}

pub fn apply_voice_activation_settings(settings: &mut Settings, enabled: bool) {
    settings.voice_activation_enabled = enabled;
}

pub const VOICE_ACTIVATION_NEEDS_WHISPER: &str = "Voice activation needs the local Whisper model.";

pub fn validate_voice_activation(settings: &Settings, whisper_can_run: bool) -> Result<(), String> {
    if settings.voice_activation_enabled && !whisper_can_run {
        return Err(VOICE_ACTIVATION_NEEDS_WHISPER.to_string());
    }
    Ok(())
}

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
    fn voice_activation_needs_a_whisper_provider_that_can_run() {
        let mut settings = Settings::default();
        settings.voice_activation_enabled = true;

        assert_eq!(
            validate_voice_activation(&settings, false),
            Err(VOICE_ACTIVATION_NEEDS_WHISPER.to_string())
        );
        assert_eq!(validate_voice_activation(&settings, true), Ok(()));

        settings.voice_activation_enabled = false;
        assert_eq!(validate_voice_activation(&settings, false), Ok(()));
    }

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
        let mut settings = Settings::default();
        crate::apply_typed_estimate(&mut settings.typing_baseline, Some(48)).unwrap();
        apply_usage_settings(&mut settings, true);

        apply_usage_settings(&mut settings, false);

        assert_eq!(settings.typing_baseline.effective_wpm(), Some(48));
    }

    fn temp_settings_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-settings-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn segment_pause_defaults_to_five_seconds() {
        assert_eq!(DEFAULT_SEGMENT_PAUSE_SECS, 5);
        assert_eq!(Settings::default().segment_pause_secs, 5);
    }

    #[test]
    fn a_settings_file_written_before_segment_pause_loads_as_five_seconds() {
        let path = temp_settings_path("legacy-segment-pause");
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"balanced"}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.segment_pause_secs, DEFAULT_SEGMENT_PAUSE_SECS);
        assert_eq!(
            segment_pause_duration(&loaded),
            std::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn apply_segment_pause_settings_accepts_every_length_in_the_range() {
        let mut settings = Settings::default();

        for secs in MIN_SEGMENT_PAUSE_SECS..=MAX_SEGMENT_PAUSE_SECS {
            apply_segment_pause_settings(&mut settings, secs).unwrap();
            assert_eq!(settings.segment_pause_secs, secs);
        }
    }

    #[test]
    fn apply_segment_pause_settings_rejects_lengths_outside_the_range_and_keeps_the_saved_one() {
        let mut settings = Settings::default();
        apply_segment_pause_settings(&mut settings, 8).unwrap();

        for secs in [-1, 0, 1, 11, 60] {
            let result = apply_segment_pause_settings(&mut settings, secs);
            assert!(result.is_err(), "{secs} should be rejected");
            assert_eq!(settings.segment_pause_secs, 8);
        }
    }

    #[test]
    fn segment_pause_duration_clamps_a_hand_edited_value_into_the_range() {
        let mut settings = Settings::default();

        settings.segment_pause_secs = -1;
        assert_eq!(
            segment_pause_duration(&settings),
            std::time::Duration::from_secs(2)
        );

        settings.segment_pause_secs = 0;
        assert_eq!(
            segment_pause_duration(&settings),
            std::time::Duration::from_secs(2)
        );

        settings.segment_pause_secs = 600;
        assert_eq!(
            segment_pause_duration(&settings),
            std::time::Duration::from_secs(10)
        );

        settings.segment_pause_secs = 7;
        assert_eq!(
            segment_pause_duration(&settings),
            std::time::Duration::from_secs(7)
        );
    }

    #[test]
    fn segment_pause_persists_as_a_plain_number_of_seconds() {
        let path = temp_settings_path("segment-pause-roundtrip");
        let mut settings = Settings::default();
        apply_segment_pause_settings(&mut settings, 8).unwrap();

        save_settings(&path, &settings).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert!(json.contains("\"segment_pause_secs\": 8"), "got: {json}");
        assert_eq!(loaded.segment_pause_secs, 8);
    }

    #[test]
    fn a_hand_edited_negative_pause_loads_without_resetting_the_rest_of_the_file() {
        let path = temp_settings_path("negative-segment-pause");
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null,"speed_profile":"accurate","segment_pause_secs":-1}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.speed_profile, SpeedProfile::Accurate);
        assert_eq!(loaded.segment_pause_secs, -1);
        assert_eq!(
            segment_pause_duration(&loaded),
            std::time::Duration::from_secs(2)
        );
    }

    #[test]
    fn an_out_of_range_pause_is_stored_as_written_and_clamped_only_when_used() {
        let path = temp_settings_path("pause-clamped-only-at-use");
        let mut settings = Settings::default();
        settings.segment_pause_secs = -1;

        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded.segment_pause_secs, -1);
        assert_eq!(
            segment_pause_duration(&loaded),
            std::time::Duration::from_secs(MIN_SEGMENT_PAUSE_SECS as u64)
        );
        assert_eq!(loaded.segment_pause_secs, -1, "using the pause does not rewrite it");
    }

    #[test]
    fn saving_a_segment_pause_persists_it_and_reports_the_new_length() {
        let path = temp_settings_path("segment-pause-save");
        let adopted = std::cell::RefCell::new(Vec::new());

        let saved = save_segment_pause(
            &Settings::default(),
            8,
            |pause| adopted.borrow_mut().push(pause),
            |settings| save_settings(&path, settings).map_err(|error| error.to_string()),
        )
        .unwrap();

        let loaded = load_settings(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(saved.segment_pause_secs, 8);
        assert_eq!(loaded.segment_pause_secs, 8);
        assert_eq!(*adopted.borrow(), [std::time::Duration::from_secs(8)]);
    }

    #[test]
    fn a_rejected_segment_pause_touches_neither_the_file_nor_the_follow_up() {
        let current = Settings::default();
        let adopted = std::cell::RefCell::new(Vec::new());
        let persisted = std::cell::Cell::new(false);

        for secs in [-1, 1, 11] {
            let result = save_segment_pause(
                &current,
                secs,
                |pause| adopted.borrow_mut().push(pause),
                |_| {
                    persisted.set(true);
                    Ok(())
                },
            );

            assert!(result.is_err(), "{secs} should be rejected");
        }

        assert!(!persisted.get());
        assert!(adopted.borrow().is_empty());
    }

    #[test]
    fn the_saved_pause_stands_until_the_write_succeeds() {
        let current = Settings {
            segment_pause_secs: 3,
            ..Settings::default()
        };
        let adopted_pause = std::cell::Cell::new(std::time::Duration::from_secs(3));
        let seen_while_writing = std::cell::Cell::new(None);

        let saved = save_segment_pause(
            &current,
            8,
            |pause| adopted_pause.set(pause),
            |_| {
                seen_while_writing.set(Some(adopted_pause.get()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(saved.segment_pause_secs, 8);
        assert_eq!(
            seen_while_writing.get(),
            Some(std::time::Duration::from_secs(3))
        );
        assert_eq!(adopted_pause.get(), std::time::Duration::from_secs(8));
    }

    #[test]
    fn a_failed_segment_pause_write_keeps_the_saved_file_and_value() {
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-failed-segment-pause-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let blocked_temp_path = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        let current = Settings {
            segment_pause_secs: 3,
            ..Settings::default()
        };
        save_settings(&path, &current).unwrap();
        let saved_file = std::fs::read(&path).unwrap();

        std::fs::create_dir(&blocked_temp_path).unwrap();
        let adopted_pause = std::cell::Cell::new(std::time::Duration::from_secs(3));
        let seen_while_writing = std::cell::Cell::new(None);

        let result = save_segment_pause(
            &current,
            8,
            |pause| adopted_pause.set(pause),
            |settings| {
                seen_while_writing.set(Some(adopted_pause.get()));
                save_settings(&path, settings).map_err(|error| error.to_string())
            },
        );

        assert!(result.is_err());
        assert_eq!(
            seen_while_writing.get(),
            Some(std::time::Duration::from_secs(3))
        );
        assert_eq!(adopted_pause.get(), std::time::Duration::from_secs(3));
        assert_eq!(std::fs::read(&path).unwrap(), saved_file);
        assert_eq!(load_settings(&path).segment_pause_secs, 3);

        std::fs::remove_dir(&blocked_temp_path).unwrap();
        std::fs::remove_file(&path).unwrap();
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
            segment_pause_secs: 8,
            bar_position: BarPosition::BottomLeft,
            accent_color: AccentColor::Green,
            bar_display: BarDisplay::Monitor("Studio Display".to_string()),
            primary_engine: crate::TranscriptionEngine::Parakeet,
            second_opinion: crate::SecondOpinionMode::Automatic,
            store_usage: true,
            transcript_cleanup: crate::TranscriptCleanupMode::CleanDictationWithPauseBreaks,
            voice_activation_enabled: true,
            prefer_built_in_microphone: false,
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
    fn settings_file_without_a_microphone_choice_prefers_the_built_in_microphone() {
        let path = std::env::temp_dir().join(format!(
            "slugtale-settings-legacy-microphone-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"hotkey":null,"activation_mode":"toggle","launch_at_login":false,"diagnostic_logging":false,"model":null}"#,
        )
        .unwrap();

        let loaded = load_settings(&path);

        std::fs::remove_file(&path).ok();
        assert!(loaded.prefer_built_in_microphone);
        assert!(Settings::default().prefer_built_in_microphone);

        let mut settings = loaded;
        apply_microphone_settings(&mut settings, false);
        assert!(!settings.prefer_built_in_microphone);
    }
    #[test]
    fn dictation_bar_appearance_defaults_match_todays_bar() {
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
