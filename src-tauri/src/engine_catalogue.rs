//! The app's closed catalogue of local Transcription Engines.
//!
//! The catalogue owns provider lifetime, availability, Whisper runtime reuse,
//! and Second Opinion selection. Settings and the Dictation Workflow ask the
//! same module, so they cannot disagree about what can run.

use crate::{
    default_model_path, engine_that_can_run, AppleSpeechProvider, AsrError, EngineAvailability,
    LocalWhisperRuntime, ParakeetProvider, SecondOpinionCoordinator, SecondOpinionMode,
    SecondOpinionRouter, Settings, TranscriptionEngine, TranscriptionProvider,
    WhisperRuntimeCache, WhisperTranscriptionProvider,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct TranscriptionEngineCatalogue {
    model_dir: Mutex<Option<PathBuf>>,
    whisper: WhisperRuntimeCache,
    parakeet: Mutex<Option<Arc<ParakeetProvider>>>,
    apple: Arc<AppleSpeechProvider>,
    /// Bumped every time a warm-up is requested, so a slow warm-up started by
    /// an older Settings state can recognise that it was superseded and stand
    /// down instead of loading a model nobody selected any more.
    warm_generation: Arc<std::sync::atomic::AtomicU64>,
    /// The engine the current resident models were kept for. Repeated
    /// warm-ups of the same engine must not keep releasing the other models:
    /// a Second Opinion or an unrelated caller may have reloaded them.
    released_for: Mutex<Option<TranscriptionEngine>>,
    /// One in-flight gate for this catalogue's whole lifetime, shared by every
    /// router it hands out, so a timed-out escalation still blocks the next
    /// segment's escalation instead of piling up slow second engines.
    coordinator: SecondOpinionCoordinator,
}

impl TranscriptionEngineCatalogue {
    pub fn new(model_dir: Option<PathBuf>) -> Self {
        let catalogue = Self {
            model_dir: Mutex::new(None),
            whisper: WhisperRuntimeCache::default(),
            parakeet: Mutex::new(None),
            apple: Arc::new(AppleSpeechProvider::new()),
            warm_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            released_for: Mutex::new(None),
            coordinator: SecondOpinionCoordinator::default(),
        };
        if let Some(model_dir) = model_dir {
            catalogue.set_model_dir(model_dir);
        }
        catalogue
    }

    pub fn set_model_dir(&self, model_dir: PathBuf) {
        *self
            .model_dir
            .lock()
            .expect("engine catalogue model directory mutex poisoned") = Some(model_dir.clone());
        let mut parakeet = self
            .parakeet
            .lock()
            .expect("engine catalogue parakeet mutex poisoned");
        if parakeet.is_none() {
            *parakeet = Some(Arc::new(ParakeetProvider::new(crate::parakeet_asset_dir(
                &model_dir,
            ))));
        }
    }

    pub fn model_path(&self, settings: &Settings) -> Option<PathBuf> {
        settings.model.as_ref().map(PathBuf::from).or_else(|| {
            self.model_dir
                .lock()
                .ok()
                .and_then(|dir| dir.as_deref().map(default_model_path))
        })
    }

    pub fn whisper_runtime(&self, settings: &Settings) -> Option<Arc<LocalWhisperRuntime>> {
        let runtime = self.whisper.runtime_for(&self.model_path(settings)?);
        runtime.set_speed_profile(settings.speed_profile);
        Some(runtime)
    }

    /// Which engine the next dictation would actually use, applying the same
    /// fallback rule as the Second Opinion router and Dictation Readiness
    /// ([`engine_that_can_run`]). Warm-up asks through here so it loads exactly
    /// what dictation will use, never a hard-coded engine.
    pub fn effective_primary_engine(&self, settings: &Settings) -> Option<TranscriptionEngine> {
        engine_that_can_run(settings.primary_engine, &self.availability(settings))
    }

    /// Prepare a warm-up of the effective primary engine, ready to run off the
    /// caller's thread. `None` when no engine can run, in which case there is
    /// nothing worth warming.
    pub fn prepare_primary_warm_up(&self, settings: &Settings) -> Option<EngineWarmUp> {
        let engine = self.effective_primary_engine(settings)?;
        let provider = self.provider(settings, engine)?;
        let expected_generation =
            self.warm_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Some(EngineWarmUp {
            generation: Arc::clone(&self.warm_generation),
            expected_generation,
            provider,
        })
    }

    /// Release every large loaded model except `keep`, so switching engines on
    /// a memory-constrained machine does not leave two large models resident.
    /// In-flight transcriptions keep their own references and finish safely;
    /// released engines simply reload on their next use. Idempotent per
    /// engine: repeated calls for the same `keep` do nothing, so a polled
    /// caller cannot unload a model another engine legitimately reloaded.
    pub fn release_models_except(&self, keep: TranscriptionEngine) {
        let mut released_for = self
            .released_for
            .lock()
            .expect("engine catalogue release mutex poisoned");
        if *released_for == Some(keep) {
            return;
        }
        *released_for = Some(keep);
        drop(released_for);

        if keep != TranscriptionEngine::Whisper {
            self.whisper.release();
        }
        if keep != TranscriptionEngine::Parakeet {
            if let Some(parakeet) = self.parakeet_provider() {
                parakeet.unload();
            }
        }
    }

    pub fn whisper_provider(&self, settings: &Settings) -> Option<Arc<dyn TranscriptionProvider>> {
        Some(Arc::new(WhisperTranscriptionProvider::new(
            self.whisper_runtime(settings)?,
        )))
    }

    pub fn parakeet_provider(&self) -> Option<Arc<ParakeetProvider>> {
        self.parakeet
            .lock()
            .ok()
            .and_then(|provider| provider.clone())
    }

    pub fn apple_provider(&self) -> Arc<AppleSpeechProvider> {
        self.apple.clone()
    }

    pub fn provider(
        &self,
        settings: &Settings,
        engine: TranscriptionEngine,
    ) -> Option<Arc<dyn TranscriptionProvider>> {
        match engine {
            TranscriptionEngine::Whisper => self.whisper_provider(settings),
            TranscriptionEngine::Parakeet => self
                .parakeet_provider()
                .map(|provider| provider as Arc<dyn TranscriptionProvider>),
            TranscriptionEngine::AppleSpeech => {
                Some(self.apple_provider() as Arc<dyn TranscriptionProvider>)
            }
        }
    }

    pub fn availability(
        &self,
        settings: &Settings,
    ) -> Vec<(TranscriptionEngine, EngineAvailability)> {
        TranscriptionEngine::ALL
            .into_iter()
            .filter_map(|engine| {
                self.provider(settings, engine)
                    .map(|provider| (engine, provider.availability()))
            })
            .collect()
    }

    pub fn router(&self, settings: &Settings) -> Result<SecondOpinionRouter, AsrError> {
        let availability = self.availability(settings);
        let primary = selected_primary(
            settings,
            &availability,
            |engine| self.provider(settings, engine),
            self.whisper_provider(settings),
        )
        .ok_or_else(|| {
            AsrError::Runtime("could not resolve a local Transcription Engine".to_string())
        })?;

        Ok(match settings.second_opinion {
            SecondOpinionMode::Off => SecondOpinionRouter::single(primary),
            SecondOpinionMode::Automatic => {
                let second = TranscriptionEngine::ALL
                    .into_iter()
                    .filter(|engine| *engine != primary.engine())
                    .filter_map(|engine| self.provider(settings, engine))
                    .find(|provider| provider.availability().is_available());
                let coordinator = self.coordinator.clone();
                second
                    .map(|second| {
                        SecondOpinionRouter::new(
                            primary.clone(),
                            second,
                            SecondOpinionMode::Automatic,
                        )
                        .with_coordinator(coordinator)
                    })
                    .unwrap_or_else(|| SecondOpinionRouter::single(primary))
            }
        })
    }

    pub fn shutdown(&self) {
        self.whisper.shutdown();
        if let Some(parakeet) = self.parakeet_provider() {
            parakeet.shutdown();
        }
    }
}

fn selected_primary(
    settings: &Settings,
    availability: &[(TranscriptionEngine, EngineAvailability)],
    resolve: impl Fn(TranscriptionEngine) -> Option<Arc<dyn TranscriptionProvider>>,
    whisper_fallback: Option<Arc<dyn TranscriptionProvider>>,
) -> Option<Arc<dyn TranscriptionProvider>> {
    engine_that_can_run(settings.primary_engine, availability)
        .and_then(resolve)
        .or(whisper_fallback)
}

/// One pending warm-up of the effective primary engine, resolved against the
/// Settings it was prepared from. Run it off the caller's thread: loading a
/// large model takes seconds and must never block Settings saves or UI events.
pub struct EngineWarmUp {
    generation: Arc<std::sync::atomic::AtomicU64>,
    expected_generation: u64,
    provider: Arc<dyn TranscriptionProvider>,
}

impl EngineWarmUp {
    /// The engine this warm-up will load.
    pub fn engine(&self) -> TranscriptionEngine {
        self.provider.engine()
    }

    /// True when a newer warm-up request superseded this one. Rapid Settings
    /// changes must not publish a stale engine as the current warm engine, so a
    /// superseded warm-up stands down instead of loading.
    pub fn is_stale(&self) -> bool {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
            != self.expected_generation
    }

    /// Warm the engine unless a newer request superseded this one. Safe to run
    /// next to shutdown: providers check their own shutdown flags under the
    /// same locks that teardown uses, so a late warm-up cannot resurrect a
    /// released model.
    pub fn run(self) -> Result<(), AsrError> {
        if self.is_stale() {
            return Ok(());
        }
        self.provider.warm_up()
    }
}

impl Default for TranscriptionEngineCatalogue {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfidence, EngineMetadata, EngineTranscription, FinalTranscription};
    use std::time::Duration;

    struct FakeProvider(TranscriptionEngine);

    impl TranscriptionProvider for FakeProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.0
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.0,
                model_id: "test",
                revision: "test",
                approximate_bytes: None,
                source_url: None,
                license: "test",
                license_url: "https://example.test",
                attribution: None,
                modifications: None,
                system_managed: false,
                supported_platforms: "test",
            }
        }

        fn availability(&self) -> EngineAvailability {
            EngineAvailability::Available
        }

        fn transcribe(
            &self,
            _audio: &crate::CapturedAudio,
        ) -> Result<EngineTranscription, AsrError> {
            Ok(EngineTranscription {
                engine: self.0,
                transcription: FinalTranscription {
                    text: String::new(),
                    segments: Vec::new(),
                },
                alternatives: Vec::new(),
                confidence: EngineConfidence::unreported(),
                latency: Duration::ZERO,
            })
        }
    }

    #[test]
    fn selected_model_path_wins_over_the_default_model_path() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let settings = Settings {
            model: Some("chosen.ggml".to_string()),
            ..Settings::default()
        };
        assert_eq!(
            catalogue.model_path(&settings),
            Some(PathBuf::from("chosen.ggml"))
        );
    }

    #[test]
    fn default_model_path_requires_a_models_directory() {
        let settings = Settings::default();
        assert_eq!(
            TranscriptionEngineCatalogue::default().model_path(&settings),
            None
        );
        assert_eq!(
            TranscriptionEngineCatalogue::new(Some(PathBuf::from("models"))).model_path(&settings),
            Some(std::path::Path::new("models").join("ggml-base.en.bin")),
        );
    }

    #[test]
    fn an_available_non_whisper_engine_does_not_need_a_whisper_fallback() {
        let parakeet: Arc<dyn TranscriptionProvider> =
            Arc::new(FakeProvider(TranscriptionEngine::Parakeet));
        let settings = Settings {
            primary_engine: TranscriptionEngine::Parakeet,
            ..Settings::default()
        };
        let availability = vec![(TranscriptionEngine::Parakeet, EngineAvailability::Available)];

        let selected = selected_primary(
            &settings,
            &availability,
            |engine| (engine == TranscriptionEngine::Parakeet).then(|| parakeet.clone()),
            None,
        );

        assert_eq!(selected.unwrap().engine(), TranscriptionEngine::Parakeet);
    }

    /// A provider whose warm-up is observable, so tests can prove whether a
    /// warm-up actually loaded anything.
    struct WarmCountingProvider {
        warm_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl TranscriptionProvider for WarmCountingProvider {
        fn engine(&self) -> TranscriptionEngine {
            TranscriptionEngine::Whisper
        }

        fn metadata(&self) -> EngineMetadata {
            FakeProvider(TranscriptionEngine::Whisper).metadata()
        }

        fn availability(&self) -> EngineAvailability {
            EngineAvailability::Available
        }

        fn transcribe(
            &self,
            _audio: &crate::CapturedAudio,
        ) -> Result<EngineTranscription, AsrError> {
            Err(AsrError::Runtime("warm-up test never transcribes".to_string()))
        }

        fn warm_up(&self) -> Result<(), AsrError> {
            self.warm_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn a_warm_up_whose_generation_was_superseded_stands_down_without_loading() {
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let warm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let warm_up = EngineWarmUp {
            generation: Arc::clone(&generation),
            expected_generation: 1,
            provider: Arc::new(WarmCountingProvider {
                warm_calls: Arc::clone(&warm_calls),
            }),
        };

        // A newer Settings state requested a warm-up before this one started.
        generation.store(2, std::sync::atomic::Ordering::SeqCst);

        assert!(warm_up.is_stale());
        warm_up.run().unwrap();

        assert_eq!(warm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn a_current_warm_up_loads_its_engine_once() {
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let warm_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let warm_up = EngineWarmUp {
            generation: Arc::clone(&generation),
            expected_generation: 1,
            provider: Arc::new(WarmCountingProvider {
                warm_calls: Arc::clone(&warm_calls),
            }),
        };

        generation.store(1, std::sync::atomic::Ordering::SeqCst);

        assert!(!warm_up.is_stale());
        assert_eq!(warm_up.engine(), TranscriptionEngine::Whisper);
        warm_up.run().unwrap();

        assert_eq!(warm_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn releasing_models_repeatedly_for_the_same_engine_only_releases_once() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let settings = Settings::default();

        // A polled caller repeats the same request; the second release must
        // not clear a runtime another engine legitimately reloaded.
        let before_release = catalogue.whisper_runtime(&settings).unwrap();
        catalogue.release_models_except(TranscriptionEngine::Parakeet);
        let after_first_release = catalogue.whisper_runtime(&settings).unwrap();
        catalogue.release_models_except(TranscriptionEngine::Parakeet);
        let after_second_release = catalogue.whisper_runtime(&settings).unwrap();

        assert!(!std::sync::Arc::ptr_eq(
            &before_release,
            &after_first_release
        ));
        assert!(std::sync::Arc::ptr_eq(
            &after_first_release,
            &after_second_release
        ));
    }
}
