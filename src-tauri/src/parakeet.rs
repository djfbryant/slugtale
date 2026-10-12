//! **Installation is explicit and pinned; inference never touches the network.**
//! The weights are 631 MiB of NVIDIA's model that Slugtale does not bundle and
//! may not silently fetch (ADR-0010, ADR-0001). They arrive only through
//! [`install_parakeet_assets`], driven by a user action in Settings, against a
//! pinned upstream revision and a SHA-256 digest per file. After that,
//! [`ParakeetProvider::transcribe`] reads local files and nothing else — the
//! network-denied test in slugtale-vjs.5 depends on there being no lazy fetch
//! anywhere on this path.
//! **Parakeet reports no confidence, and that is not the same as low
//! confidence.** The TDT decoder does score its tokens, but `parakeet-rs` 0.3.6
//! throws the scores away: its `TimedToken` carries only `text`, `start`, and
//! `end`. So this provider returns [`crate::EngineConfidence::unreported`]
//! rather than a number invented from token count or duration, and the Second
//! Opinion router escalates *from* Parakeet on the transcript anomaly rules
//! instead of on a threshold. Revisit if the crate starts exposing per-token
//! log-probabilities.

mod assets;

use assets::{
    delete_parakeet_assets, install_parakeet_assets, parakeet_asset_status, TdtModelFiles,
    PARAKEET_FILES, PHONON_FILES,
};

use crate::{
    AsrError, CapturedAudio, EngineAssetLifecycle, EngineAssets, EngineAvailability,
    EngineMetadata, EngineTranscriber, EngineTranscription, EngineUnavailable, TranscriptionEngine,
};
use crate::{AssetInstall, DownloadProgress};
#[cfg(feature = "local-parakeet-runtime")]
use crate::{EngineConfidence, FinalTranscription};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

pub struct TdtModel {
    engine: TranscriptionEngine,
    model_id: &'static str,
    revision: &'static str,
    source_url: &'static str,
    attribution: &'static str,
    /// The CC BY 4.0 "indicate if changes were made" clause.
    modifications: &'static str,
    capability: &'static str,
    files: TdtModelFiles,
}

impl TdtModel {
    pub fn asset_dir(&self, model_dir: &Path) -> PathBuf {
        self.files.asset_dir(model_dir)
    }

    fn name(&self) -> &'static str {
        self.engine.display_name()
    }

}

/// Both models are released under CC BY 4.0, which is an attribution licence:
/// Slugtale may use them commercially and offline, but must credit the authors,
/// link the licence, and state what was changed. Those three obligations are
/// the reason [`EngineMetadata`] has `attribution` and `modifications` fields at
/// all.
const TDT_LICENSE: &str = "CC BY 4.0";
const TDT_LICENSE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";

/// NVIDIA Parakeet TDT 0.6B v2. Slugtale does not train or fine-tune the
/// weights; the changes are the ONNX export and the int8 quantisation carried
/// out upstream, which Slugtale installs as-is.
pub const PARAKEET_TDT_V2: TdtModel = TdtModel {
    engine: TranscriptionEngine::Parakeet,
    model_id: "nvidia/parakeet-tdt-0.6b-v2",
    revision: "istupakov/parakeet-tdt-0.6b-v2-onnx@0bbb45a3365852604aef28b538a8f066f4ccaa85",
    source_url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/tree/0bbb45a3365852604aef28b538a8f066f4ccaa85",
    attribution: "Speech recognition by NVIDIA Parakeet TDT 0.6B v2 (© NVIDIA Corporation), used under CC BY 4.0.",
    modifications: concat!(
        "Not the original NeMo checkpoint: exported to ONNX and quantised to int8 upstream ",
        "(istupakov/parakeet-tdt-0.6b-v2-onnx). Slugtale installs those artefacts unmodified ",
        "and does not train, fine-tune, or otherwise alter the weights."
    ),
    capability: "NVIDIA's fast, accurate English transcriber. Best for longer dictations \
                 — paragraphs, emails, documents — where punctuation quality matters \
                 as much as speed.",
    files: PARAKEET_FILES,
};

/// Phonon-2 by Fermion Research, itself a changed Parakeet TDT 0.6B v3, so the
/// credit names both. The ONNX export is a third party's, not Fermion's.
pub const PHONON_2: TdtModel = TdtModel {
    engine: TranscriptionEngine::Phonon,
    model_id: "FermionResearch/Phonon-2",
    revision: "tiyuvta/Phonon-2-ONNX@12c9688bbc4fc52d23c1a66ca873fd3ac6ed4408",
    source_url:
        "https://huggingface.co/tiyuvta/Phonon-2-ONNX/tree/12c9688bbc4fc52d23c1a66ca873fd3ac6ed4408",
    attribution: concat!(
        "Speech recognition by Phonon-2 (Fermion Research), derived from NVIDIA Parakeet TDT ",
        "0.6B v3 (© NVIDIA Corporation); both used under CC BY 4.0."
    ),
    modifications: concat!(
        "Not Fermion's own runtime format: exported to ONNX upstream by Tiyuvta ",
        "(tiyuvta/Phonon-2-ONNX), with the encoder's weights stored exactly as two 4-bit planes. ",
        "Slugtale installs those artefacts unmodified and does not train, fine-tune, or otherwise ",
        "alter the weights."
    ),
    capability: "A newer-generation model in the Parakeet family, derived from NVIDIA \
                 Parakeet TDT v3 by Fermion Research. An alternative transcription voice \
                 to Parakeet TDT v2 — try it if it suits you better.",
    files: PHONON_FILES,
};

#[cfg(any(test, feature = "local-parakeet-runtime"))]
fn parakeet_intra_threads(available: usize) -> usize {
    available.clamp(1, 8)
}

pub struct ParakeetProvider {
    model: &'static TdtModel,
    asset_dir: PathBuf,
    availability: Mutex<EngineAvailability>,
    #[cfg(feature = "local-parakeet-runtime")]
    session_slot: Mutex<Option<parakeet_rs::ParakeetTDT>>,
    #[cfg(feature = "local-parakeet-runtime")]
    shutting_down: std::sync::atomic::AtomicBool,
}

impl ParakeetProvider {
    pub fn new(asset_dir: PathBuf) -> Self {
        Self::for_model(&PARAKEET_TDT_V2, asset_dir)
    }

    pub fn for_model(model: &'static TdtModel, asset_dir: PathBuf) -> Self {
        let availability = probe_availability(model, &asset_dir);
        Self {
            model,
            asset_dir,
            availability: Mutex::new(availability),
            #[cfg(feature = "local-parakeet-runtime")]
            session_slot: Mutex::new(None),
            #[cfg(feature = "local-parakeet-runtime")]
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn refresh_availability(&self) -> EngineAvailability {
        let refreshed = probe_availability(self.model, &self.asset_dir);
        *lock(&self.availability) = refreshed.clone();
        refreshed
    }
}

fn probe_availability(model: &TdtModel, asset_dir: &Path) -> EngineAvailability {
    if !cfg!(feature = "local-parakeet-runtime") {
        return EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt);
    }

    let status = parakeet_asset_status(asset_dir, &model.files);
    if status.present {
        return EngineAvailability::Available;
    }

    EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
        detail: format!(
            "The {} model has not been installed yet ({} of {} files missing).",
            model.name(),
            status.missing.len(),
            model.files.assets.len()
        ),
    })
}

#[cfg(any(test, feature = "local-parakeet-runtime"))]
fn decode_failed(model: &TdtModel) -> AsrError {
    AsrError::Runtime(format!(
        "{} could not decode this recording. Try dictating again.",
        model.name()
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn validate_captured_audio(model: &TdtModel, audio: &CapturedAudio) -> Result<(), AsrError> {
    if audio.sample_rate_hz != 16_000 {
        return Err(AsrError::UnsupportedAudio(format!(
            "{} transcription expects 16 kHz mono f32 samples",
            model.name()
        )));
    }
    if audio.samples.is_empty() {
        return Err(AsrError::UnsupportedAudio(format!(
            "{} transcription needs at least one audio sample",
            model.name()
        )));
    }
    Ok(())
}

impl EngineTranscriber for ParakeetProvider {
    fn engine(&self) -> TranscriptionEngine {
        self.model.engine
    }

    fn metadata(&self) -> EngineMetadata {
        EngineMetadata {
            engine: self.model.engine,
            model_id: self.model.model_id,
            capability: self.model.capability,
            revision: self.model.revision,
            approximate_bytes: Some(self.model.files.total_bytes()),
            source_url: Some(self.model.source_url),
            license: TDT_LICENSE,
            license_url: TDT_LICENSE_URL,
            attribution: Some(self.model.attribution),
            modifications: Some(self.model.modifications),
            system_managed: false,
            supported_platforms: "macOS, Windows, and Linux",
        }
    }

    fn availability(&self) -> EngineAvailability {
        lock(&self.availability).clone()
    }

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        validate_captured_audio(self.model, audio)?;
        self.transcribe_validated(audio)
    }

    fn warm_up(&self) -> Result<(), AsrError> {
        self.load_session_now()
    }
}

impl EngineAssetLifecycle for ParakeetProvider {
    fn assets(&self) -> EngineAssets {
        let status = parakeet_asset_status(&self.asset_dir, &self.model.files);
        EngineAssets {
            installed_bytes: Some(status.installed_bytes),
            present: Some(status.present),
        }
    }

    fn can_install_assets(&self) -> bool {
        true
    }

    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        install_parakeet_assets(
            &self.asset_dir,
            &self.model.files,
            &crate::HttpModelDownloader,
            on_progress,
        )
        .map_err(|error| error.to_string())?;
        self.refresh_availability();

        Ok(AssetInstall {
            warm_up: false,
        })
    }

    fn remove_assets(&self) -> Result<(), String> {
        delete_parakeet_assets(&self.asset_dir, &self.model.files)
            .map_err(|error| error.to_string())?;
        self.refresh_availability();
        Ok(())
    }
}

#[cfg(not(feature = "local-parakeet-runtime"))]
impl ParakeetProvider {
    fn load_session_now(&self) -> Result<(), AsrError> {
        Err(self.runtime_not_built())
    }

    pub fn shutdown(&self) {}

    pub fn unload(&self) {}

    fn transcribe_validated(
        &self,
        _audio: &CapturedAudio,
    ) -> Result<EngineTranscription, AsrError> {
        Err(self.runtime_not_built())
    }

    fn runtime_not_built(&self) -> AsrError {
        AsrError::EngineUnavailable {
            engine: self.model.engine,
            reason: EngineUnavailable::RuntimeNotBuilt,
        }
    }
}

#[cfg(feature = "local-parakeet-runtime")]
impl ParakeetProvider {
    fn load_session_now(&self) -> Result<(), AsrError> {
        self.with_session(|_| Ok(()))
    }

    ///
    /// Tauri's default `run` path ends in `process::exit`, which skips Rust
    /// destructors — the same reason `WhisperRuntimeCache::shutdown` exists
    /// (slugtale-p1u). ONNX Runtime holds a C++ environment and, on the Core ML
    /// path, Core ML/Metal globals; dropping the sessions here, under the lock
    pub fn shutdown(&self) {
        use std::sync::atomic::Ordering;

        self.shutting_down.store(true, Ordering::Release);
        self.drop_session_under_its_lock();
    }

    pub fn unload(&self) {
        self.drop_session_under_its_lock();
    }

    fn drop_session_under_its_lock(&self) {
        lock(&self.session_slot).take();
    }

    /// Run an operation against the cached session, loading it on first use.
    ///
    /// Holding the lock across the whole operation serialises decoding, which
    /// `parakeet-rs` requires anyway (`transcribe_samples` takes `&mut self`),
    /// and makes shutdown safe by construction.
    fn with_session<T>(
        &self,
        operation: impl FnOnce(&mut parakeet_rs::ParakeetTDT) -> Result<T, AsrError>,
    ) -> Result<T, AsrError> {
        use std::sync::atomic::Ordering;

        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "the Parakeet runtime is shutting down".to_string(),
            ));
        }

        let mut session = lock(&self.session_slot);
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "the Parakeet runtime is shutting down".to_string(),
            ));
        }

        if session.is_none() {
            let status = parakeet_asset_status(&self.asset_dir, &self.model.files);
            if !status.present {
                let reason = EngineUnavailable::AssetsMissing {
                    detail: format!(
                        "The {} model is not installed in {} ({} of {} files missing). Install it from Settings.",
                        self.model.name(),
                        self.asset_dir.display(),
                        status.missing.len(),
                        self.model.files.assets.len()
                    ),
                };
                *lock(&self.availability) = EngineAvailability::Unavailable(reason.clone());
                return Err(AsrError::EngineUnavailable {
                    engine: self.model.engine,
                    reason,
                });
            }

            *session = Some(self.load_session()?);
        }

        operation(session.as_mut().expect("session was just loaded"))
    }

    fn load_session(&self) -> Result<parakeet_rs::ParakeetTDT, AsrError> {
        // `mut` is used only on the Core ML build; the CPU build takes the
        // default provider and never reassigns.
        #[allow(unused_mut)]
        let mut config =
            parakeet_rs::ExecutionConfig::new().with_intra_threads(parakeet_intra_threads(
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1),
            ));

        // The Core ML execution provider is opt-in and macOS-only. `parakeet-rs`
        // warns that Core ML can be *slower* than CPU for these graphs, because
        // their dynamic input shapes stop it planning for the Neural Engine and
        // it ends up claiming nodes it then runs on the CPU anyway. It is behind
        #[cfg(all(feature = "local-parakeet-runtime-coreml", target_os = "macos"))]
        {
            config = config
                .with_execution_provider(parakeet_rs::ExecutionProvider::CoreML)
                .with_coreml_cache_dir(self.asset_dir.join("coreml-cache"));
        }

        parakeet_rs::ParakeetTDT::from_pretrained(&self.asset_dir, Some(config)).map_err(|error| {
            AsrError::Runtime(format!(
                "the {} model in {} could not be loaded ({error}). Re-install it from Settings.",
                self.model.name(),
                self.asset_dir.display()
            ))
        })
    }

    fn transcribe_validated(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        use parakeet_rs::Transcriber;

        let started = std::time::Instant::now();
        let result = self.with_session(|session| {
            // `parakeet-rs` takes ownership of the samples, so the clone is the
            // price of the borrowing signature every provider shares — the same
            session
                .transcribe_samples(
                    audio.samples.clone(),
                    audio.sample_rate_hz,
                    1,
                    Some(parakeet_rs::TimestampMode::Words),
                )
                .map_err(|_| decode_failed(self.model))
        })?;

        Ok(EngineTranscription {
            engine: self.model.engine,
            transcription: FinalTranscription::plain(result.text.trim()),
            alternatives: Vec::new(),
            confidence: EngineConfidence::unreported(),
            latency: started.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineView, TranscriptionProvider};
    use std::sync::Arc;

    #[test]
    fn metadata_carries_every_cc_by_obligation_settings_has_to_render() {
        // CC BY 4.0 requires the credit, the licence link, and a statement of
        // what changed. Settings renders these verbatim, so losing one here is a
        // licensing failure rather than a cosmetic one.
        let provider = ParakeetProvider::new(unique_test_dir("metadata"));
        let metadata = provider.metadata();

        assert_eq!(metadata.engine, TranscriptionEngine::Parakeet);
        assert_eq!(metadata.model_id, "nvidia/parakeet-tdt-0.6b-v2");
        assert_eq!(metadata.license, "CC BY 4.0");
        assert_eq!(
            metadata.license_url,
            "https://creativecommons.org/licenses/by/4.0/"
        );
        assert!(metadata
            .attribution
            .expect("CC BY 4.0 obliges an NVIDIA credit")
            .contains("NVIDIA"));
        let modifications = metadata
            .modifications
            .expect("CC BY 4.0 obliges a statement of changes");
        assert!(modifications.contains("ONNX"));
        assert!(modifications.contains("int8"));
        assert!(!metadata.system_managed);
    }

    #[test]
    fn phonon_answers_as_its_own_engine_and_credits_both_authors() {
        // Phonon-2 is a changed Parakeet v3, so CC BY 4.0 obliges a credit to
        // Fermion Research and to NVIDIA, and its own files and directory.
        let model_dir = unique_test_dir("phonon-metadata");
        let provider = ParakeetProvider::for_model(&PHONON_2, PHONON_2.asset_dir(&model_dir));
        let metadata = provider.metadata();

        assert_eq!(provider.engine(), TranscriptionEngine::Phonon);
        assert_eq!(metadata.engine, TranscriptionEngine::Phonon);
        assert_eq!(metadata.model_id, "FermionResearch/Phonon-2");
        assert_eq!(metadata.license, "CC BY 4.0");
        let attribution = metadata.attribution.unwrap();
        assert!(attribution.contains("Fermion Research"));
        assert!(attribution.contains("NVIDIA"));
        assert!(metadata.modifications.unwrap().contains("ONNX"));
        assert_eq!(metadata.approximate_bytes, Some(PHONON_FILES.total_bytes()));
        assert!(metadata
            .source_url
            .unwrap()
            .contains(PHONON_2.revision.split_once('@').unwrap().1));
        assert_ne!(
            PHONON_2.asset_dir(&model_dir),
            PARAKEET_TDT_V2.asset_dir(&model_dir)
        );
        #[cfg(feature = "local-parakeet-runtime")]
        assert_eq!(
            EngineView::of(&provider, false)
                .unavailable_reason
                .as_deref(),
            Some("The Phonon-2 model has not been installed yet (3 of 3 files missing).")
        );
    }

    #[test]
    fn metadata_pins_a_commit_rather_than_a_branch() {
        let provider = ParakeetProvider::new(unique_test_dir("revision"));
        let metadata = provider.metadata();

        assert_eq!(metadata.revision, PARAKEET_TDT_V2.revision);
        let commit = PARAKEET_TDT_V2
            .revision
            .split_once('@')
            .expect("the revision names a repository and a commit")
            .1;
        assert_eq!(commit.len(), 40, "a pinned revision is a full commit hash");
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(metadata
            .source_url
            .expect("Settings links to what it installs")
            .contains(commit));
    }

    #[test]
    fn metadata_reports_every_platform_onnx_runtime_covers() {
        let provider = ParakeetProvider::new(unique_test_dir("platforms"));

        assert_eq!(
            provider.metadata().supported_platforms,
            "macOS, Windows, and Linux"
        );
    }

    #[test]
    fn metadata_size_matches_what_an_install_actually_downloads() {
        let provider = ParakeetProvider::new(unique_test_dir("size"));

        assert_eq!(
            provider.metadata().approximate_bytes,
            Some(PARAKEET_FILES.total_bytes())
        );
        assert!((600..700).contains(&(PARAKEET_FILES.total_bytes() / (1024 * 1024))));
    }

    #[test]
    fn availability_is_answered_from_cache_not_from_the_filesystem() {
        let asset_dir = unique_test_dir("availability-cache");
        std::fs::create_dir_all(&asset_dir).unwrap();
        let provider = ParakeetProvider::new(asset_dir.clone());
        let at_construction = provider.availability();

        for asset in PARAKEET_FILES.assets {
            std::fs::write(asset_dir.join(asset.filename), b"x").unwrap();
        }
        assert_eq!(provider.availability(), at_construction);

        std::fs::remove_dir_all(&asset_dir).ok();
        assert_eq!(provider.availability(), at_construction);
    }

    #[test]
    fn refreshing_availability_republishes_the_cached_answer() {
        let asset_dir = unique_test_dir("availability-refresh");
        let provider = ParakeetProvider::new(asset_dir.clone());

        let refreshed = provider.refresh_availability();

        assert_eq!(refreshed, provider.availability());
        assert!(!refreshed.is_available());
    }

    #[cfg(not(feature = "local-parakeet-runtime"))]
    #[test]
    fn a_build_without_the_runtime_says_so_instead_of_offering_an_install() {
        let provider = ParakeetProvider::new(unique_test_dir("runtime-not-built"));

        assert_eq!(
            provider.availability(),
            EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt)
        );
        assert!(!EngineUnavailable::RuntimeNotBuilt.is_user_resolvable());
    }

    #[cfg(feature = "local-parakeet-runtime")]
    #[test]
    fn a_runtime_build_without_assets_offers_an_install() {
        let provider = ParakeetProvider::new(unique_test_dir("assets-missing"));

        match provider.availability() {
            EngineAvailability::Unavailable(reason) => {
                assert!(reason.is_user_resolvable());
                assert!(matches!(reason, EngineUnavailable::AssetsMissing { .. }));
            }
            EngineAvailability::Available => panic!("no assets are installed"),
        }
    }

    #[test]
    fn a_partly_installed_model_accounts_for_exactly_the_bytes_still_missing() {
        let asset_dir = unique_test_dir("row-missing-bytes");
        std::fs::create_dir_all(&asset_dir).unwrap();
        let installed = &PARAKEET_FILES.assets[0];
        std::fs::write(
            asset_dir.join(installed.filename),
            vec![0u8; installed.bytes as usize],
        )
        .unwrap();
        let row = EngineView::of(&ParakeetProvider::new(asset_dir.clone()), true);

        assert_eq!(row.assets.present, Some(false));
        assert_eq!(row.assets.installed_bytes, Some(installed.bytes));
        assert_eq!(
            row.metadata.approximate_bytes.unwrap() - row.assets.installed_bytes.unwrap(),
            PARAKEET_FILES.total_bytes() - installed.bytes,
            "the row has to let the user work out how much is left to fetch"
        );
        #[cfg(feature = "local-parakeet-runtime")]
        assert_eq!(
            row.unavailable_reason.as_deref(),
            Some("The Parakeet TDT v2 model has not been installed yet (2 of 3 files missing).")
        );

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn a_row_says_whether_this_build_can_do_anything_with_an_install_button() {
        let asset_dir = unique_test_dir("row-installable");
        let row = EngineView::of(&ParakeetProvider::new(asset_dir.clone()), false);

        #[cfg(feature = "local-parakeet-runtime")]
        {
            assert!(row.installable);
            assert_eq!(
                row.unavailable_reason.as_deref(),
                Some(
                    "The Parakeet TDT v2 model has not been installed yet (3 of 3 files missing)."
                )
            );
        }
        #[cfg(not(feature = "local-parakeet-runtime"))]
        {
            assert!(
                !row.installable,
                "a build that cannot decode the model cannot install it"
            );
            assert_eq!(
                row.unavailable_reason.as_deref(),
                Some("this build was compiled without support for this engine")
            );
        }

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn removing_the_assets_takes_the_engine_back_to_unavailable() {
        let asset_dir = unique_test_dir("row-remove");
        std::fs::create_dir_all(&asset_dir).unwrap();
        for asset in PARAKEET_FILES.assets {
            std::fs::File::create(asset_dir.join(asset.filename))
                .unwrap()
                .set_len(asset.bytes)
                .unwrap();
        }
        let provider = ParakeetProvider::new(asset_dir.clone());
        assert_eq!(
            provider.assets().installed_bytes,
            Some(PARAKEET_FILES.total_bytes())
        );
        assert_eq!(provider.assets().present, Some(true));

        provider.remove_assets().unwrap();

        assert_eq!(provider.assets().present, Some(false));
        assert_eq!(provider.assets().installed_bytes, Some(0));
        assert_eq!(
            provider.availability(),
            probe_availability(&PARAKEET_TDT_V2, &asset_dir)
        );
        #[cfg(feature = "local-parakeet-runtime")]
        assert!(!provider.availability().is_available());
        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn transcription_rejects_audio_that_is_not_16_khz_mono() {
        let provider = ParakeetProvider::new(unique_test_dir("wrong-rate"));

        let error = provider
            .transcribe(&CapturedAudio {
                sample_rate_hz: 44_100,
                samples: vec![0.0; 44_100],
            })
            .unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Parakeet TDT v2 transcription expects 16 kHz mono f32 samples".to_string()
            )
        );
    }

    #[test]
    fn transcription_rejects_an_empty_recording() {
        let provider = ParakeetProvider::new(unique_test_dir("empty-audio"));

        let error = provider
            .transcribe(&CapturedAudio::mono_16khz(Vec::new()))
            .unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Parakeet TDT v2 transcription needs at least one audio sample".to_string()
            )
        );
    }

    #[test]
    fn a_missing_model_is_an_actionable_error_and_never_a_panic() {
        let provider = ParakeetProvider::new(unique_test_dir("missing-model"));

        let error = provider
            .transcribe(&CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .unwrap_err();

        match &error {
            AsrError::EngineUnavailable { engine, reason } => {
                assert_eq!(*engine, TranscriptionEngine::Parakeet);
                if cfg!(feature = "local-parakeet-runtime") {
                    assert!(matches!(reason, EngineUnavailable::AssetsMissing { .. }));
                    assert!(error.to_string().contains("Settings"));
                } else {
                    assert_eq!(*reason, EngineUnavailable::RuntimeNotBuilt);
                }
            }
            other => panic!("expected an unavailable engine, got {other:?}"),
        }

        assert!(provider
            .transcribe(&CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .is_err());
        assert!(!provider.availability().is_available());
    }

    #[test]
    fn shutdown_is_idempotent_and_leaves_the_provider_answerable() {
        let provider = ParakeetProvider::new(unique_test_dir("shutdown"));

        provider.shutdown();
        provider.shutdown();

        assert_eq!(provider.engine(), TranscriptionEngine::Parakeet);
        assert!(!provider.availability().is_available());
    }

    #[cfg(feature = "local-parakeet-runtime")]
    #[test]
    fn nothing_loads_after_shutdown() {
        let provider = ParakeetProvider::new(unique_test_dir("shutdown-blocks-load"));
        provider.shutdown();

        assert_eq!(
            provider.warm_up().unwrap_err(),
            AsrError::Runtime("the Parakeet runtime is shutting down".to_string())
        );
    }

    #[test]
    fn warm_up_through_the_shared_interface_reaches_both_tdt_models() {
        for (model, expected) in [
            (&PARAKEET_TDT_V2, TranscriptionEngine::Parakeet),
            (&PHONON_2, TranscriptionEngine::Phonon),
        ] {
            let provider: Arc<dyn TranscriptionProvider> =
                Arc::new(ParakeetProvider::for_model(model, unique_test_dir("warm-up")));

            let error = provider.warm_up().expect_err(
                "warm-up through the trait must reach the TDT loader, not the \
                 interface's default no-op",
            );
            match error {
                AsrError::EngineUnavailable { engine, reason } => {
                    assert_eq!(engine, expected);
                    if cfg!(feature = "local-parakeet-runtime") {
                        assert!(
                            matches!(reason, EngineUnavailable::AssetsMissing { .. }),
                            "a runtime build should report missing weights, got {reason:?}"
                        );
                    } else {
                        assert_eq!(reason, EngineUnavailable::RuntimeNotBuilt);
                    }
                }
                other => panic!("expected an unavailable engine, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_decode_failure_says_nothing_about_what_was_decoded() {
        assert_eq!(
            decode_failed(&PARAKEET_TDT_V2),
            AsrError::Runtime(
                "Parakeet TDT v2 could not decode this recording. Try dictating again."
                    .to_string()
            )
        );
    }

    #[test]
    fn onnx_threads_stay_within_what_the_machine_offers() {
        assert_eq!(parakeet_intra_threads(4), 4);
        assert_eq!(parakeet_intra_threads(32), 8);
        assert_eq!(parakeet_intra_threads(0), 1);
    }

    #[test]
    fn the_engine_never_prints_anything() {
        let source = concat!(
            include_str!("parakeet.rs"),
            include_str!("parakeet/assets.rs")
        );
        for stem in ["print", "eprint", "dbg"] {
            for macro_name in [format!("{stem}!"), format!("{stem}ln!")] {
                assert!(
                    !source.contains(&macro_name),
                    "{macro_name} must not appear in the Parakeet engine"
                );
            }
        }
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-parakeet-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
