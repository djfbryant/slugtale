//! Whisper as a Transcription Engine: the provider the dictation path and the
//! Second Opinion router see.
//!
//! This is the dictation half only — which engine it is, what it is licensed
//! as, whether it can run, and how to make it transcribe. The asset lifecycle
//! (measure, install, remove the Local Model) lives in [`super::whisper_assets`],
//! because nothing that transcribes has any business deleting a model.

use crate::{AsrError, CapturedAudio, EngineTranscriber, LocalWhisperRuntime, SpeedProfile};
use std::sync::Arc;

/// Presents the established Whisper runtime through the Transcription Engine
/// boundary so the Second Opinion router can treat it like any other engine.
///
/// This adapter adds no decoding work: it times the existing call and reports
/// no confidence, because whisper.cpp's segment iterator gives Slugtale plain
/// text today. That is why Whisper can only ever be escalated *from* on the
/// anomaly rules (empty output, repetition, implausibly short text), never on a
/// confidence threshold — see [`crate::EngineConfidence`].
///
/// The Transcription Speed Profile belongs here rather than on the runtime
/// behind it. The runtime is one loaded model shared by every caller whose
/// Settings name the same path, and a profile stored there is whichever caller
/// spoke last: a wake check on the always-listening microphone decoding greedily
/// would silently re-decode the next dictation too.
///
/// The Local Model Manager comes along for the same reason the profile does:
/// Whisper's assets *are* the Local Model, so installing and removing them is
/// this engine's business rather than something Settings has to know. `None`
/// before startup hands one over, which leaves the engine with no install path
/// and no bytes to report — an honest answer, and the reason
/// [`crate::EngineAssetLifecycle::can_install_assets`] asks.
pub struct WhisperTranscriptionProvider {
    runtime: Arc<LocalWhisperRuntime>,
    speed_profile: SpeedProfile,
    model_manager: Option<crate::LocalModelManager>,
}

impl WhisperTranscriptionProvider {
    pub fn new(
        runtime: Arc<LocalWhisperRuntime>,
        speed_profile: SpeedProfile,
        model_manager: Option<crate::LocalModelManager>,
    ) -> Self {
        Self {
            runtime,
            speed_profile,
            model_manager,
        }
    }

    /// The Transcription Speed Profile this provider decodes with. Read by the
    /// engine catalogue's tests to prove one caller's profile cannot reach
    /// another's; the runtime it wraps no longer knows the value at all.
    pub fn speed_profile(&self) -> SpeedProfile {
        self.speed_profile
    }

    /// The Local Model Manager the asset lifecycle half reads. `None` before
    /// startup hands one over, which leaves the engine with no install path and
    /// no bytes to report.
    pub(super) fn model_manager(&self) -> Option<&crate::LocalModelManager> {
        self.model_manager.as_ref()
    }
}

impl EngineTranscriber for WhisperTranscriptionProvider {
    fn engine(&self) -> crate::TranscriptionEngine {
        crate::TranscriptionEngine::Whisper
    }

    fn metadata(&self) -> crate::EngineMetadata {
        crate::EngineMetadata {
            engine: crate::TranscriptionEngine::Whisper,
            model_id: crate::DEFAULT_MODEL_ID,
            capability: "General-purpose English dictation on a modest model that runs on \
                         every platform Slugtale supports. A dependable default for quick \
                         notes, short messages, and everyday commands.",
            revision: "ggerganov/whisper.cpp@main",
            approximate_bytes: Some(148 * 1024 * 1024),
            source_url: Some(crate::DEFAULT_MODEL_DOWNLOAD_URL),
            license: "MIT",
            license_url: "https://github.com/openai/whisper/blob/main/LICENSE",
            attribution: None,
            modifications: Some("Converted to the GGML format by the whisper.cpp project."),
            system_managed: false,
            supported_platforms: "macOS, Windows, and Linux",
        }
    }

    fn availability(&self) -> crate::EngineAvailability {
        if !cfg!(feature = "local-whisper-runtime") {
            return crate::EngineAvailability::Unavailable(
                crate::EngineUnavailable::RuntimeNotBuilt,
            );
        }
        if !self.runtime.model().is_present() {
            return crate::EngineAvailability::Unavailable(
                crate::EngineUnavailable::AssetsMissing {
                    detail: "The Whisper model has not been downloaded yet.".to_string(),
                },
            );
        }
        crate::EngineAvailability::Available
    }

    fn warm_up(&self) -> Result<(), AsrError> {
        self.runtime.warm_up()
    }

    fn transcribe(&self, audio: &CapturedAudio) -> Result<crate::EngineTranscription, AsrError> {
        let started = std::time::Instant::now();
        // The runtime still owns its audio, so this clone is the cost of giving
        // every provider a borrowing signature. It only happens on the Whisper
        // leg; the router never clones for the engines that borrow natively.
        let transcription = self.runtime.transcribe(audio.clone(), self.speed_profile)?;
        Ok(crate::EngineTranscription::plain(
            crate::TranscriptionEngine::Whisper,
            transcription,
            started.elapsed(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::unique_test_dir;
    use super::*;
    use crate::{LocalModelRef, DEFAULT_MODEL_FILENAME};

    #[test]
    fn one_loaded_model_serves_providers_that_decode_differently() {
        // The whole point of putting the profile on the provider: two callers
        // share the model context and neither can move the other's Beam Search.
        let runtime = Arc::new(LocalWhisperRuntime::new(LocalModelRef::at(
            unique_test_dir("shared-model-profiles").join(DEFAULT_MODEL_FILENAME),
        )));

        let fast =
            WhisperTranscriptionProvider::new(Arc::clone(&runtime), SpeedProfile::Fast, None);
        let accurate =
            WhisperTranscriptionProvider::new(Arc::clone(&runtime), SpeedProfile::Accurate, None);

        assert_eq!(fast.speed_profile(), SpeedProfile::Fast);
        assert_eq!(accurate.speed_profile(), SpeedProfile::Accurate);
    }
}
