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
//! - **Non-content diagnostics** — engine identity, availability reasons, asset
//!   accounting, latency, and escalation reason codes. These are safe to log and
//!   to render in Settings, and every type carrying them is a closed enum or a
//!   number so a caller cannot accidentally smuggle speech through them. The
//!   Settings surface asks four questions of an engine — what it is licensed as
//!   ([`EngineMetadata`]), whether it can run ([`EngineAvailability`]), what it
//!   has on disk ([`EngineAssets`]), and whether its assets can be installed or
//!   removed — and the [`EngineView`] row is built from nothing else.

use crate::{AsrError, CapturedAudio, DownloadProgress, FinalTranscription};
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
    /// Fermion Research's Phonon-2: MLX on supported Apple silicon Macs,
    /// portable ONNX on other platforms and builds.
    Phonon,
    /// Apple SpeechTranscriber, system-managed and Apple-only (slugtale-vjs.2).
    AppleSpeech,
}

impl TranscriptionEngine {
    /// Every engine Slugtale knows about, in the order Settings lists them.
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

    /// The name shown to the user in Settings.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper base.en",
            Self::Parakeet => "Parakeet TDT v2",
            Self::Phonon => "Phonon-2",
            Self::AppleSpeech => "Apple SpeechTranscriber",
        }
    }

    /// Why Settings has no row for this engine, worded for the user.
    ///
    /// The catalogue resolves providers and knows only whether it could. Which
    /// prerequisite is missing is the engine's own fact, so the wording lives
    /// with the engine rather than at the place that noticed.
    pub fn missing_provider_reason(self) -> &'static str {
        match self {
            // Whisper opens one Local Model file, so a Settings File naming no
            // model and a catalogue holding no model directory leave it nothing
            // to open.
            Self::Whisper => "could not resolve a local model directory for Whisper",
            // The catalogue registers Parakeet and Phonon once it has a model
            // directory to put their assets in, and Apple when it is built, so
            // reaching this means Settings was asked before startup finished.
            Self::Parakeet | Self::Phonon | Self::AppleSpeech => {
                "transcription engines are not ready yet"
            }
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

/// Installed-asset accounting for one Transcription Engine.
///
/// Kept apart from [`EngineAvailability`] for two reasons: an engine can be
/// unavailable for reasons that have nothing to do with its assets (wrong
/// operating system, a build without its runtime), and the operating system
/// may own those bytes outright, in which case Slugtale measures neither how
/// many there are nor whether they are there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EngineAssets {
    /// Bytes on disk for assets Slugtale itself owns. `None` for Apple
    /// SpeechTranscriber, whose assets Slugtale never downloads or measures.
    pub installed_bytes: Option<u64>,
    /// Whether Slugtale's own copy of the assets is fully installed. `None` for
    /// system-managed engines, where a `false` would be a guess and
    /// [`EngineAvailability`] is the honest answer instead.
    pub present: Option<bool>,
}

impl EngineAssets {
    /// Assets Slugtale neither measures nor owns: the operating system's, and any
    /// Slugtale cannot even see the directory of. The pair of `None`s says so by
    /// answering nothing, rather than reporting a zero nobody verified.
    pub fn unmeasured() -> Self {
        Self {
            installed_bytes: None,
            present: None,
        }
    }
}

/// What one install of a Transcription Engine's assets left for the Dictation
/// Runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetInstall {
    /// Whether the app should load the model the next dictation is about to
    /// need. True for the engine whose assets are the Local Model, so a cold
    /// load does not land on the user's first dictation; false for an engine
    /// the Dictation Runtime opens on first use anyway.
    pub warm_up: bool,
}

/// What Settings needs to render one row of the Transcription Engines list
/// (slugtale-vjs.4): whether it is the current primary, its licence and
/// provenance from [`EngineMetadata`], whether it can run right now, and how
/// much of its assets are actually on disk.
///
/// It mirrors `EngineMetadata`/`EngineAvailability` rather than replacing them —
/// Settings renders the licence and attribution strings straight out of
/// `metadata` so the CC BY 4.0 wording is never retyped in the frontend. It
/// knows no engine: every fact is asked of the engine's own provider, so a
/// fourth engine changes nothing here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineView {
    pub id: &'static str,
    pub display_name: &'static str,
    pub is_primary: bool,
    pub metadata: EngineMetadata,
    pub availability: EngineAvailability,
    /// `availability`'s reason rendered through [`EngineUnavailable`]'s
    /// `Display`, so Settings shows the same wording the rest of Slugtale does
    /// rather than re-deriving copy per reason code in JavaScript. `None` when
    /// the engine is available.
    pub unavailable_reason: Option<String>,
    /// Whether Settings should offer an Install action right now. Both halves
    /// have to agree: the engine's reason must be one the user can fix
    /// ([`EngineUnavailable::is_user_resolvable`], so never an unsupported
    /// operating system or a build without the feature) *and* the engine must
    /// have a way to fetch its assets
    /// ([`TranscriptionProvider::can_install_assets`]). A row therefore never
    /// offers a button that can only refuse.
    pub installable: bool,
    pub assets: EngineAssets,
}

impl EngineView {
    /// One row, asked of the engine's own provider plus whether the Settings
    /// File names it the primary. Answers from the same cached availability the
    /// dictation path reads, so Settings and the Dictation Runtime cannot
    /// disagree about an engine.
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
/// the filesystem or the OS on every dictation. The asset methods below are the
/// deliberate exception: only Settings asks them, their answer changes only when
/// the user installs or removes something, and Settings cannot render an honest
/// row without them.
pub trait TranscriptionProvider: Send + Sync {
    fn engine(&self) -> TranscriptionEngine;

    fn metadata(&self) -> EngineMetadata;

    fn availability(&self) -> EngineAvailability;

    /// How much of this engine's assets are on disk, in bytes and in full.
    /// An engine the operating system owns answers
    /// [`EngineAssets::system_managed`].
    fn assets(&self) -> EngineAssets;

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError>;

    /// Load whatever this engine needs before the first transcription so the
    /// first dictation does not pay for it. Engines with nothing to load —
    /// Apple Speech, test fakes — inherit this no-op.
    fn warm_up(&self) -> Result<(), AsrError> {
        Ok(())
    }

    /// Whether this engine has a way to fetch its own assets. False by default,
    /// because an engine that cannot install must not look installable in
    /// Settings; every engine that can install says so.
    fn can_install_assets(&self) -> bool {
        false
    }

    /// Fetch this engine's assets as an explicit user action, reporting download
    /// progress on `on_progress` for the engines that download. The honest
    /// default refuses: an engine with no download behind it must not look like
    /// a silent success.
    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        let _ = on_progress;
        Err(assets_cannot_be_installed(self.engine()))
    }

    /// Delete this engine's installed assets as an explicit user action. An
    /// engine whose bytes belong to the operating system overrides this with its
    /// own refusal rather than pretending to free space Slugtale never claimed.
    fn remove_assets(&self) -> Result<(), String> {
        Err(assets_cannot_be_removed(self.engine()))
    }
}

/// The refusal an engine gives when Settings asks it to fetch assets it has no
/// way to fetch. Also the answer an engine gives that *has* a mechanism it
/// cannot reach, so one wording covers both.
pub(crate) fn assets_cannot_be_installed(engine: TranscriptionEngine) -> String {
    format!(
        "{} has no installation path in Slugtale.",
        engine.display_name()
    )
}

/// The refusal an engine gives when there is nothing of its own on disk to
/// delete.
pub(crate) fn assets_cannot_be_removed(engine: TranscriptionEngine) -> String {
    format!(
        "{} has no assets for Slugtale to remove.",
        engine.display_name()
    )
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

    /// A provider that answers whatever a test needs it to, so the Settings row
    /// can be asked about any engine state without a running app, a models
    /// directory, or a real engine.
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

    impl TranscriptionProvider for StatedProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.engine
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.engine,
                model_id: "test",
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

        fn assets(&self) -> EngineAssets {
            self.assets
        }

        fn can_install_assets(&self) -> bool {
            self.can_install
        }

        fn transcribe(&self, _audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
            Err(AsrError::Runtime(
                "this provider never transcribes".to_string(),
            ))
        }
    }

    fn missing_assets() -> EngineAvailability {
        EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
            detail: "The model has not been downloaded yet.".to_string(),
        })
    }

    #[test]
    fn a_system_managed_engine_reports_no_bytes_and_never_an_install_slugtale_does() {
        // Apple SpeechTranscriber's weights are macOS's. Reporting a size Slugtale
        // never measured, or offering a download Slugtale cannot perform, would
        // describe assets Slugtale does not own.
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

        // The only thing that may put an Install button on a system-managed row is
        // the engine's own reason: on a fresh macOS install that reason is
        // `AssetsMissing`, and macOS — not Slugtale — is what installing means.
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
        // Every reason the user cannot fix from Settings, with an install path
        // behind it: a button here would dead-end.
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

        // Missing assets with an install path behind them: the one true case.
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
        // Missing assets, but nothing behind them to fetch the bytes with. The
        // row must not offer a button whose only outcome is a refusal.
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
        // A provider that inherits them has no download and nothing of its own to
        // delete. A silent `Ok(())` would be a Settings row that frees space
        // Slugtale never claimed.
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
        // The frontend reads these names out of the serialised row, so renaming
        // one is a silent breakage in the Settings surface.
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
        // Settings shows these verbatim when the catalogue resolved no provider.
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
        // The Settings File and non-content diagnostics persist these ids, so a
        // rename would silently reset a user's engine choice.
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
            FinalTranscription::plain("hello from slugtale"),
            Duration::from_millis(240),
        );

        assert_eq!(result.text(), "hello from slugtale");
        assert!(result.alternatives.is_empty());
        assert_eq!(result.confidence, EngineConfidence::unreported());
    }
}
