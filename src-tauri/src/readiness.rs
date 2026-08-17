use crate::{
    engine_blocked_reason, engine_that_can_run, EngineAvailability, Settings, TranscriptionEngine,
};
use serde::{Deserialize, Serialize};

/// Platform Adapter boundary (ADR-0021) for the OS-specific facts that gate
/// dictation: microphone permission and text insertion permission.
pub trait PlatformReadiness {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
}

/// Dictation Readiness (ADR-0013): dictation is only available once microphone
/// permission, text insertion permission, a configured hotkey, the assets for
/// the engine that will run, and a Transcription Engine that can actually run
/// are all ready.
///
/// The engine check is separate from the model check on purpose. A downloaded
/// model says only that the weights are on disk; whether anything in *this
/// binary* can decode them is a fact about the build, and a build compiled
/// without `local-whisper-runtime` has the file and no runtime (slugtale-bre).
pub fn dictation_ready(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    settings.hotkey.is_some()
        && platform.microphone_granted()
        && platform.insertion_granted()
        && (local_model_ready || !whisper_model_is_required(settings, engines))
        && engine_that_can_run(settings.primary_engine, engines).is_some()
}

/// Which engine a dictation started right now would actually be transcribed by,
/// falling back to the user's choice when nothing can run so the report still
/// talks about the engine they picked.
fn engine_in_play(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> TranscriptionEngine {
    engine_that_can_run(settings.primary_engine, engines).unwrap_or(settings.primary_engine)
}

/// Whether the Whisper ggml file on disk gates dictation on this machine.
///
/// "Local model" meant one thing when Whisper was the only engine. Now that
/// engines are plural it means *the assets for the engine that will actually
/// run*, and every other engine already reports its own assets through
/// [`EngineAvailability`] — Apple SpeechTranscriber's are system-managed and
/// Parakeet's are installed from Settings. So the Whisper download is required
/// only when Whisper is the engine in play, and a Parakeet-primary machine is
/// no longer blocked on a file it will never open (slugtale-y4m).
fn whisper_model_is_required(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    engine_in_play(settings, engines) == TranscriptionEngine::Whisper
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessItem {
    pub id: String,
    pub label: String,
    pub ready: bool,
    pub required: bool,
    /// Why this item is not ready, when the reason is specific to this machine
    /// or this build rather than fixed guidance the settings window already
    /// knows. `None` means the static copy for `id` is the whole story.
    pub detail: Option<String>,
}

impl ReadinessItem {
    pub fn ready(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: true,
            required,
            detail: None,
        }
    }

    pub fn missing(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: false,
            required,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsReadinessReport {
    pub dictation_available: bool,
    pub items: Vec<ReadinessItem>,
}

pub fn settings_readiness_report(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> SettingsReadinessReport {
    let engine_blocker = engine_blocked_reason(settings.primary_engine, engines);
    let whisper_model_required = whisper_model_is_required(settings, engines);

    SettingsReadinessReport {
        dictation_available: dictation_ready(settings, platform, local_model_ready, engines),
        items: vec![
            readiness_item(
                "microphone",
                "Microphone permission",
                true,
                platform.microphone_granted(),
            ),
            readiness_item(
                "text_insertion",
                "Text insertion permission",
                true,
                platform.insertion_granted(),
            ),
            readiness_item("hotkey", "Hotkey", true, settings.hotkey.is_some()),
            readiness_item(
                "local_model",
                "Local model",
                whisper_model_required,
                local_model_ready,
            )
            .with_detail(if whisper_model_required {
                None
            } else {
                Some(format!(
                    "Not needed: {} transcribes without the Whisper model.",
                    engine_in_play(settings, engines).display_name()
                ))
            }),
            readiness_item(
                "transcription_engine",
                "Transcription engine",
                true,
                engine_blocker.is_none(),
            )
            .with_detail(engine_blocker),
            readiness_item("launch_at_login", "Launch at login", false, true),
        ],
    }
}

fn readiness_item(id: &str, label: &str, required: bool, ready: bool) -> ReadinessItem {
    if ready {
        ReadinessItem::ready(id, label, required)
    } else {
        ReadinessItem::missing(id, label, required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_is_not_ready_when_nothing_is_ready() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        assert!(!dictation_ready(
            &Settings::default(),
            &platform,
            false,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_microphone_permission() {
        let platform = FakePlatform {
            microphone: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(
            &configured_settings(),
            &platform,
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_insertion_permission() {
        let platform = FakePlatform {
            insertion: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(
            &configured_settings(),
            &platform,
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_configured_hotkey() {
        let settings = Settings {
            hotkey: None,
            ..Settings::default()
        };
        assert!(!dictation_ready(
            &settings,
            &FakePlatform::all_ready(),
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_local_model() {
        assert!(!dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_ready_when_all_requirements_are_met() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn settings_readiness_report_shows_missing_required_items() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        let report = settings_readiness_report(
            &Settings::default(),
            &platform,
            false,
            &whisper_runtime_not_built(),
        );

        assert!(!report.dictation_available);
        assert_eq!(
            report.items,
            vec![
                ReadinessItem::missing("microphone", "Microphone permission", true),
                ReadinessItem::missing("text_insertion", "Text insertion permission", true),
                ReadinessItem::missing("hotkey", "Hotkey", true),
                ReadinessItem::missing("local_model", "Local model", true),
                ReadinessItem::missing("transcription_engine", "Transcription engine", true)
                    .with_detail(Some(
                        "Whisper base.en cannot run: this build was compiled without support for this engine"
                            .to_string(),
                    )),
                ReadinessItem::ready("launch_at_login", "Launch at login", false),
            ]
        );
    }
    #[test]
    fn settings_readiness_report_allows_dictation_when_required_items_are_ready() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        );

        assert!(report.dictation_available);
        assert!(report
            .items
            .iter()
            .filter(|item| item.required)
            .all(|item| item.ready));
    }
    #[test]
    fn model_readiness_is_supplied_outside_the_platform_adapter() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_available(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(!report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing("local_model", "Local model", true)
        );
    }

    #[test]
    fn readiness_uses_default_local_model_when_settings_model_is_unset() {
        let model_dir = unique_test_dir("readiness-default-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(crate::default_model_path(&model_dir), b"model").unwrap();

        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        };
        let report = settings_readiness_report(
            &settings,
            &FakePlatform::all_ready(),
            crate::local_model_ready(&model_dir),
            &whisper_available(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(local_model.ready);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn readiness_uses_default_local_model_when_settings_model_is_stale() {
        let model_dir = unique_test_dir("readiness-stale-model-setting");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(crate::default_model_path(&model_dir), b"model").unwrap();

        let stale_settings = Settings {
            model: Some(
                model_dir
                    .join("missing-custom-model.bin")
                    .to_string_lossy()
                    .to_string(),
            ),
            ..Settings::default()
        };
        let report = settings_readiness_report(
            &stale_settings,
            &FakePlatform::all_ready(),
            crate::local_model_ready(&model_dir),
            &whisper_available(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(local_model.ready);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn dictation_is_not_ready_when_no_engine_can_run() {
        // slugtale-bre: a default-feature build compiles no Whisper runtime. The
        // model file on disk says nothing about whether anything can decode it,
        // so readiness must not be satisfied by the download alone.
        assert!(!dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_runtime_not_built(),
        ));
    }

    #[test]
    fn dictation_is_ready_on_a_whisper_only_build_with_the_model_downloaded() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        ));
    }

    #[test]
    fn a_build_without_the_whisper_runtime_reports_why_rather_than_ready() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_runtime_not_built(),
        );
        let engine = report
            .items
            .iter()
            .find(|item| item.id == "transcription_engine")
            .unwrap();

        assert!(!report.dictation_available);
        assert!(!engine.ready);
        assert!(engine.required);
        // The user is told what is actually wrong with the binary, not sent to
        // re-download a model they already have.
        assert_eq!(
            engine.detail.as_deref(),
            Some(
                "Whisper base.en cannot run: this build was compiled without support for this engine"
            )
        );
    }

    #[test]
    fn a_whisper_only_build_that_can_transcribe_reports_no_engine_blocker() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        );
        let engine = report
            .items
            .iter()
            .find(|item| item.id == "transcription_engine")
            .unwrap();

        assert!(report.dictation_available);
        assert_eq!(
            engine,
            &ReadinessItem::ready("transcription_engine", "Transcription engine", true)
        );
    }

    #[test]
    fn a_machine_whose_engine_needs_no_whisper_model_is_ready_without_one() {
        // slugtale-y4m: Parakeet decodes its own installed assets, so blocking
        // dictation on a 148 MB Whisper download the user will never open is
        // over-blocking, not safety.
        assert!(dictation_ready(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &parakeet_available(),
        ));
    }

    #[test]
    fn the_local_model_is_optional_and_says_why_when_another_engine_runs() {
        let report = settings_readiness_report(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &parakeet_available(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing("local_model", "Local model", false).with_detail(Some(
                "Not needed: Parakeet TDT v2 transcribes without the Whisper model.".to_string()
            ))
        );
    }

    #[test]
    fn the_whisper_model_is_required_again_when_the_fallback_is_whisper() {
        // The chosen engine lost its assets, so the router falls back to Whisper
        // — which means the Whisper download is once more the thing standing
        // between this machine and a transcription.
        let engines = [
            (
                crate::TranscriptionEngine::Parakeet,
                crate::EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                    detail: "Parakeet assets are not installed.".to_string(),
                }),
            ),
            (
                crate::TranscriptionEngine::Whisper,
                crate::EngineAvailability::Available,
            ),
        ];
        let report = settings_readiness_report(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &engines,
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(!report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing("local_model", "Local model", true)
        );
    }

    #[test]
    fn the_whisper_model_stays_required_when_nothing_can_run_for_a_whisper_user() {
        // Nothing runs, so there is no engine in play to defer to; the user
        // chose Whisper, so the report keeps describing Whisper's requirements.
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_runtime_not_built(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(!report.dictation_available);
        assert!(local_model.required);
    }

    fn parakeet_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            primary_engine: crate::TranscriptionEngine::Parakeet,
            ..Settings::default()
        }
    }

    fn parakeet_available() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![
            (
                crate::TranscriptionEngine::Whisper,
                crate::EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                    detail: "The Whisper model has not been downloaded yet.".to_string(),
                }),
            ),
            (
                crate::TranscriptionEngine::Parakeet,
                crate::EngineAvailability::Available,
            ),
        ]
    }

    fn whisper_available() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![(
            crate::TranscriptionEngine::Whisper,
            crate::EngineAvailability::Available,
        )]
    }

    fn whisper_runtime_not_built() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![(
            crate::TranscriptionEngine::Whisper,
            crate::EngineAvailability::Unavailable(crate::EngineUnavailable::RuntimeNotBuilt),
        )]
    }

    struct FakePlatform {
        microphone: bool,
        insertion: bool,
    }

    impl FakePlatform {
        fn all_ready() -> Self {
            Self {
                microphone: true,
                insertion: true,
            }
        }
    }

    impl PlatformReadiness for FakePlatform {
        fn microphone_granted(&self) -> bool {
            self.microphone
        }
        fn insertion_granted(&self) -> bool {
            self.insertion
        }
    }

    fn configured_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        }
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-readiness-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
