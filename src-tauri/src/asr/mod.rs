
mod machine_probe;
mod whisper_assets;
mod whisper_cache;
mod whisper_engine;
mod whisper_ffi;
mod whisper_runtime;

pub use whisper_cache::WhisperRuntimeCache;
pub use whisper_engine::WhisperTranscriptionProvider;
pub use whisper_runtime::LocalWhisperRuntime;

use crate::CapturedAudio;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalTranscription {
    pub text: String,
    /// The ordered recognized segments behind [`FinalTranscription::text`].
    /// Empty when an engine exposes no segment timing (or in tests where
    /// timing is irrelevant); cleanup must treat a flattened transcript with
    /// no segments exactly like before.
    #[serde(default)]
    pub segments: Vec<TranscriptSegment>,
}

impl FinalTranscription {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            segments: Vec::new(),
        }
    }

    /// Flatten ordered segments into the final text. Joining is concatenation
    /// followed by one overall trim — exactly what the Whisper runtime did
    /// before segments were preserved — so existing behavior is unchanged
    /// when no cleanup consumes the boundaries yet.
    pub fn from_segments(segments: Vec<TranscriptSegment>) -> Self {
        let text = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>()
            .trim()
            .to_string();
        Self { text, segments }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsrError {
    ModelMissing {
        path: std::path::PathBuf,
    },
    UnsupportedAudio(String),
    Runtime(String),
    /// A Transcription Engine was asked to transcribe on a machine or a build
    /// where it cannot run. Kept distinct from [`AsrError::Runtime`] so the
    /// Second Opinion router can tell "this engine is not for you" from "this
    /// engine broke", and fall back without reporting a failure to the user.
    EngineUnavailable {
        engine: crate::TranscriptionEngine,
        reason: crate::EngineUnavailable,
    },
    Timeout {
        engine: crate::TranscriptionEngine,
    },
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModelMissing { path } => {
                write!(f, "local model is missing at {}", path.display())
            }
            Self::UnsupportedAudio(message) => write!(f, "unsupported captured audio: {message}"),
            Self::Runtime(message) => write!(f, "local transcription failed: {message}"),
            Self::EngineUnavailable { engine, reason } => {
                write!(f, "{engine} is unavailable: {reason}")
            }
            Self::Timeout { engine } => write!(f, "{engine} did not finish in time"),
        }
    }
}

impl std::error::Error for AsrError {}

pub trait AsrRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError>;
}

pub fn transcribe_captured_audio(
    runtime: &dyn AsrRuntime,
    audio: CapturedAudio,
) -> Result<FinalTranscription, AsrError> {
    if audio.sample_rate_hz != 16_000 {
        return Err(AsrError::UnsupportedAudio(
            "Whisper transcription expects 16 kHz mono f32 samples".to_string(),
        ));
    }

    runtime.transcribe(audio)
}

#[cfg(test)]
pub(crate) fn unique_test_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "slugtale-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_captured_audio_returns_final_transcription_from_asr_runtime() {
        let runtime = FakeAsrRuntime::new("hello from slugtale");
        let audio = CapturedAudio::mono_16khz(vec![0.0, 0.25, -0.25]);

        let transcription = transcribe_captured_audio(&runtime, audio).unwrap();

        assert_eq!(transcription.text, "hello from slugtale");
        assert_eq!(runtime.sample_counts.borrow().as_slice(), &[3]);
    }

    #[test]
    fn transcribe_captured_audio_rejects_non_16khz_audio_before_runtime() {
        let runtime = FakeAsrRuntime::new("should not run");
        let audio = CapturedAudio {
            sample_rate_hz: 44_100,
            samples: vec![0.0],
        };

        let error = transcribe_captured_audio(&runtime, audio).unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Whisper transcription expects 16 kHz mono f32 samples".to_string()
            )
        );
        assert!(runtime.sample_counts.borrow().is_empty());
    }

    struct FakeAsrRuntime {
        text: &'static str,
        sample_counts: std::cell::RefCell<Vec<usize>>,
    }

    impl FakeAsrRuntime {
        fn new(text: &'static str) -> Self {
            Self {
                text,
                sample_counts: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl AsrRuntime for FakeAsrRuntime {
        fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
            self.sample_counts.borrow_mut().push(audio.samples.len());
            Ok(FinalTranscription::plain(self.text))
        }
    }
}
