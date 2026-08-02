//! The Transcription Engine boundary (CONTEXT.md): the seam every local speech
//! recognizer sits behind so the Dictation Workflow, the Second Opinion router,
//! and Settings can talk about engines without knowing how any of them decode.
//!
//! Every engine reachable through this boundary runs entirely on the user's
//! device. There is no cloud engine and no remote fallback; a provider that
//! cannot answer locally reports [`EngineUnavailable`] rather than reaching for
//! the network (docs/research/2026-07-24-small-local-asr-and-model-collaboration.md).
//!
//! Two kinds of value cross this boundary and they are deliberately separated:
//!
//! - **User content** — the transcription itself, its alternatives, and its
//!   per-word confidence. These live only in [`EngineTranscription`], are passed
//!   in-process to the router and the Text Insertion path, and must never reach
//!   the Local Diagnostic Log, analytics, or the network.
//! - **Non-content diagnostics** — engine identity, availability reasons,
//!   latency, and escalation reason codes. These are safe to log and to render
//!   in Settings, and every type carrying them is a closed enum or a number so
//!   a caller cannot accidentally smuggle speech through them.

use crate::{AsrError, CapturedAudio, FinalTranscription};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A local speech recognition implementation Slugtale can ask for a
/// transcription. The set is closed on purpose: each engine carries its own
/// licence, attribution, and platform constraints that Settings has to render
/// accurately, so engines cannot be registered dynamically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionEngine {
    /// Whisper `base.en` through whisper.cpp — the established engine, and the
    /// only one available on every platform Slugtale targets.
    Whisper,
    /// NVIDIA Parakeet TDT v2 0.6B through ONNX Runtime (slugtale-vjs.1).
    Parakeet,
    /// Apple SpeechTranscriber, system-managed and Apple-only (slugtale-vjs.2).
    AppleSpeech,
}

impl TranscriptionEngine {
    /// Every engine Slugtale knows about, in the order Settings lists them.
    pub const ALL: [Self; 3] = [Self::Whisper, Self::Parakeet, Self::AppleSpeech];

    /// The stable identifier used in the Settings File and in non-content
    /// diagnostics. It never changes once shipped, even if the display name does.
    pub fn id(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Parakeet => "parakeet",
            Self::AppleSpeech => "apple-speech",
        }
    }

    /// The name shown to the user in Settings.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper base.en",
            Self::Parakeet => "Parakeet TDT v2",
            Self::AppleSpeech => "Apple SpeechTranscriber",
        }
    }
}

impl Default for TranscriptionEngine {
    /// Whisper, because it is the only engine available on every platform
    /// Slugtale ships to and the only one whose behaviour is already proven in
    /// this product. A Settings File that predates engine choice loads as
    /// Whisper and behaves exactly as it did before.
    fn default() -> Self {
        Self::Whisper
    }
}

impl std::fmt::Display for TranscriptionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Why a Transcription Engine cannot run on this machine right now.
///
/// Every variant describes the machine, the build, or the installed assets —
/// never anything the user said. That is what makes these safe to write to the
/// Local Diagnostic Log and to render verbatim in Settings. The `detail`
/// strings are authored by the providers themselves and must stay free of
/// audio, transcript text, vocabulary, and application context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum EngineUnavailable {
    /// The engine is bound to an operating system this build is not running on
    /// — Apple SpeechTranscriber asked for on Linux or Windows, for instance.
    UnsupportedPlatform { detail: String },
    /// The operating system is right but predates the engine's API.
    UnsupportedOsVersion { required: String, detected: String },
    /// The engine cannot transcribe the Dictation Language on this machine.
    UnsupportedLocale { detected: String },
    /// The engine is supported here, but the user has not installed its assets.
    /// This is the one recoverable variant: Settings turns it into an install
    /// action rather than a dead end.
    AssetsMissing { detail: String },
    /// This build was compiled without the engine's Cargo feature. Developer-run
    /// builds hit this whenever an engine's native toolchain is not wanted.
    RuntimeNotBuilt,
    /// Probing the engine failed for a reason none of the above covers.
    ProbeFailed { detail: String },
}

impl EngineUnavailable {
    /// Whether the user can fix this themselves from Settings. Only missing
    /// assets qualify: an unsupported OS or a build without the feature needs a
    /// different machine or a different build, not a button.
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

/// Whether a Transcription Engine can transcribe right now.
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

    /// Build the availability an engine reports when this build is running on
    /// an operating system the engine does not exist on. Kept here so every
    /// provider words the Linux/Windows case identically.
    pub fn unsupported_platform(engine: TranscriptionEngine, supported: &str) -> Self {
        Self::Unavailable(EngineUnavailable::UnsupportedPlatform {
            detail: format!("{} is available only on {supported}", engine.display_name()),
        })
    }
}

/// How confident an engine is in the transcription it just produced.
///
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
    /// Mean per-word (or per-token) confidence over the whole transcription,
    /// normalized to 0.0..=1.0 by the provider.
    pub mean: Option<f32>,
    /// The single least confident word in the transcription, same scale.
    pub minimum: Option<f32>,
}

impl EngineConfidence {
    pub fn unreported() -> Self {
        Self::default()
    }

    /// The score escalation rules read: the weakest word when the engine
    /// reports one, otherwise the mean. A transcription is usually wrong in one
    /// place rather than uniformly, so the minimum is the more useful trigger.
    pub fn escalation_score(&self) -> Option<f32> {
        self.minimum.or(self.mean)
    }
}

/// One engine's complete answer for one dictation.
///
/// `transcription` and `alternatives` are **user content**. They may be passed
/// in-process to the Second Opinion router and the Text Insertion path and
/// nowhere else — not the Local Diagnostic Log, not analytics, not the network.
/// Everything else on this struct is non-content and safe to record.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineTranscription {
    pub engine: TranscriptionEngine,
    pub transcription: FinalTranscription,
    /// Whole-transcript alternatives, best first, when the engine offers them.
    /// Slugtale selects between complete transcripts rather than merging words,
    /// so these stay unparsed strings.
    pub alternatives: Vec<String>,
    pub confidence: EngineConfidence,
    /// Wall-clock time this engine took, measured by the provider. Used for the
    /// escalation budget and for the measurement harness; safe to log.
    pub latency: Duration,
}

impl EngineTranscription {
    /// A result from an engine that reports no confidence and no alternatives —
    /// the shape Whisper produces today.
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
    /// The upstream model identifier, e.g. `nvidia/parakeet-tdt-0.6b-v2`.
    pub model_id: &'static str,
    /// The pinned upstream revision. Installation must not float to `main`.
    pub revision: &'static str,
    /// Roughly how much disk the installed assets take, for the Settings copy.
    /// `None` for system-managed assets Slugtale does not own.
    pub approximate_bytes: Option<u64>,
    /// Where the assets are fetched from during an explicit install. `None` when
    /// Slugtale never downloads them — Apple's are already on the machine.
    pub source_url: Option<&'static str>,
    pub license: &'static str,
    pub license_url: &'static str,
    /// The credit the licence obliges Slugtale to display, when it obliges one.
    pub attribution: Option<&'static str>,
    /// What Slugtale (or its upstream converter) changed relative to the
    /// original weights — format conversion, quantisation. CC BY 4.0 requires
    /// this to be stated; `None` means the artefact is unmodified.
    pub modifications: Option<&'static str>,
    /// True when the operating system owns the assets, so Slugtale neither
    /// bundles, extracts, nor redistributes them. Settings must say so rather
    /// than implying the app ships Apple's model.
    pub system_managed: bool,
    /// The operating systems this engine can ever run on, for Settings copy on
    /// machines where it is unavailable.
    pub supported_platforms: &'static str,
}

/// A Transcription Engine Slugtale can ask for a complete transcription.
///
/// Providers take `&CapturedAudio` rather than owning it because a Second
/// Opinion replays the same recording through a second engine; cloning a
/// dictation's samples on every escalation would cost real memory on the 8 GB
/// reference machine.
///
/// Implementations must be cheap to construct and must not load model weights
/// until [`TranscriptionProvider::transcribe`] or an explicit warm-up runs.
/// [`TranscriptionProvider::availability`] is called from Settings and from the
/// router's fast path, so it must answer from cached state rather than probing
/// the filesystem or the OS on every dictation.
pub trait TranscriptionProvider: Send + Sync {
    fn engine(&self) -> TranscriptionEngine;

    fn metadata(&self) -> EngineMetadata;

    fn availability(&self) -> EngineAvailability;

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError>;
}

/// Which Transcription Engine will actually transcribe the next dictation, given
/// what every engine reports about itself right now.
///
/// The rule is the preferred engine when it can run, otherwise the first engine
/// in [`TranscriptionEngine::ALL`] order that can. Falling back matters because a
/// user whose chosen engine's assets were deleted should still get a
/// transcription rather than a dead hotkey; falling back *to an engine that can
/// actually run* matters because Whisper — the obvious fallback — is itself
/// unavailable in a build compiled without `local-whisper-runtime` (slugtale-bre).
///
/// Dictation Readiness and the Second Opinion router both ask through here, so
/// Settings cannot report ready while the router quietly picks an engine that
/// fails at transcription. An engine missing from `availability` is treated as
/// unavailable: this build never resolved a provider for it.
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

/// Why dictation cannot transcribe at all, worded for Settings. `None` when some
/// engine can run.
///
/// It quotes the preferred engine's own reason, because that is the engine the
/// user chose and the reason they can act on — "the model has not been
/// downloaded" is a different instruction from "this build has no Whisper". The
/// reasons are non-content by construction ([`EngineUnavailable`]), so this is
/// safe to render and to log.
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

/// How long a recording runs. The Second Opinion router compares this against
/// the transcript length to catch an engine that returned far too little text
/// for the speech it was given.
pub fn captured_audio_duration(audio: &CapturedAudio) -> Duration {
    if audio.sample_rate_hz == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(audio.samples.len() as f64 / f64::from(audio.sample_rate_hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ids_are_stable_and_distinct() {
        // The Settings File and non-content diagnostics persist these ids, so a
        // rename would silently reset a user's engine choice.
        let ids: Vec<&str> = TranscriptionEngine::ALL.iter().map(|e| e.id()).collect();
        assert_eq!(ids, vec!["whisper", "parakeet", "apple-speech"]);

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
        // Settings offers an install action for exactly one of these; the rest
        // need a different machine or a different build, so offering a button
        // would be a lie.
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
        // A dictation is usually wrong in one place rather than uniformly, so a
        // healthy mean must not hide a single badly heard name.
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
        // An engine that reports nothing is not an uncertain engine.
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
        // A malformed recording must not divide by zero on the dictation path.
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
        // The user's chosen engine lost its assets. Refusing to transcribe would
        // punish them for a setting they may not remember making.
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
        // The bug this exists to stop (slugtale-bre): a default-feature build
        // compiles no Whisper runtime, so falling back to Whisper produces a
        // dictation that fails at transcription rather than one that works.
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
        // Settings can be carrying an engine whose provider this build never
        // registered; absence from the list is unavailability, not silence.
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
        // Nothing resolved a provider for the chosen engine, so there is no
        // engine-authored reason to quote.
        assert_eq!(
            engine_blocked_reason(TranscriptionEngine::AppleSpeech, &[]),
            Some("Apple SpeechTranscriber cannot run in this build.".to_string())
        );
    }

    #[test]
    fn a_plain_engine_result_reports_no_confidence_and_no_alternatives() {
        let result = EngineTranscription::plain(
            TranscriptionEngine::Whisper,
            FinalTranscription {
                text: "hello from slugtale".to_string(),
            },
            Duration::from_millis(240),
        );

        assert_eq!(result.text(), "hello from slugtale");
        assert!(result.alternatives.is_empty());
        assert_eq!(result.confidence, EngineConfidence::unreported());
    }
}
