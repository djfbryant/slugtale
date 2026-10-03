use crate::{CapturedAudio, LocalModelRef, SpeedProfile};
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "local-whisper-runtime"))]
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

/// One recognized segment of a local Whisper transcription: the recognized
/// text plus its start/end timing. Ordered segments are preserved through ASR
/// so Transcript Cleanup can decide later whether pauses imply line breaks,
/// instead of the runtime flattening them away immediately (slugtale-cqy).
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
    /// Segmentless construction for engines that expose no segment timing and
    /// for tests where timing is irrelevant.
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
    /// A second opinion ran past its bounded budget. The router keeps the first
    /// usable transcript when this happens, so the user never waits on a slow
    /// engine (slugtale-vjs.3).
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
    context: Mutex<Option<whisper_rs::WhisperContext>>,
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

/// Caches the loaded Whisper runtime across transcriptions so the model file is
/// read from disk once rather than on every call. The runtime is rebuilt only
/// when the configured model path changes.
#[derive(Default)]
pub struct WhisperRuntimeCache(Mutex<WhisperRuntimeCacheState>);

#[derive(Default)]
struct WhisperRuntimeCacheState {
    runtime: Option<Arc<LocalWhisperRuntime>>,
    shutting_down: bool,
}

impl WhisperRuntimeCache {
    pub fn runtime_for(&self, model: &LocalModelRef) -> Arc<LocalWhisperRuntime> {
        let mut state = self.0.lock().expect("whisper runtime cache mutex poisoned");
        let runtime = Self::runtime_for_locked(&mut state, model);
        if state.shutting_down {
            // A dictation task can race ExitRequested after obtaining the app
            // handle. Return a permanently stopped runtime so it cannot create
            // a new Metal context after shutdown has already drained the cache.
            runtime.shutdown();
        }
        runtime
    }

    /// Drop the cache's reference to the runtime without ending the cache
    /// itself, so the loaded context is released once in-flight transcriptions
    /// finish and a later warm-up can start again. Unlike [`Self::shutdown`]
    /// this is reversible: the next [`Self::runtime_for`] builds a fresh
    /// runtime. A no-op after shutdown, which stays final.
    pub fn release(&self) {
        let mut state = match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.shutting_down {
            return;
        }
        state.runtime = None;
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
        if let Some(runtime) = state.runtime.as_ref() {
            runtime.shutdown();
        }
    }

    fn runtime_for_locked(
        state: &mut WhisperRuntimeCacheState,
        model: &LocalModelRef,
    ) -> Arc<LocalWhisperRuntime> {
        if let Some(existing) = state.runtime.as_ref() {
            if existing.model().path() == model.path() {
                return existing.clone();
            }
        }

        let runtime = Arc::new(LocalWhisperRuntime::new(model.clone()));
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
            if !self.model.is_present() {
                return Err(AsrError::ModelMissing {
                    path: self.model.path().to_path_buf(),
                });
            }

            let model_path =
                self.model.path().to_str().ok_or_else(|| {
                    AsrError::Runtime("model path is not valid UTF-8".to_string())
                })?;
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
            let mut state = context
                .create_state()
                .map_err(|error| AsrError::Runtime(error.to_string()))?;
            let decode_settings = recommended_whisper_decode_settings(speed_profile);
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

            let segments = state
                .as_iter()
                .map(|segment| {
                    (
                        segment.to_string(),
                        segment.start_timestamp(),
                        segment.end_timestamp(),
                    )
                })
                .collect::<Vec<_>>();

            Ok(transcript_from_whisper_segments(segments))
        })
    }
}

/// Map raw Whisper segments (text plus centisecond timestamps) to a
/// [`FinalTranscription`]. whisper.cpp reports t0/t1 in 10 ms ticks, so they
/// are converted to milliseconds here; negative ticks (seen on some models
/// before audio start) clamp to zero.
#[cfg(any(test, feature = "local-whisper-runtime"))]
fn transcript_from_whisper_segments(
    segments: impl IntoIterator<Item = (String, i64, i64)>,
) -> FinalTranscription {
    FinalTranscription::from_segments(
        segments
            .into_iter()
            .map(|(text, start_cs, end_cs)| TranscriptSegment {
                text,
                start_ms: start_cs.clamp(0, i64::MAX) as u64 * 10,
                end_ms: end_cs.clamp(0, i64::MAX) as u64 * 10,
            })
            .collect(),
    )
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

    fn shutdown(&self) {}
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
/// [`crate::TranscriptionProvider::can_install_assets`] asks.
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
}

impl crate::TranscriptionProvider for WhisperTranscriptionProvider {
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

    fn assets(&self) -> crate::EngineAssets {
        match &self.model_manager {
            Some(manager) => {
                let status = manager.status();
                crate::EngineAssets {
                    installed_bytes: status.bytes,
                    present: Some(status.present),
                }
            }
            // Without a manager Slugtale cannot see the model directory, so it
            // reports neither a size nor a presence it never looked up.
            None => crate::EngineAssets::unmeasured(),
        }
    }

    fn can_install_assets(&self) -> bool {
        self.model_manager.is_some()
    }

    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(crate::DownloadProgress),
    ) -> Result<crate::AssetInstall, String> {
        let installed = self
            .model_manager
            .as_ref()
            .ok_or_else(|| {
                crate::transcription_engine::assets_cannot_be_installed(
                    crate::TranscriptionEngine::Whisper,
                )
            })?
            .download_default(&crate::HttpModelDownloader, on_progress)
            .map_err(|error| error.to_string())?;

        Ok(crate::AssetInstall {
            // The Local Model is the model every engine falls back to, and the
            // Dictation Runtime pre-loads it, so a finished download is the one
            // install worth warming for.
            warm_up: installed.present,
        })
    }

    fn remove_assets(&self) -> Result<(), String> {
        self.model_manager
            .as_ref()
            .ok_or_else(|| {
                crate::transcription_engine::assets_cannot_be_removed(
                    crate::TranscriptionEngine::Whisper,
                )
            })?
            .delete_default()
            .map_err(|error| error.to_string())?;
        Ok(())
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
    use super::*;
    use crate::{TranscriptionProvider, DEFAULT_MODEL_FILENAME};

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
        let runtime = LocalWhisperRuntime::new(LocalModelRef::at(model_path.clone()));

        let error = runtime
            .transcribe(
                CapturedAudio::mono_16khz(vec![0.0; 16_000]),
                SpeedProfile::default(),
            )
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

    #[test]
    fn a_whisper_row_reports_the_local_model_it_would_open() {
        // Whisper's assets are the Local Model, so the row has to read the file
        // the engine would actually open rather than a number the engine keeps.
        let root = unique_test_dir("whisper-assets");
        let manager =
            crate::AppFiles::from_dirs_for_test(Some(root.join("config")), Some(root.clone()))
                .model_manager()
                .expect("a data directory resolves a model directory");
        let model_path = crate::default_model_path(manager.model_dir());
        std::fs::create_dir_all(manager.model_dir()).unwrap();
        std::fs::write(&model_path, b"ggml").unwrap();

        let provider = WhisperTranscriptionProvider::new(
            Arc::new(LocalWhisperRuntime::new(LocalModelRef::at(model_path))),
            SpeedProfile::Balanced,
            Some(manager),
        );
        let row = crate::EngineView::of(&provider, true);

        assert_eq!(
            row.assets,
            crate::EngineAssets {
                installed_bytes: Some(4),
                present: Some(true),
            }
        );
        assert!(provider.can_install_assets());
        assert!(
            !row.installable,
            "installed assets are not an install action"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn installing_a_local_model_that_is_already_there_asks_for_a_warm_up() {
        // The Local Model Manager short-circuits an install whose bytes are
        // already on disk, so this is the whole install path without a download.
        // The answer the app acts on is "load the model a dictation is about to
        // need", which is true exactly when the file landed.
        let root = unique_test_dir("whisper-install-warm-up");
        let manager =
            crate::AppFiles::from_dirs_for_test(Some(root.join("config")), Some(root.clone()))
                .model_manager()
                .unwrap();
        std::fs::create_dir_all(manager.model_dir()).unwrap();
        let model_path = crate::default_model_path(manager.model_dir());
        std::fs::write(&model_path, b"ggml").unwrap();
        let provider = WhisperTranscriptionProvider::new(
            Arc::new(LocalWhisperRuntime::new(LocalModelRef::at(model_path))),
            SpeedProfile::Balanced,
            Some(manager),
        );

        let mut updates = Vec::new();
        let install = provider
            .install_assets(&mut |progress| updates.push(progress))
            .unwrap();

        assert_eq!(install, crate::AssetInstall { warm_up: true });
        assert!(
            updates.is_empty(),
            "nothing was downloaded, so nothing to report"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_whisper_provider_with_no_model_manager_has_no_install_path() {
        // Before the catalogue is handed a manager, the engine can see no model
        // directory. It must say so rather than report bytes it never looked up
        // or accept an install it cannot carry out.
        let provider = WhisperTranscriptionProvider::new(
            Arc::new(LocalWhisperRuntime::new(LocalModelRef::at(
                unique_test_dir("whisper-no-manager").join(DEFAULT_MODEL_FILENAME),
            ))),
            SpeedProfile::Balanced,
            None,
        );

        assert_eq!(provider.assets(), crate::EngineAssets::unmeasured());
        assert!(!provider.can_install_assets());
        assert_eq!(
            provider.install_assets(&mut |_| {}).unwrap_err(),
            "Whisper base.en has no installation path in Slugtale."
        );
        assert_eq!(
            provider.remove_assets().unwrap_err(),
            "Whisper base.en has no assets for Slugtale to remove."
        );
    }

    #[test]
    fn whisper_runtime_cache_reuses_runtime_for_same_model_path() {
        let cache = WhisperRuntimeCache::default();
        let model =
            LocalModelRef::at(unique_test_dir("whisper-cache").join(DEFAULT_MODEL_FILENAME));

        let first = cache.runtime_for(&model);
        let second = cache.runtime_for(&model);

        assert!(std::sync::Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn whisper_runtime_cache_rebuilds_runtime_when_model_path_changes() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-model-change");
        let first_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        let second_path = model_dir.join("custom-model.bin");

        let first = cache.runtime_for(&LocalModelRef::at(first_path));
        let second = cache.runtime_for(&LocalModelRef::at(second_path.clone()));

        assert!(!std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(second.model().path(), second_path);
    }

    #[test]
    fn whisper_runtime_cache_release_drops_the_runtime_but_stays_reusable() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-release");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&model_path, b"model").unwrap();

        let model = LocalModelRef::at(model_path);
        let warmed = cache.runtime_for(&model);
        cache.release();

        let after_release = cache.runtime_for(&model);

        assert!(!std::sync::Arc::ptr_eq(&warmed, &after_release));

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn whisper_runtime_cache_release_after_shutdown_does_not_resurrect_the_cache() {
        let cache = WhisperRuntimeCache::default();
        let model_dir = unique_test_dir("whisper-cache-release-shutdown");
        std::fs::create_dir_all(&model_dir).unwrap();
        let model_path = model_dir.join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&model_path, b"model").unwrap();

        cache.shutdown();
        cache.release();
        let runtime = cache.runtime_for(&LocalModelRef::at(model_path));

        // A released runtime must behave like a shut-down one: the next warm-up
        // or transcription fails instead of creating a new context.
        assert!(runtime.warm_up().is_err());
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
        let runtime = cache.runtime_for(&LocalModelRef::at(model_path));
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
            Ok(FinalTranscription::plain(self.text))
        }
    }

    #[test]
    fn whisper_segments_preserve_ordered_text_and_timing() {
        let transcription = transcript_from_whisper_segments(vec![
            (" Hello".to_string(), 0, 150),
            (" from slugtale.".to_string(), 160, 320),
        ]);

        assert_eq!(
            transcription.segments,
            vec![
                TranscriptSegment {
                    text: " Hello".to_string(),
                    start_ms: 0,
                    end_ms: 1_500
                },
                TranscriptSegment {
                    text: " from slugtale.".to_string(),
                    start_ms: 1_600,
                    end_ms: 3_200
                },
            ]
        );
    }

    #[test]
    fn flattening_whisper_segments_matches_the_previous_immediate_join() {
        // Before segments were preserved the runtime joined segment texts with
        // no separator and trimmed once; the flattened text must stay identical
        // so behavior is unchanged until cleanup consumes boundaries.
        let transcription = transcript_from_whisper_segments(vec![
            (" Hello ".to_string(), 0, 100),
            (" from slugtale. ".to_string(), 110, 250),
        ]);

        assert_eq!(transcription.text, "Hello  from slugtale.");
    }

    #[test]
    fn negative_whisper_timestamps_clamp_to_zero() {
        let transcription = transcript_from_whisper_segments(vec![(" Hi.".to_string(), -5, -1)]);

        assert_eq!(transcription.segments[0].start_ms, 0);
        assert_eq!(transcription.segments[0].end_ms, 0);
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
