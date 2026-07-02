use crate::{CapturedAudio, SpeedProfile};
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "local-whisper-runtime"))]
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalTranscription {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsrError {
    ModelMissing { path: std::path::PathBuf },
    UnsupportedAudio(String),
    Runtime(String),
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModelMissing { path } => {
                write!(f, "local model is missing at {}", path.display())
            }
            Self::UnsupportedAudio(message) => write!(f, "unsupported captured audio: {message}"),
            Self::Runtime(message) => write!(f, "local transcription failed: {message}"),
        }
    }
}

impl std::error::Error for AsrError {}

pub trait AsrRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError>;
}

#[cfg(any(test, feature = "local-whisper-runtime"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhisperDecodeStrategy {
    Greedy { best_of: i32 },
    BeamSearch { beam_size: i32 },
}

#[cfg(any(test, feature = "local-whisper-runtime"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WhisperDecodeSettings {
    strategy: WhisperDecodeStrategy,
    n_threads: i32,
}

/// Map a Transcription Speed Profile to the decode strategy the local Whisper
/// runtime uses: a wider Beam Search is more accurate but slower (CONTEXT.md);
/// Fast skips Beam Search entirely with greedy decoding. Values were picked
/// from measured latency on real speech clips — greedy is fastest, beam 2 costs
/// little over greedy, and beam 5 (the pre-profile default) is 25-45% slower on
/// longer clips (docs/research/whisper-decode-benchmark.md). Note whisper.cpp
/// ignores greedy `best_of` at its default temperature, so meaningfully wider
/// search requires the BeamSearch strategy, not a larger `best_of`.
#[cfg(any(test, feature = "local-whisper-runtime"))]
fn decode_strategy_for_speed_profile(profile: SpeedProfile) -> WhisperDecodeStrategy {
    match profile {
        SpeedProfile::Fast => WhisperDecodeStrategy::Greedy { best_of: 1 },
        SpeedProfile::Balanced => WhisperDecodeStrategy::BeamSearch { beam_size: 2 },
        SpeedProfile::Accurate => WhisperDecodeStrategy::BeamSearch { beam_size: 5 },
    }
}

#[cfg(feature = "local-whisper-runtime")]
fn recommended_whisper_decode_settings(profile: SpeedProfile) -> WhisperDecodeSettings {
    whisper_decode_settings_for_available_threads(
        profile,
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
    )
}

#[cfg(any(test, feature = "local-whisper-runtime"))]
fn whisper_decode_settings_for_available_threads(
    profile: SpeedProfile,
    available_threads: NonZeroUsize,
) -> WhisperDecodeSettings {
    WhisperDecodeSettings {
        strategy: decode_strategy_for_speed_profile(profile),
        n_threads: available_threads.get().min(i32::MAX as usize) as i32,
    }
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

pub struct LocalWhisperRuntime {
    model_path: std::path::PathBuf,
    // The Transcription Speed Profile sets the decode strategy per call. It is
    // interior-mutable so the caller can update it from settings without
    // rebuilding the cached model context, which the profile does not affect.
    speed_profile: Mutex<SpeedProfile>,
    // The loaded model is expensive to read and parse, so it is initialized once
    // and reused across transcriptions rather than rebuilt on every call.
    #[cfg(feature = "local-whisper-runtime")]
    context: std::sync::OnceLock<whisper_rs::WhisperContext>,
    #[cfg(feature = "local-whisper-runtime")]
    context_init: Mutex<()>,
}

impl LocalWhisperRuntime {
    pub fn new(model_path: std::path::PathBuf) -> Self {
        Self {
            model_path,
            speed_profile: Mutex::new(SpeedProfile::default()),
            #[cfg(feature = "local-whisper-runtime")]
            context: std::sync::OnceLock::new(),
            #[cfg(feature = "local-whisper-runtime")]
            context_init: Mutex::new(()),
        }
    }

    pub fn model_path(&self) -> &std::path::Path {
        &self.model_path
    }

    /// Set the Transcription Speed Profile applied to subsequent transcriptions.
    /// Callers set this from the current Settings File before each dictation so
    /// the accuracy/speed choice takes effect without reloading the model.
    pub fn set_speed_profile(&self, profile: SpeedProfile) {
        match self.speed_profile.lock() {
            Ok(mut current) => *current = profile,
            Err(poisoned) => *poisoned.into_inner() = profile,
        }
    }

    #[cfg(any(test, feature = "local-whisper-runtime"))]
    fn speed_profile(&self) -> SpeedProfile {
        match self.speed_profile.lock() {
            Ok(profile) => *profile,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

/// Caches the loaded Whisper runtime across transcriptions so the model file is
/// read from disk once rather than on every call. The runtime is rebuilt only
/// when the configured model path changes.
#[derive(Default)]
pub struct WhisperRuntimeCache(Mutex<WhisperRuntimeCacheState>);

#[derive(Default)]
struct WhisperRuntimeCacheState {
    runtime: Option<Arc<LocalWhisperRuntime>>,
    warming_model_path: Option<std::path::PathBuf>,
}

impl WhisperRuntimeCache {
    pub fn runtime_for(&self, model_path: &std::path::Path) -> Arc<LocalWhisperRuntime> {
        let mut state = self.0.lock().expect("whisper runtime cache mutex poisoned");
        Self::runtime_for_locked(&mut state, model_path)
    }

    pub fn begin_warming_existing_model(
        &self,
        model_path: &std::path::Path,
    ) -> Option<Arc<LocalWhisperRuntime>> {
        if !model_path.exists() {
            return None;
        }

        let mut state = self.0.lock().expect("whisper runtime cache mutex poisoned");
        if state.warming_model_path.as_deref() == Some(model_path) {
            return None;
        }

        let runtime = Self::runtime_for_locked(&mut state, model_path);
        state.warming_model_path = Some(model_path.to_path_buf());
        Some(runtime)
    }

    fn runtime_for_locked(
        state: &mut WhisperRuntimeCacheState,
        model_path: &std::path::Path,
    ) -> Arc<LocalWhisperRuntime> {
        if let Some(existing) = state.runtime.as_ref() {
            if existing.model_path() == model_path {
                return existing.clone();
            }
        }

        let runtime = Arc::new(LocalWhisperRuntime::new(model_path.to_path_buf()));
        state.runtime = Some(runtime.clone());
        runtime
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl LocalWhisperRuntime {
    pub fn warm_up(&self) -> Result<(), AsrError> {
        self.context().map(|_| ())
    }

    /// Return the loaded Whisper context, reading the model file from disk only
    /// on the first call and caching it for subsequent transcriptions.
    fn context(&self) -> Result<&whisper_rs::WhisperContext, AsrError> {
        if let Some(context) = self.context.get() {
            return Ok(context);
        }

        let _guard = self
            .context_init
            .lock()
            .map_err(|_| AsrError::Runtime("whisper context mutex poisoned".to_string()))?;
        if let Some(context) = self.context.get() {
            return Ok(context);
        }

        if !self.model_path.exists() {
            return Err(AsrError::ModelMissing {
                path: self.model_path.clone(),
            });
        }

        let model_path = self
            .model_path
            .to_str()
            .ok_or_else(|| AsrError::Runtime("model path is not valid UTF-8".to_string()))?;
        let context = whisper_rs::WhisperContext::new_with_params(
            model_path,
            whisper_rs::WhisperContextParameters::default(),
        )
        .map_err(|error| AsrError::Runtime(error.to_string()))?;

        // If another thread won the race to initialize, keep the stored context.
        let _ = self.context.set(context);
        Ok(self.context.get().expect("context was just initialized"))
    }
}

#[cfg(feature = "local-whisper-runtime")]
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        let context = self.context()?;
        let mut state = context
            .create_state()
            .map_err(|error| AsrError::Runtime(error.to_string()))?;
        let decode_settings = recommended_whisper_decode_settings(self.speed_profile());
        let mut params = whisper_rs::FullParams::new(match decode_settings.strategy {
            WhisperDecodeStrategy::Greedy { best_of } => {
                whisper_rs::SamplingStrategy::Greedy { best_of }
            }
            WhisperDecodeStrategy::BeamSearch { beam_size } => {
                whisper_rs::SamplingStrategy::BeamSearch {
                    beam_size,
                    // whisper.cpp's default patience (unbounded beam pruning off).
                    patience: -1.0,
                }
            }
        });

        params.set_n_threads(decode_settings.n_threads);
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &audio.samples)
            .map_err(|error| AsrError::Runtime(error.to_string()))?;

        let text = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();

        Ok(FinalTranscription { text })
    }
}

#[cfg(not(feature = "local-whisper-runtime"))]
impl LocalWhisperRuntime {
    pub fn warm_up(&self) -> Result<(), AsrError> {
        if !self.model_path.exists() {
            return Err(AsrError::ModelMissing {
                path: self.model_path.clone(),
            });
        }

        Err(local_whisper_runtime_disabled_error())
    }
}

#[cfg(not(feature = "local-whisper-runtime"))]
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, _audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        if !self.model_path.exists() {
            return Err(AsrError::ModelMissing {
                path: self.model_path.clone(),
            });
        }

        Err(local_whisper_runtime_disabled_error())
    }
}

#[cfg(not(feature = "local-whisper-runtime"))]
fn local_whisper_runtime_disabled_error() -> AsrError {
    AsrError::Runtime(
        "local Whisper runtime was built without the local-whisper-runtime feature".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_MODEL_FILENAME;

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
    #[test]
    fn local_whisper_runtime_reports_missing_model_before_transcription() {
        let model_path = unique_test_dir("missing-model").join(DEFAULT_MODEL_FILENAME);
        let runtime = LocalWhisperRuntime::new(model_path.clone());

        let error = runtime
            .transcribe(CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .unwrap_err();

        assert_eq!(error, AsrError::ModelMissing { path: model_path });
    }

    #[test]
    fn speed_profiles_map_to_progressively_wider_decode_search() {
        // Mapping chosen from measured latency on real speech clips
        // (docs/research/whisper-decode-benchmark.md): Fast skips Beam Search
        // entirely, Balanced uses a narrow beam, Accurate uses the widest beam.
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Fast),
            WhisperDecodeStrategy::Greedy { best_of: 1 }
        );
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Balanced),
            WhisperDecodeStrategy::BeamSearch { beam_size: 2 }
        );
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Accurate),
            WhisperDecodeStrategy::BeamSearch { beam_size: 5 }
        );
    }

    #[test]
    fn decode_settings_use_selected_profile_and_available_threads() {
        let threads = NonZeroUsize::new(4).unwrap();
        let settings =
            whisper_decode_settings_for_available_threads(SpeedProfile::Accurate, threads);

        assert_eq!(
            settings.strategy,
            WhisperDecodeStrategy::BeamSearch { beam_size: 5 }
        );
        assert_eq!(settings.n_threads, 4);
    }

    #[test]
    fn runtime_defaults_to_balanced_profile_and_accepts_updates() {
        let runtime =
            LocalWhisperRuntime::new(unique_test_dir("profile").join(DEFAULT_MODEL_FILENAME));
        assert_eq!(runtime.speed_profile(), SpeedProfile::Balanced);

        runtime.set_speed_profile(SpeedProfile::Fast);
        assert_eq!(runtime.speed_profile(), SpeedProfile::Fast);
    }

    #[test]
    fn whisper_runtime_cache_reuses_runtime_for_same_model_path() {
        let cache = WhisperRuntimeCache::default();
        let model_path = unique_test_dir("whisper-cache").join(DEFAULT_MODEL_FILENAME);

        let first = cache.runtime_for(&model_path);
        let second = cache.runtime_for(&model_path);

        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn whisper_runtime_cache_rebuilds_runtime_when_model_path_changes() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-model-change");
        let first_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        let second_path = model_dir.join("custom-model.bin");

        let first = cache.runtime_for(&first_path);
        let second = cache.runtime_for(&second_path);

        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(second.model_path(), second_path);
    }

    #[test]
    fn whisper_runtime_cache_does_not_warm_missing_model() {
        let cache = WhisperRuntimeCache::default();
        let model_path = unique_test_dir("whisper-cache-missing").join(DEFAULT_MODEL_FILENAME);

        assert!(cache.begin_warming_existing_model(&model_path).is_none());
    }

    #[test]
    fn whisper_runtime_cache_warms_ready_model_once() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-ready");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&model_path, b"model").unwrap();

        let warmed = cache.begin_warming_existing_model(&model_path).unwrap();
        let duplicate = cache.begin_warming_existing_model(&model_path);
        let transcription_runtime = cache.runtime_for(&model_path);

        assert!(duplicate.is_none());
        assert!(std::sync::Arc::ptr_eq(&warmed, &transcription_runtime));

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn fast_profile_decode_settings_prioritize_low_latency_dictation() {
        let settings = whisper_decode_settings_for_available_threads(
            SpeedProfile::Fast,
            NonZeroUsize::new(10).unwrap(),
        );

        assert_eq!(
            settings,
            WhisperDecodeSettings {
                strategy: WhisperDecodeStrategy::Greedy { best_of: 1 },
                n_threads: 10,
            }
        );
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
            Ok(FinalTranscription {
                text: self.text.to_string(),
            })
        }
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
