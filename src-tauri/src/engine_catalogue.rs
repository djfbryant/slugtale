//! The app's closed catalogue of local Transcription Engines.
//!
//! The catalogue owns provider lifetime, availability, Whisper runtime reuse,
//! and Second Opinion selection. Settings and the Dictation Workflow ask the
//! same module, so they cannot disagree about what can run.

use crate::{
    default_model_path, engine_that_can_run, AppleSpeechProvider, AsrError, EngineAvailability,
    LocalWhisperRuntime, ParakeetProvider, SecondOpinionMode, SecondOpinionRouter, Settings,
    TranscriptionEngine, TranscriptionProvider, WhisperRuntimeCache, WhisperTranscriptionProvider,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct TranscriptionEngineCatalogue {
    model_dir: Mutex<Option<PathBuf>>,
    whisper: WhisperRuntimeCache,
    parakeet: Mutex<Option<Arc<ParakeetProvider>>>,
    apple: Arc<AppleSpeechProvider>,
}

impl TranscriptionEngineCatalogue {
    pub fn new(model_dir: Option<PathBuf>) -> Self {
        let catalogue = Self {
            model_dir: Mutex::new(None),
            whisper: WhisperRuntimeCache::default(),
            parakeet: Mutex::new(None),
            apple: Arc::new(AppleSpeechProvider::new()),
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

    pub fn warm_ready_whisper(&self, settings: &Settings) -> Option<Arc<LocalWhisperRuntime>> {
        self.whisper
            .begin_warming_existing_model(&self.model_path(settings)?)
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
                second
                    .map(|second| {
                        SecondOpinionRouter::new(
                            primary.clone(),
                            second,
                            SecondOpinionMode::Automatic,
                        )
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
}
