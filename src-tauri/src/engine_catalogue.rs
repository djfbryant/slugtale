
use crate::{
    engine_that_can_run, AppleSpeechProvider, AsrError, DiagnosticEvent, DiagnosticSink,
    EngineAvailability, LocalModelManager, LocalModelRef, LocalWhisperRuntime, ParakeetProvider,
    PhononProvider, SecondOpinionCoordinator, SecondOpinionMode, SecondOpinionRouter, Settings,
    SharedDiagnosticLog, TranscriptionEngine, TranscriptionProvider, WhisperRuntimeCache,
    WhisperTranscriptionProvider,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct TranscriptionEngineCatalogue {
    model_dir: Mutex<Option<PathBuf>>,
    model_manager: Mutex<Option<LocalModelManager>>,
    whisper: WhisperRuntimeCache,
    parakeet: Mutex<Option<Arc<ParakeetProvider>>>,
    phonon: Mutex<Option<Arc<PhononProvider>>>,
    apple: Arc<AppleSpeechProvider>,
    warm_generation: Arc<std::sync::atomic::AtomicU64>,
    released_for: Mutex<Option<TranscriptionEngine>>,
    coordinator: SecondOpinionCoordinator,
}

impl TranscriptionEngineCatalogue {
    pub fn new(model_dir: Option<PathBuf>) -> Self {
        let catalogue = Self {
            model_dir: Mutex::new(None),
            model_manager: Mutex::new(None),
            whisper: WhisperRuntimeCache::default(),
            parakeet: Mutex::new(None),
            phonon: Mutex::new(None),
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

    fn set_model_dir(&self, model_dir: PathBuf) {
        *self
            .model_dir
            .lock()
            .expect("engine catalogue model directory mutex poisoned") = Some(model_dir.clone());
        let mut parakeet = self
            .parakeet
            .lock()
            .expect("Parakeet provider mutex poisoned");
        if parakeet.is_none() {
            *parakeet = Some(Arc::new(ParakeetProvider::new(
                crate::PARAKEET_TDT_V2.asset_dir(&model_dir),
            )));
        }
        let mut phonon = self.phonon.lock().expect("Phonon provider mutex poisoned");
        if phonon.is_none() {
            *phonon = Some(Arc::new(PhononProvider::new(&model_dir)));
        }
    }

    pub fn set_model_manager(&self, manager: LocalModelManager) {
        *self
            .model_manager
            .lock()
            .expect("engine catalogue model manager mutex poisoned") = Some(manager.clone());
        self.set_model_dir(manager.model_dir().to_path_buf());
    }

    fn model_manager(&self) -> Option<LocalModelManager> {
        self.model_manager
            .lock()
            .ok()
            .and_then(|manager| manager.clone())
    }

    pub fn local_model(&self, settings: &Settings) -> Option<LocalModelRef> {
        let model_dir = self.model_dir.lock().ok();
        LocalModelRef::resolve(
            settings,
            model_dir.as_deref().and_then(|dir| dir.as_deref()),
        )
    }

    fn whisper_runtime(&self, settings: &Settings) -> Option<Arc<LocalWhisperRuntime>> {
        Some(self.whisper.runtime_for(&self.local_model(settings)?))
    }

    fn whisper_transcription(&self, settings: &Settings) -> Option<WhisperTranscriptionProvider> {
        Some(WhisperTranscriptionProvider::new(
            self.whisper_runtime(settings)?,
            settings.speed_profile,
            self.model_manager(),
        ))
    }

    fn effective_primary_engine(&self, settings: &Settings) -> Option<TranscriptionEngine> {
        engine_that_can_run(settings.primary_engine, &self.availability(settings))
    }

    pub fn prepare_primary_warm_up(&self, settings: &Settings) -> Option<EngineWarmUp> {
        let engine = self.effective_primary_engine(settings)?;
        let provider = self.provider(settings, engine)?;
        let expected_generation = self
            .warm_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        Some(EngineWarmUp {
            generation: Arc::clone(&self.warm_generation),
            expected_generation,
            provider,
        })
    }

    pub fn begin_primary_warm_up(&self, settings: &Settings) -> Option<EngineWarmUp> {
        let warm_up = self.prepare_primary_warm_up(settings)?;
        self.release_models_except(warm_up.engine());
        Some(warm_up)
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
        if keep != TranscriptionEngine::Phonon {
            if let Some(phonon) = self.phonon_provider() {
                phonon.unload();
            }
        }
    }

    pub fn whisper_provider(&self, settings: &Settings) -> Option<Arc<dyn TranscriptionProvider>> {
        self.whisper_transcription(settings)
            .map(|provider| Arc::new(provider) as Arc<dyn TranscriptionProvider>)
    }

    fn parakeet_provider(&self) -> Option<Arc<ParakeetProvider>> {
        self.parakeet
            .lock()
            .ok()
            .and_then(|provider| provider.clone())
    }

    fn phonon_provider(&self) -> Option<Arc<PhononProvider>> {
        self.phonon
            .lock()
            .ok()
            .and_then(|provider| provider.clone())
    }

    fn apple_provider(&self) -> Arc<AppleSpeechProvider> {
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
            TranscriptionEngine::Phonon => self
                .phonon_provider()
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

    /// The Second Opinion router for one dictation: the engine the Settings name
    /// if it can run, plus a second one only when Second Opinion is on. `None`
    /// for the primary means no engine can run, which is an error rather than a
    /// silent single-engine fallback.
    fn router(&self, settings: &Settings) -> Result<SecondOpinionRouter, AsrError> {
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

    pub fn dictation_stack<S>(
        &self,
        settings: &Settings,
        log: SharedDiagnosticLog<S>,
    ) -> Result<DictationStack<S>, AsrError>
    where
        S: DiagnosticSink + Send + Sync + 'static,
    {
        Ok(DictationStack::new(self.router(settings)?, log))
    }

    pub fn shutdown(&self) {
        self.whisper.shutdown();
        if let Some(parakeet) = self.parakeet_provider() {
            parakeet.shutdown();
        }
        if let Some(phonon) = self.phonon_provider() {
            phonon.shutdown();
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
    pub fn engine(&self) -> TranscriptionEngine {
        self.provider.engine()
    }

    pub fn is_stale(&self) -> bool {
        self.generation.load(std::sync::atomic::Ordering::SeqCst) != self.expected_generation
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

pub struct DictationStack<S> {
    router: SecondOpinionRouter,
    log: SharedDiagnosticLog<S>,
}

impl<S> DictationStack<S>
where
    S: DiagnosticSink + Send + Sync + 'static,
{
    pub(crate) fn new(router: SecondOpinionRouter, log: SharedDiagnosticLog<S>) -> Self {
        let routing_log = log.clone();
        let router = router.observing(move |routing| {
            routing_log.record(DiagnosticEvent::routing_decision(routing))
        });
        Self { router, log }
    }
}

impl<S> crate::AsrRuntime for DictationStack<S>
where
    S: DiagnosticSink,
{
    /// Report how the transcription went, never what it said (ADR-0019), and
    /// hand the result back exactly as the router produced it.
    fn transcribe(
        &self,
        audio: crate::CapturedAudio,
    ) -> Result<crate::FinalTranscription, AsrError> {
        let result = self.router.transcribe(audio);
        match &result {
            Ok(transcription) => self
                .log
                .record(DiagnosticEvent::transcription_completed(transcription)),
            Err(error) => self
                .log
                .record(DiagnosticEvent::transcription_failed(error)),
        }
        result
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
    use crate::{
        AsrRuntime, EngineAssetLifecycle, EngineAssets, EngineConfidence, EngineMetadata,
        EngineTranscriber, EngineTranscription, FinalTranscription,
    };
    use std::time::Duration;

    struct FakeProvider(TranscriptionEngine);

    impl EngineTranscriber for FakeProvider {
        fn engine(&self) -> TranscriptionEngine {
            self.0
        }

        fn metadata(&self) -> EngineMetadata {
            EngineMetadata {
                engine: self.0,
                model_id: "test",
                capability: "test",
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

    impl EngineAssetLifecycle for FakeProvider {
        fn assets(&self) -> EngineAssets {
            EngineAssets {
                installed_bytes: None,
                present: Some(true),
            }
        }
    }

    fn engine_row(
        catalogue: &TranscriptionEngineCatalogue,
        settings: &Settings,
        engine: TranscriptionEngine,
    ) -> crate::EngineView {
        let provider = catalogue
            .provider(settings, engine)
            .expect("the catalogue resolves a provider for every known engine");
        crate::EngineView::of(provider.as_ref(), settings.primary_engine == engine)
    }

    #[test]
    fn every_engine_agrees_with_itself_about_its_own_assets() {
        for model_present in [true, false] {
            let root = temp_root(&format!("row-agreement-{model_present}"));
            let files =
                crate::AppFiles::from_dirs_for_test(Some(root.join("config")), Some(root.clone()));
            let catalogue = TranscriptionEngineCatalogue::default();
            catalogue.set_model_manager(files.model_manager().unwrap());
            let settings = Settings::default();
            let model_path = catalogue
                .local_model(&settings)
                .expect("a handed-over manager brings its model directory with it")
                .path()
                .to_path_buf();
            if model_present {
                std::fs::create_dir_all(&model_path.parent().unwrap()).unwrap();
                std::fs::write(&model_path, b"ggml").unwrap();
            }

            for engine in TranscriptionEngine::ALL {
                let row = engine_row(&catalogue, &settings, engine);
                if engine == TranscriptionEngine::Whisper {
                    assert_eq!(row.assets.present, Some(model_present));
                    assert_eq!(row.assets.installed_bytes, model_present.then_some(4));
                }
                match row.assets.present {
                    Some(true) => assert!(
                        !matches!(
                            row.availability,
                            EngineAvailability::Unavailable(
                                crate::EngineUnavailable::AssetsMissing { .. }
                            )
                        ),
                        "{engine} has its assets on disk but reports them missing"
                    ),
                    Some(false) => {
                        assert!(
                            !row.metadata.system_managed,
                            "{engine} measures assets it says the operating system owns"
                        );
                        assert!(
                            !row.availability.is_available(),
                            "{engine} can run with none of its assets on disk"
                        );
                    }
                    None => assert!(row.metadata.system_managed),
                }

                assert!(
                    !row.installable
                        || matches!(
                            row.availability,
                            EngineAvailability::Unavailable(
                                crate::EngineUnavailable::AssetsMissing { .. }
                            )
                        ),
                    "{engine} offered an install button without a missing-assets reason"
                );
            }

            std::fs::remove_dir_all(&root).ok();
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "slugtale-catalogue-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn temp_model_dir(name: &str) -> PathBuf {
        let model_dir = temp_root(name).join("models");
        std::fs::create_dir_all(&model_dir).unwrap();
        model_dir
    }

    #[test]
    fn an_engine_catalogue_without_a_model_manager_offers_no_install_path() {
        let model_dir = temp_model_dir("no-manager");
        let catalogue = TranscriptionEngineCatalogue::new(Some(model_dir.clone()));
        let settings = Settings::default();

        let whisper = engine_row(&catalogue, &settings, TranscriptionEngine::Whisper);
        assert_eq!(whisper.assets, crate::EngineAssets::unmeasured());
        assert!(!whisper.installable);

        assert!(
            engine_row(&catalogue, &settings, TranscriptionEngine::Parakeet)
                .assets
                .present
                .is_some()
        );
        assert!(
            engine_row(&catalogue, &settings, TranscriptionEngine::AppleSpeech)
                .metadata
                .system_managed
        );

        std::fs::remove_dir_all(model_dir.parent().unwrap()).ok();
    }

    #[test]
    fn a_handed_over_model_manager_makes_the_local_model_the_whisper_row_reads() {
        let root = temp_root("with-manager");
        let files =
            crate::AppFiles::from_dirs_for_test(Some(root.join("config")), Some(root.clone()));
        let manager = files.model_manager().unwrap();
        std::fs::create_dir_all(manager.model_dir()).unwrap();
        std::fs::write(crate::default_model_path(manager.model_dir()), b"ggml").unwrap();
        let catalogue = TranscriptionEngineCatalogue::default();
        catalogue.set_model_manager(manager);

        let row = engine_row(
            &catalogue,
            &Settings::default(),
            TranscriptionEngine::Whisper,
        );

        assert_eq!(
            row.assets,
            crate::EngineAssets {
                installed_bytes: Some(4),
                present: Some(true),
            }
        );
        assert!(
            !row.installable,
            "a Local Model already on disk is not an install action"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn selected_model_path_wins_over_the_default_model_path() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let settings = Settings {
            model: Some("chosen.ggml".to_string()),
            ..Settings::default()
        };
        assert_eq!(
            catalogue
                .local_model(&settings)
                .map(|model| model.path().to_path_buf()),
            Some(PathBuf::from("chosen.ggml"))
        );
    }

    #[test]
    fn default_model_path_requires_a_models_directory() {
        let settings = Settings::default();
        assert_eq!(
            TranscriptionEngineCatalogue::default()
                .local_model(&settings)
                .map(|model| model.path().to_path_buf()),
            None
        );
        assert_eq!(
            TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")))
                .local_model(&settings)
                .map(|model| model.path().to_path_buf()),
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

    struct WarmCountingProvider {
        warm_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl EngineTranscriber for WarmCountingProvider {
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
            Err(AsrError::Runtime(
                "warm-up test never transcribes".to_string(),
            ))
        }

        fn warm_up(&self) -> Result<(), AsrError> {
            self.warm_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl EngineAssetLifecycle for WarmCountingProvider {
        fn assets(&self) -> EngineAssets {
            EngineAssets {
                installed_bytes: None,
                present: Some(true),
            }
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

    #[test]
    fn a_dictation_stack_without_a_runnable_engine_is_an_error_not_a_silent_path() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = SharedDiagnosticLog::new(true, collecting_sink(&lines));

        let error = TranscriptionEngineCatalogue::default()
            .dictation_stack(&Settings::default(), log)
            .err()
            .expect("a catalogue with no runnable engine must not hand out a stack");

        assert!(error.to_string().contains("could not resolve"));
    }

    #[test]
    fn a_wake_check_does_not_change_the_profile_the_next_dictation_uses() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let dictation_settings = Settings {
            speed_profile: crate::SpeedProfile::Accurate,
            ..Settings::default()
        };
        let wake_check_settings = Settings {
            speed_profile: crate::SpeedProfile::Fast,
            ..dictation_settings.clone()
        };

        let next_dictation = catalogue
            .whisper_transcription(&dictation_settings)
            .unwrap();
        let wake_check = catalogue
            .whisper_transcription(&wake_check_settings)
            .unwrap();

        assert_eq!(wake_check.speed_profile(), crate::SpeedProfile::Fast);
        assert_eq!(
            next_dictation.speed_profile(),
            crate::SpeedProfile::Accurate
        );
    }

    #[test]
    fn asking_availability_leaves_the_decode_strategy_alone() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let in_flight = catalogue
            .whisper_transcription(&Settings {
                speed_profile: crate::SpeedProfile::Accurate,
                ..Settings::default()
            })
            .unwrap();
        let other_caller = Settings {
            speed_profile: crate::SpeedProfile::Fast,
            ..Settings::default()
        };

        let _ = catalogue.availability(&other_caller);
        let _ = catalogue.effective_primary_engine(&other_caller);
        let _ = catalogue.prepare_primary_warm_up(&other_caller);

        assert_eq!(in_flight.speed_profile(), crate::SpeedProfile::Accurate);
    }

    #[test]
    fn each_dictation_stack_pins_exactly_the_profile_its_settings_carried() {
        let catalogue = TranscriptionEngineCatalogue::new(Some(PathBuf::from("models")));
        let profiles = [
            crate::SpeedProfile::Fast,
            crate::SpeedProfile::Balanced,
            crate::SpeedProfile::Accurate,
        ];

        let stacks = profiles
            .iter()
            .map(|profile| {
                catalogue
                    .whisper_transcription(&Settings {
                        speed_profile: *profile,
                        ..Settings::default()
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();

        for (profile, stack) in profiles.iter().zip(stacks) {
            assert_eq!(
                stack.speed_profile(),
                *profile,
                "{profile:?} did not reach the provider that carries it"
            );
        }
    }

    #[test]
    fn a_dictation_stack_reports_routing_and_outcomes_to_one_log() {
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = SharedDiagnosticLog::new(true, collecting_sink(&lines));
        let stack = DictationStack::new(
            SecondOpinionRouter::single(Arc::new(FakeProvider(TranscriptionEngine::Whisper))
                as Arc<dyn TranscriptionProvider>),
            log,
        );

        let transcription = stack
            .transcribe(crate::CapturedAudio::mono_16khz(vec![0.0]))
            .unwrap();
        assert_eq!(transcription.text, "");

        let recorded = lines.lock().unwrap();
        assert!(
            recorded.iter().any(|line| line.contains("routed via")),
            "routing decision missing from {recorded:?}"
        );
        assert!(
            recorded
                .iter()
                .any(|line| line.contains("final transcription completed")),
            "transcription outcome missing from {recorded:?}"
        );
    }

    fn collecting_sink(
        lines: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> impl FnMut(&str) + Send + Sync + 'static {
        let lines = std::sync::Arc::clone(lines);
        move |line: &str| lines.lock().unwrap().push(line.to_string())
    }
}
