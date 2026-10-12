
use crate::{AsrError, CapturedAudio, DownloadProgress, FinalTranscription};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionEngine {
    Whisper,
    Parakeet,
    Phonon,
    AppleSpeech,
}

impl TranscriptionEngine {
    pub const ALL: [Self; 4] = [
        Self::Whisper,
        Self::Parakeet,
        Self::Phonon,
        Self::AppleSpeech,
    ];

    /// The stable identifier used in the Settings File and in non-content
    /// diagnostics. It never changes once shipped, even if the display name does.
    pub fn id(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Parakeet => "parakeet",
            Self::Phonon => "phonon",
            Self::AppleSpeech => "apple-speech",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper base.en",
            Self::Parakeet => "Parakeet TDT v2",
            Self::Phonon => "Phonon-2",
            Self::AppleSpeech => "Apple SpeechTranscriber",
        }
    }

    pub fn missing_provider_reason(self) -> &'static str {
        match self {
            Self::Whisper => "could not resolve a local model directory for Whisper",
            Self::Parakeet | Self::Phonon | Self::AppleSpeech => {
                "transcription engines are not ready yet"
            }
        }
    }
}

impl Default for TranscriptionEngine {
    fn default() -> Self {
        Self::Whisper
    }
}

impl std::fmt::Display for TranscriptionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// never anything the user said. That is what makes these safe to write to the
/// Local Diagnostic Log and to render verbatim in Settings. The `detail`
/// strings are authored by the providers themselves and must stay free of
/// audio, transcript text, vocabulary, and application context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum EngineUnavailable {
    UnsupportedPlatform { detail: String },
    UnsupportedOsVersion { required: String, detected: String },
    UnsupportedLocale { detected: String },
    AssetsMissing { detail: String },
    RuntimeNotBuilt,
    ProbeFailed { detail: String },
}

impl EngineUnavailable {
    pub fn is_user_resolvable(&self) -> bool {
        matches!(self, Self::AssetsMissing { .. })
    }
}

impl std::fmt::Display for EngineUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform { detail } => write!(f, "{detail}"),
            Self::UnsupportedOsVersion { required, detected } => {
                write!(f, "needs {required}; this machine runs {detected}")
            }
            Self::UnsupportedLocale { detected } => {
                write!(f, "no local assets for the {detected} locale")
            }
            Self::AssetsMissing { detail } => write!(f, "{detail}"),
            Self::RuntimeNotBuilt => {
                f.write_str("this build was compiled without support for this engine")
            }
            Self::ProbeFailed { detail } => write!(f, "{detail}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum EngineAvailability {
    Available,
    Unavailable(EngineUnavailable),
}

impl EngineAvailability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn unsupported_platform(engine: TranscriptionEngine, supported: &str) -> Self {
        Self::Unavailable(EngineUnavailable::UnsupportedPlatform {
            detail: format!("{} is available only on {supported}", engine.display_name()),
        })
    }
}

/// The scores are **engine-native and not comparable across engines**: `0.8`
/// from Apple SpeechTranscriber and `0.8` from Parakeet do not mean the same
/// thing until they have been calibrated on the same recordings
/// (docs/research/2026-07-24-small-local-asr-and-model-collaboration.md). The
/// Second Opinion router therefore uses these only against that engine's own
/// escalation threshold, never to rank one engine's result above another's.
///
/// `None` means the engine does not report that signal at all, which is a
/// different thing from reporting a low score — the router must not treat a
/// silent engine as an uncertain one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineConfidence {
    pub mean: Option<f32>,
    pub minimum: Option<f32>,
}

impl EngineConfidence {
    pub fn unreported() -> Self {
        Self::default()
    }

    pub fn escalation_score(&self) -> Option<f32> {
        self.minimum.or(self.mean)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EngineTranscription {
    pub engine: TranscriptionEngine,
    pub transcription: FinalTranscription,
    pub alternatives: Vec<String>,
    pub confidence: EngineConfidence,
    pub latency: Duration,
}

impl EngineTranscription {
    pub fn plain(
        engine: TranscriptionEngine,
        transcription: FinalTranscription,
        latency: Duration,
    ) -> Self {
        Self {
            engine,
            transcription,
            alternatives: Vec::new(),
            confidence: EngineConfidence::unreported(),
            latency,
        }
    }

    pub fn text(&self) -> &str {
        &self.transcription.text
    }
}

/// Where an engine's model files come from and what the user is entitled to
/// know about them. Settings renders this directly, so the licence, attribution,
/// and modification fields are the product's compliance surface rather than
/// decoration: Parakeet's CC BY 4.0 terms require the NVIDIA credit and a
/// statement of what Slugtale changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineMetadata {
    pub engine: TranscriptionEngine,
    pub model_id: &'static str,
    pub capability: &'static str,
    pub revision: &'static str,
    pub approximate_bytes: Option<u64>,
    pub source_url: Option<&'static str>,
    pub license: &'static str,
    pub license_url: &'static str,
    /// The credit the licence obliges Slugtale to display, when it obliges one.
    pub attribution: Option<&'static str>,
    /// What Slugtale (or its upstream converter) changed relative to the
    /// original weights — format conversion, quantisation. CC BY 4.0 requires
    /// this to be stated; `None` means the artefact is unmodified.
    pub modifications: Option<&'static str>,
    pub system_managed: bool,
    pub supported_platforms: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EngineAssets {
    pub installed_bytes: Option<u64>,
    pub present: Option<bool>,
}

impl EngineAssets {
    pub fn unmeasured() -> Self {
        Self {
            installed_bytes: None,
            present: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetInstall {
    pub warm_up: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineView {
    pub id: &'static str,
    pub display_name: &'static str,
    pub is_primary: bool,
    pub metadata: EngineMetadata,
    pub availability: EngineAvailability,
    pub unavailable_reason: Option<String>,
    pub installable: bool,
    pub assets: EngineAssets,
}

impl EngineView {
    pub fn of(provider: &dyn TranscriptionProvider, is_primary: bool) -> Self {
        let engine = provider.engine();
        let availability = provider.availability();
        let (unavailable_reason, installable) = match &availability {
            EngineAvailability::Available => (None, false),
            EngineAvailability::Unavailable(reason) => (
                Some(reason.to_string()),
                reason.is_user_resolvable() && provider.can_install_assets(),
            ),
        };

        Self {
            id: engine.id(),
            display_name: engine.display_name(),
            is_primary,
            metadata: provider.metadata(),
            availability,
            unavailable_reason,
            installable,
            assets: provider.assets(),
        }
    }
}

pub trait EngineTranscriber: Send + Sync {
    fn engine(&self) -> TranscriptionEngine;

    fn metadata(&self) -> EngineMetadata;

    fn availability(&self) -> EngineAvailability;

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError>;

    fn warm_up(&self) -> Result<(), AsrError> {
        Ok(())
    }
}

pub trait EngineAssetLifecycle: EngineTranscriber {
    fn assets(&self) -> EngineAssets;

    fn can_install_assets(&self) -> bool {
        false
    }

    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        let _ = on_progress;
        Err(assets_cannot_be_installed(self.engine()))
    }

    fn remove_assets(&self) -> Result<(), String> {
        Err(assets_cannot_be_removed(self.engine()))
    }
}

/// Providers take `&CapturedAudio` rather than owning it because a Second
/// Opinion replays the same recording through a second engine; cloning a
/// dictation's samples on every escalation would cost real memory on the 8 GB
/// reference machine. Implementations must be cheap to construct and must not
/// load model weights until [`EngineTranscriber::transcribe`] or an explicit
/// warm-up runs.
pub trait TranscriptionProvider: EngineTranscriber + EngineAssetLifecycle {}

impl<T: EngineTranscriber + EngineAssetLifecycle + ?Sized> TranscriptionProvider for T {}

pub(crate) fn assets_cannot_be_installed(engine: TranscriptionEngine) -> String {
    format!(
        "{} has no installation path in Slugtale.",
        engine.display_name()
    )
}

pub(crate) fn assets_cannot_be_removed(engine: TranscriptionEngine) -> String {
    format!(
        "{} has no assets for Slugtale to remove.",
        engine.display_name()
    )
}

pub fn engine_that_can_run(
    preferred: TranscriptionEngine,
    availability: &[(TranscriptionEngine, EngineAvailability)],
) -> Option<TranscriptionEngine> {
    let can_run = |engine: TranscriptionEngine| {
        availability
            .iter()
            .any(|(candidate, state)| *candidate == engine && state.is_available())
    };

    if can_run(preferred) {
        return Some(preferred);
    }

    TranscriptionEngine::ALL
        .into_iter()
        .find(|candidate| can_run(*candidate))
}

pub fn engine_blocked_reason(
    preferred: TranscriptionEngine,
    availability: &[(TranscriptionEngine, EngineAvailability)],
) -> Option<String> {
    if engine_that_can_run(preferred, availability).is_some() {
        return None;
    }

    let reason = availability
        .iter()
        .find(|(candidate, _)| *candidate == preferred)
        .and_then(|(_, state)| match state {
            EngineAvailability::Available => None,
            EngineAvailability::Unavailable(reason) => Some(reason),
        });

    Some(match reason {
        Some(reason) => format!("{} cannot run: {reason}", preferred.display_name()),
        None => format!("{} cannot run in this build.", preferred.display_name()),
    })
}

pub fn captured_audio_duration(audio: &CapturedAudio) -> Duration {
    if audio.sample_rate_hz == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(audio.samples.len() as f64 / f64::from(audio.sample_rate_hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StatedProvider {
        engine: TranscriptionEngine,
        availability: EngineAvailability,
        assets: EngineAssets,
        can_install: bool,
    }

    impl StatedProvider {
        fn new(
            engine: TranscriptionEngine,
            availability: EngineAvailability,
            assets: EngineAssets,
        ) -> Self {
            Self {
                engine,
                availability,
                assets,
                can_install: false,
            }
        }

        fn installable(mut self) -> Self {
            self.can_install = true;
            self
        }
    }

    impl EngineTranscriber for StatedProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.engine
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.engine,
                model_id: "test",
                capability: "test",
                revision: "test",
                approximate_bytes: None,
                source_url: None,
                license: "test",
                license_url: "https://example.test",
                attribution: None,
                modifications: None,
                system_managed: self.assets == EngineAssets::unmeasured(),
                supported_platforms: "test",
            }
        }

        fn availability(&self) -> EngineAvailability {
            self.availability.clone()
        }

        fn transcribe(&self, _audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
            Err(AsrError::Runtime(
                "this provider never transcribes".to_string(),
            ))
        }
    }

    impl EngineAssetLifecycle for StatedProvider {
        fn assets(&self) -> EngineAssets {
            self.assets
        }

        fn can_install_assets(&self) -> bool {
            self.can_install
        }
    }

    fn missing_assets() -> EngineAvailability {
        EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
            detail: "The model has not been downloaded yet.".to_string(),
        })
    }

    #[test]
    fn a_system_managed_engine_reports_no_bytes_and_never_an_install_slugtale_does() {
        let row = EngineView::of(
            &crate::AppleSpeechProvider::new(),
            TranscriptionEngine::AppleSpeech == crate::TranscriptionEngine::Whisper,
        );

        assert_eq!(row.assets, EngineAssets::unmeasured());
        assert_eq!(row.assets.installed_bytes, None);
        assert_eq!(row.assets.present, None);
        assert!(row.metadata.system_managed);
        assert_eq!(row.metadata.approximate_bytes, None);
        assert_eq!(row.metadata.source_url, None);

        assert_eq!(
            row.installable,
            matches!(
                row.availability,
                EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing { .. })
            ),
            "a system-managed row is installable only on the OS's own reason"
        );
    }

    #[test]
    fn the_install_button_needs_both_a_reason_the_user_can_fix_and_an_install_path() {
        for unavailable in [
            EngineAvailability::Available,
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedPlatform {
                detail: "available only on macOS 26+".to_string(),
            }),
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedOsVersion {
                required: "macOS 26".to_string(),
                detected: "macOS 15".to_string(),
            }),
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedLocale {
                detected: "fr-FR".to_string(),
            }),
            EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt),
            EngineAvailability::Unavailable(EngineUnavailable::ProbeFailed {
                detail: "could not read the asset directory".to_string(),
            }),
        ] {
            let row = EngineView::of(
                &StatedProvider::new(
                    TranscriptionEngine::Parakeet,
                    unavailable.clone(),
                    EngineAssets {
                        installed_bytes: None,
                        present: Some(false),
                    },
                )
                .installable(),
                false,
            );
            assert!(
                !row.installable,
                "{unavailable:?} must not earn an install button"
            );
        }

        let installable = EngineView::of(
            &StatedProvider::new(
                TranscriptionEngine::Parakeet,
                missing_assets(),
                EngineAssets {
                    installed_bytes: Some(0),
                    present: Some(false),
                },
            )
            .installable(),
            false,
        );
        assert!(installable.installable);
    }

    #[test]
    fn no_engine_reports_installable_when_it_has_no_install_path() {
        let row = EngineView::of(
            &StatedProvider::new(
                TranscriptionEngine::Whisper,
                missing_assets(),
                EngineAssets {
                    installed_bytes: Some(0),
                    present: Some(false),
                },
            ),
            true,
        );

        assert!(!row.installable);
        assert_eq!(
            row.unavailable_reason.as_deref(),
            Some("The model has not been downloaded yet.")
        );
        assert!(row.is_primary);
    }

    #[test]
    fn the_default_asset_operations_refuse_instead_of_pretending() {
        let provider = StatedProvider::new(
            TranscriptionEngine::Parakeet,
            missing_assets(),
            EngineAssets::unmeasured(),
        );

        assert_eq!(
            provider.install_assets(&mut |_| {}).unwrap_err(),
            "Parakeet TDT v2 has no installation path in Slugtale."
        );
        assert_eq!(
            provider.remove_assets().unwrap_err(),
            "Parakeet TDT v2 has no assets for Slugtale to remove."
        );
    }

    #[test]
    fn the_row_keeps_the_field_names_the_settings_window_reads() {
        let row = EngineView::of(
            &StatedProvider::new(
                TranscriptionEngine::Parakeet,
                missing_assets(),
                EngineAssets {
                    installed_bytes: Some(41),
                    present: Some(false),
                },
            )
            .installable(),
            true,
        );

        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "id": "parakeet",
                "display_name": "Parakeet TDT v2",
                "is_primary": true,
                "metadata": serde_json::to_value(row.metadata.clone()).unwrap(),
                "availability": serde_json::json!({
                    "state": "unavailable",
                    "reason": "assets-missing",
                    "detail": "The model has not been downloaded yet.",
                }),
                "unavailable_reason": "The model has not been downloaded yet.",
                "installable": true,
                "assets": { "installed_bytes": 41, "present": false },
            })
        );
    }

    #[test]
    fn each_engine_words_its_own_missing_provider() {
        assert_eq!(
            TranscriptionEngine::Whisper.missing_provider_reason(),
            "could not resolve a local model directory for Whisper"
        );
        assert_eq!(
            TranscriptionEngine::Parakeet.missing_provider_reason(),
            "transcription engines are not ready yet"
        );
        assert_eq!(
            TranscriptionEngine::AppleSpeech.missing_provider_reason(),
            "transcription engines are not ready yet"
        );
    }

    #[test]
    fn engine_ids_are_stable_and_distinct() {
        let ids: Vec<&str> = TranscriptionEngine::ALL.iter().map(|e| e.id()).collect();
        assert_eq!(ids, vec!["whisper", "parakeet", "phonon", "apple-speech"]);

        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "engine ids must be distinct");
    }

    #[test]
    fn engines_round_trip_through_the_settings_file_as_their_ids() {
        for engine in TranscriptionEngine::ALL {
            let json = serde_json::to_string(&engine).unwrap();
            assert_eq!(json, format!("\"{}\"", engine.id()));
            assert_eq!(
                serde_json::from_str::<TranscriptionEngine>(&json).unwrap(),
                engine
            );
        }
    }

    #[test]
    fn only_missing_assets_are_something_the_user_can_fix() {
        assert!(EngineUnavailable::AssetsMissing {
            detail: "model not installed".to_string(),
        }
        .is_user_resolvable());

        for unavailable in [
            EngineUnavailable::UnsupportedPlatform {
                detail: "macOS only".to_string(),
            },
            EngineUnavailable::UnsupportedOsVersion {
                required: "macOS 26".to_string(),
                detected: "macOS 15".to_string(),
            },
            EngineUnavailable::UnsupportedLocale {
                detected: "fr-FR".to_string(),
            },
            EngineUnavailable::RuntimeNotBuilt,
            EngineUnavailable::ProbeFailed {
                detail: "could not read the asset directory".to_string(),
            },
        ] {
            assert!(
                !unavailable.is_user_resolvable(),
                "{unavailable:?} must not offer an install action"
            );
        }
    }

    #[test]
    fn unsupported_platform_availability_names_the_engine_and_where_it_runs() {
        let availability =
            EngineAvailability::unsupported_platform(TranscriptionEngine::AppleSpeech, "macOS 26+");

        assert!(!availability.is_available());
        assert_eq!(
            availability,
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedPlatform {
                detail: "Apple SpeechTranscriber is available only on macOS 26+".to_string(),
            })
        );
    }

    #[test]
    fn escalation_reads_the_weakest_word_before_the_mean() {
        assert_eq!(
            EngineConfidence {
                mean: Some(0.95),
                minimum: Some(0.20),
            }
            .escalation_score(),
            Some(0.20)
        );
        assert_eq!(
            EngineConfidence {
                mean: Some(0.60),
                minimum: None,
            }
            .escalation_score(),
            Some(0.60)
        );
        assert_eq!(EngineConfidence::unreported().escalation_score(), None);
    }

    #[test]
    fn captured_audio_duration_reads_the_recording_length() {
        assert_eq!(
            captured_audio_duration(&CapturedAudio::mono_16khz(vec![0.0; 24_000])),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            captured_audio_duration(&CapturedAudio::mono_16khz(Vec::new())),
            Duration::ZERO
        );
        assert_eq!(
            captured_audio_duration(&CapturedAudio {
                sample_rate_hz: 0,
                samples: vec![0.0; 16_000],
            }),
            Duration::ZERO
        );
    }

    #[test]
    fn the_preferred_engine_runs_when_it_can() {
        let availability = [
            (TranscriptionEngine::Whisper, EngineAvailability::Available),
            (TranscriptionEngine::Parakeet, EngineAvailability::Available),
        ];

        assert_eq!(
            engine_that_can_run(TranscriptionEngine::Parakeet, &availability),
            Some(TranscriptionEngine::Parakeet)
        );
        assert_eq!(
            engine_blocked_reason(TranscriptionEngine::Parakeet, &availability),
            None
        );
    }

    #[test]
    fn a_preferred_engine_that_cannot_run_falls_back_to_one_that_can() {
        let availability = [
            (TranscriptionEngine::Whisper, EngineAvailability::Available),
            (
                TranscriptionEngine::Parakeet,
                EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
                    detail: "Parakeet assets are not installed.".to_string(),
                }),
            ),
        ];

        assert_eq!(
            engine_that_can_run(TranscriptionEngine::Parakeet, &availability),
            Some(TranscriptionEngine::Whisper)
        );
    }

    #[test]
    fn a_build_without_the_whisper_runtime_does_not_fall_back_to_whisper() {
        let availability = [
            (
                TranscriptionEngine::Whisper,
                EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt),
            ),
            (TranscriptionEngine::Parakeet, EngineAvailability::Available),
        ];

        assert_eq!(
            engine_that_can_run(TranscriptionEngine::Whisper, &availability),
            Some(TranscriptionEngine::Parakeet)
        );
    }

    #[test]
    fn no_engine_can_run_when_none_is_available() {
        let availability = [
            (
                TranscriptionEngine::Whisper,
                EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt),
            ),
            (
                TranscriptionEngine::Parakeet,
                EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt),
            ),
        ];

        assert_eq!(
            engine_that_can_run(TranscriptionEngine::Whisper, &availability),
            None
        );
    }

    #[test]
    fn an_engine_nobody_resolved_cannot_run() {
        assert_eq!(
            engine_that_can_run(TranscriptionEngine::AppleSpeech, &[]),
            None
        );
    }

    #[test]
    fn the_blocking_reason_names_the_engine_the_user_chose() {
        let availability = [(
            TranscriptionEngine::Whisper,
            EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt),
        )];

        assert_eq!(
            engine_blocked_reason(TranscriptionEngine::Whisper, &availability),
            Some(
                "Whisper base.en cannot run: this build was compiled without support for this engine"
                    .to_string()
            )
        );
    }

    #[test]
    fn the_blocking_reason_falls_back_to_a_general_statement() {
        assert_eq!(
            engine_blocked_reason(TranscriptionEngine::AppleSpeech, &[]),
            Some("Apple SpeechTranscriber cannot run in this build.".to_string())
        );
    }

    #[test]
    fn the_two_readings_of_one_engine_answer_the_same_thing() {
        let provider = StatedProvider::new(
            TranscriptionEngine::Parakeet,
            missing_assets(),
            EngineAssets {
                installed_bytes: Some(41),
                present: Some(false),
            },
        )
        .installable();

        let transcriber: &dyn EngineTranscriber = &provider;
        let assets: &dyn EngineAssetLifecycle = &provider;

        assert_eq!(transcriber.engine(), provider.engine());
        assert_eq!(transcriber.metadata(), provider.metadata());
        assert_eq!(transcriber.availability(), provider.availability());
        assert_eq!(assets.assets(), provider.assets());
        assert_eq!(assets.can_install_assets(), provider.can_install_assets());
    }

    #[test]
    fn a_plain_engine_result_reports_no_confidence_and_no_alternatives() {
        let result = EngineTranscription::plain(
            TranscriptionEngine::Whisper,
            FinalTranscription::plain("hello from slugtale"),
            Duration::from_millis(240),
        );

        assert_eq!(result.text(), "hello from slugtale");
        assert!(result.alternatives.is_empty());
        assert_eq!(result.confidence, EngineConfidence::unreported());
    }
}
