//! The loaded Whisper model and the serialization that keeps its lifetime
//! honest.
//!
//! [`LocalWhisperRuntime`] owns the model reference and the mutex that decides
//! when the loaded context exists: it initializes the context once, reuses it
//! across transcriptions, and takes the context lock before dropping it so a
//! transcription or a Metal initialization cannot race process exit. When this
//! build has no `local-whisper-runtime` feature the same type answers with the
//! reason it cannot decode, so the engine catalogue can still report a Whisper
//! engine it cannot run.
//!
//! The whisper.cpp calls themselves — the context, the parameters, the segment
//! iterator — live in [`super::whisper_ffi`]. Nothing here names a binding, and
//! nothing here knows which caller is asking, or with what Transcription Speed
//! Profile: that is the caller's parameter, never runtime state, because the
//! runtime is shared.

#[cfg(feature = "local-whisper-runtime")]
use super::machine_probe::recommended_whisper_decode_settings;
#[cfg(feature = "local-whisper-runtime")]
use super::whisper_ffi::{self, RawSegment, WhisperContext};
use crate::{AsrError, LocalModelRef};
use crate::{CapturedAudio, FinalTranscription, SpeedProfile};
#[cfg(feature = "local-whisper-runtime")]
use std::sync::Mutex;

/// The loaded Whisper model and nothing else. The Transcription Speed Profile
/// deliberately lives outside this type: the runtime is shared by every caller
/// whose Settings name the same model path, and a decode strategy stored here
/// would be whichever caller spoke last.
pub struct LocalWhisperRuntime {
    model: LocalModelRef,
    // The loaded model is expensive to read and parse, so it is initialized once
    // and reused across transcriptions rather than rebuilt on every call. The
    // mutex also owns the model lifetime: shutdown takes it before dropping the
    // context, so Metal initialization or transcription cannot race process exit.
    #[cfg(feature = "local-whisper-runtime")]
    context: Mutex<Option<WhisperContext>>,
    #[cfg(feature = "local-whisper-runtime")]
    shutting_down: std::sync::atomic::AtomicBool,
}

impl LocalWhisperRuntime {
    pub fn new(model: LocalModelRef) -> Self {
        Self {
            model,
            #[cfg(feature = "local-whisper-runtime")]
            context: Mutex::new(None),
            #[cfg(feature = "local-whisper-runtime")]
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn model(&self) -> &LocalModelRef {
        &self.model
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl LocalWhisperRuntime {
    pub fn warm_up(&self) -> Result<(), AsrError> {
        self.with_context(|_| Ok(()))
    }

    /// Run an operation while owning the cached context's lifecycle lock. This
    /// serializes shutdown with both initialization and decoding, ensuring the
    /// Metal context is never used while it is being explicitly released.
    fn with_context<T>(
        &self,
        operation: impl FnOnce(&WhisperContext) -> Result<T, AsrError>,
    ) -> Result<T, AsrError> {
        use std::sync::atomic::Ordering;

        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "local Whisper runtime is shutting down".to_string(),
            ));
        }

        let mut context = self
            .context
            .lock()
            .map_err(|_| AsrError::Runtime("whisper context mutex poisoned".to_string()))?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "local Whisper runtime is shutting down".to_string(),
            ));
        }

        if context.is_none() {
            if !self.model.is_present() {
                return Err(AsrError::ModelMissing {
                    path: self.model.path().to_path_buf(),
                });
            }

            let model_path =
                self.model.path().to_str().ok_or_else(|| {
                    AsrError::Runtime("model path is not valid UTF-8".to_string())
                })?;
            *context = Some(whisper_ffi::open_context(model_path)?);
        }

        operation(context.as_ref().expect("context was just initialized"))
    }

    /// Drop the loaded context, once, whichever half of the build this is.
    /// The runtime cache owns this call: it is the only thing that decides when
    /// the shared model is released.
    pub(super) fn shutdown(&self) {
        use std::sync::atomic::Ordering;

        self.shutting_down.store(true, Ordering::Release);
        let mut context = match self.context.lock() {
            Ok(context) => context,
            Err(poisoned) => poisoned.into_inner(),
        };
        context.take();
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl LocalWhisperRuntime {
    /// Transcribe `audio` with the caller's Transcription Speed Profile. The
    /// profile is a parameter rather than runtime state so two callers sharing
    /// one loaded model can decode differently at the same moment.
    pub fn transcribe(
        &self,
        audio: CapturedAudio,
        speed_profile: SpeedProfile,
    ) -> Result<FinalTranscription, AsrError> {
        self.with_context(|context| {
            let segments: Vec<RawSegment> = whisper_ffi::decode(
                context,
                &audio.samples,
                recommended_whisper_decode_settings(speed_profile),
            )?;
            Ok(whisper_ffi::transcript_from_whisper_segments(segments))
        })
    }
}

#[cfg(not(feature = "local-whisper-runtime"))]
impl LocalWhisperRuntime {
    pub fn warm_up(&self) -> Result<(), AsrError> {
        self.require_present_model()?;
        Err(local_whisper_runtime_disabled_error())
    }

    pub fn transcribe(
        &self,
        _audio: CapturedAudio,
        _speed_profile: SpeedProfile,
    ) -> Result<FinalTranscription, AsrError> {
        self.require_present_model()?;
        Err(local_whisper_runtime_disabled_error())
    }

    fn require_present_model(&self) -> Result<(), AsrError> {
        if self.model.is_present() {
            return Ok(());
        }
        Err(AsrError::ModelMissing {
            path: self.model.path().to_path_buf(),
        })
    }

    pub(super) fn shutdown(&self) {}
}

#[cfg(not(feature = "local-whisper-runtime"))]
fn local_whisper_runtime_disabled_error() -> AsrError {
    AsrError::Runtime(
        "local Whisper runtime was built without the local-whisper-runtime feature".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::unique_test_dir;
    use super::*;
    use crate::DEFAULT_MODEL_FILENAME;

    #[test]
    fn local_whisper_runtime_reports_missing_model_before_transcription() {
        let model_path = unique_test_dir("missing-model").join(DEFAULT_MODEL_FILENAME);
        let runtime = LocalWhisperRuntime::new(LocalModelRef::at(model_path.clone()));

        let error = runtime
            .transcribe(
                CapturedAudio::mono_16khz(vec![0.0; 16_000]),
                SpeedProfile::default(),
            )
            .unwrap_err();

        assert_eq!(error, AsrError::ModelMissing { path: model_path });
    }
}
