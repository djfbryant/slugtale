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
    /// A Transcription Engine was asked to transcribe on a machine or a build
    /// where it cannot run. Kept distinct from [`AsrError::Runtime`] so the
    /// Second Opinion router can tell "this engine is not for you" from "this
    /// engine broke", and fall back without reporting a failure to the user.
    EngineUnavailable {
        engine: crate::TranscriptionEngine,
        reason: crate::EngineUnavailable,
    },
    /// A second opinion ran past its bounded budget. The router keeps the first
    /// usable transcript when this happens, so the user never waits on a slow
    /// engine (slugtale-vjs.3).
    Timeout { engine: crate::TranscriptionEngine },
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
        whisper_thread_count(
            num_cpus::get_physical(),
            std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
        ),
    )
}

/// How many threads Whisper decoding should use: the physical core count,
/// clamped to the parallelism this process may actually use. ggml's compute
/// threads contend on shared execution units, so running one per SMT sibling
/// is much slower than one per core — 4x slower on the 6C/12T Linux reference
/// machine (slugtale-jwy). `physical_cores` of 0 means detection failed; fall
/// back to the available parallelism.
#[cfg(any(test, feature = "local-whisper-runtime"))]
fn whisper_thread_count(physical_cores: usize, available: NonZeroUsize) -> NonZeroUsize {
    NonZeroUsize::new(physical_cores.min(available.get())).unwrap_or(available)
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
    // and reused across transcriptions rather than rebuilt on every call. The
    // mutex also owns the model lifetime: shutdown takes it before dropping the
    // context, so Metal initialization or transcription cannot race process exit.
    #[cfg(feature = "local-whisper-runtime")]
    context: Mutex<Option<whisper_rs::WhisperContext>>,
    #[cfg(feature = "local-whisper-runtime")]
    shutting_down: std::sync::atomic::AtomicBool,
}

impl LocalWhisperRuntime {
    pub fn new(model_path: std::path::PathBuf) -> Self {
        Self {
            model_path,
            speed_profile: Mutex::new(SpeedProfile::default()),
            #[cfg(feature = "local-whisper-runtime")]
            context: Mutex::new(None),
            #[cfg(feature = "local-whisper-runtime")]
            shutting_down: std::sync::atomic::AtomicBool::new(false),
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
    shutting_down: bool,
}

impl WhisperRuntimeCache {
    pub fn runtime_for(&self, model_path: &std::path::Path) -> Arc<LocalWhisperRuntime> {
        let mut state = self.0.lock().expect("whisper runtime cache mutex poisoned");
        let runtime = Self::runtime_for_locked(&mut state, model_path);
        if state.shutting_down {
            // A dictation task can race ExitRequested after obtaining the app
            // handle. Return a permanently stopped runtime so it cannot create
            // a new Metal context after shutdown has already drained the cache.
            runtime.shutdown();
        }
        runtime
    }

    pub fn begin_warming_existing_model(
        &self,
        model_path: &std::path::Path,
    ) -> Option<Arc<LocalWhisperRuntime>> {
        if !model_path.exists() {
            return None;
        }

        let mut state = self.0.lock().expect("whisper runtime cache mutex poisoned");
        if state.shutting_down {
            return None;
        }
        if state.warming_model_path.as_deref() == Some(model_path) {
            return None;
        }

        let runtime = Self::runtime_for_locked(&mut state, model_path);
        state.warming_model_path = Some(model_path.to_path_buf());
        Some(runtime)
    }

    /// Stop accepting model warm-up work and synchronously release the cached
    /// Whisper context. Tauri's default `run` path ends in `process::exit`, which
    /// skips Rust destructors; explicitly dropping here is therefore required
    /// before ggml's C++ Metal globals are torn down (slugtale-p1u).
    pub fn shutdown(&self) {
        let mut state = match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.shutting_down = true;
        state.warming_model_path = None;
        if let Some(runtime) = state.runtime.as_ref() {
            runtime.shutdown();
        }
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
        self.with_context(|_| Ok(()))
    }

    /// Run an operation while owning the cached context's lifecycle lock. This
    /// serializes shutdown with both initialization and decoding, ensuring the
    /// Metal context is never used while it is being explicitly released.
    fn with_context<T>(
        &self,
        operation: impl FnOnce(&whisper_rs::WhisperContext) -> Result<T, AsrError>,
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
            if !self.model_path.exists() {
                return Err(AsrError::ModelMissing {
                    path: self.model_path.clone(),
                });
            }

            let model_path = self
                .model_path
                .to_str()
                .ok_or_else(|| AsrError::Runtime("model path is not valid UTF-8".to_string()))?;
            let initialized = whisper_rs::WhisperContext::new_with_params(
                model_path,
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|error| AsrError::Runtime(error.to_string()))?;
            *context = Some(initialized);
        }

        operation(context.as_ref().expect("context was just initialized"))
    }

    fn shutdown(&self) {
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
impl AsrRuntime for LocalWhisperRuntime {
    fn transcribe(&self, audio: CapturedAudio) -> Result<FinalTranscription, AsrError> {
        self.with_context(|context| {
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
        })
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

    fn shutdown(&self) {}
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

/// Presents the established Whisper runtime through the Transcription Engine
/// boundary so the Second Opinion router can treat it like any other engine.
///
/// This adapter adds no decoding work: it times the existing call and reports
/// no confidence, because whisper.cpp's segment iterator gives Slugtale plain
/// text today. That is why Whisper can only ever be escalated *from* on the
/// anomaly rules (empty output, repetition, implausibly short text), never on a
/// confidence threshold — see [`crate::EngineConfidence`].
pub struct WhisperTranscriptionProvider {
    runtime: Arc<LocalWhisperRuntime>,
}

impl WhisperTranscriptionProvider {
    pub fn new(runtime: Arc<LocalWhisperRuntime>) -> Self {
        Self { runtime }
    }
}

impl crate::TranscriptionProvider for WhisperTranscriptionProvider {
    fn engine(&self) -> crate::TranscriptionEngine {
        crate::TranscriptionEngine::Whisper
    }

    fn metadata(&self) -> crate::EngineMetadata {
        crate::EngineMetadata {
            engine: crate::TranscriptionEngine::Whisper,
            model_id: crate::DEFAULT_MODEL_ID,
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
            return crate::EngineAvailability::Unavailable(crate::EngineUnavailable::RuntimeNotBuilt);
        }
        if !self.runtime.model_path().exists() {
            return crate::EngineAvailability::Unavailable(
                crate::EngineUnavailable::AssetsMissing {
                    detail: "The Whisper model has not been downloaded yet.".to_string(),
                },
            );
        }
        crate::EngineAvailability::Available
    }

    fn transcribe(&self, audio: &CapturedAudio) -> Result<crate::EngineTranscription, AsrError> {
        let started = std::time::Instant::now();
        // The runtime still owns its audio, so this clone is the cost of giving
        // every provider a borrowing signature. It only happens on the Whisper
        // leg; the router never clones for the engines that borrow natively.
        let transcription = self.runtime.transcribe(audio.clone())?;
        Ok(crate::EngineTranscription::plain(
            crate::TranscriptionEngine::Whisper,
            transcription,
            started.elapsed(),
        ))
    }
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
    fn whisper_threads_prefer_physical_cores_over_smt_siblings() {
        // ggml's compute threads contend on shared FP units, so hyperthread
        // siblings slow decoding down instead of speeding it up: on the 6C/12T
        // Linux reference box an 11s clip took 6.1s with 12 threads vs 1.6s with
        // 6 (slugtale-jwy). Use the physical core count, never the SMT total.
        assert_eq!(
            whisper_thread_count(6, NonZeroUsize::new(12).unwrap()),
            NonZeroUsize::new(6).unwrap()
        );
    }

    #[test]
    fn whisper_threads_never_exceed_available_parallelism() {
        // A containerized/affinity-restricted process can see fewer logical CPUs
        // than the machine has physical cores; stay within what we may use.
        assert_eq!(
            whisper_thread_count(8, NonZeroUsize::new(4).unwrap()),
            NonZeroUsize::new(4).unwrap()
        );
    }

    #[test]
    fn whisper_threads_fall_back_to_available_parallelism_when_physical_unknown() {
        assert_eq!(
            whisper_thread_count(0, NonZeroUsize::new(8).unwrap()),
            NonZeroUsize::new(8).unwrap()
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
    fn whisper_runtime_cache_rejects_warmup_after_shutdown() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-shutdown");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&model_path, b"model").unwrap();

        cache.shutdown();

        assert!(cache.begin_warming_existing_model(&model_path).is_none());
        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[cfg(feature = "local-whisper-runtime")]
    #[test]
    fn runtime_returned_after_cache_shutdown_cannot_initialize_model() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-runtime-after-shutdown");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&model_path, b"not-a-real-model").unwrap();

        cache.shutdown();
        let runtime = cache.runtime_for(&model_path);
        let error = runtime.warm_up().unwrap_err();

        assert_eq!(
            error,
            AsrError::Runtime("local Whisper runtime is shutting down".to_string())
        );
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
