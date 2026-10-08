//! Select Fermion MLX on supported Apple silicon Macs; retain portable ONNX.
use crate::{
    AsrError, AssetInstall, CapturedAudio, DownloadProgress, EngineAssetLifecycle, EngineAssets,
    EngineAvailability, EngineMetadata, EngineTranscriber, EngineTranscription, ParakeetProvider,
    TranscriptionEngine, PHONON_2,
};
use std::path::Path;

#[cfg(all(
    target_os = "macos",
    target_arch = "aarch64",
    feature = "local-phonon-mlx"
))]
use crate::macos::phonon_mlx as mlx;

pub enum PhononProvider {
    Onnx(ParakeetProvider),
    #[cfg(all(
        target_os = "macos",
        target_arch = "aarch64",
        feature = "local-phonon-mlx"
    ))]
    Mlx(mlx::MlxProvider),
}

impl PhononProvider {
    pub fn new(model_dir: &Path) -> Self {
        #[cfg(all(
            target_os = "macos",
            target_arch = "aarch64",
            feature = "local-phonon-mlx"
        ))]
        if mlx::supported_os() {
            return Self::Mlx(mlx::MlxProvider::new(model_dir.join("phonon-2-mlx")));
        }
        Self::Onnx(ParakeetProvider::for_model(
            &PHONON_2,
            PHONON_2.asset_dir(model_dir),
        ))
    }

    /// The engine behind this selection. Both are
    /// [`EngineTranscriber`] + [`EngineAssetLifecycle`] providers, so the two
    /// halves below forward to whichever one this build chose.
    fn transcriber(&self) -> &dyn EngineTranscriber {
        match self {
            Self::Onnx(provider) => provider,
            #[cfg(all(
                target_os = "macos",
                target_arch = "aarch64",
                feature = "local-phonon-mlx"
            ))]
            Self::Mlx(provider) => provider,
        }
    }

    /// The asset lifecycle behind this selection. Read separately from
    /// [`Self::transcriber`] so the dictation half of this provider never
    /// reaches an install or a removal.
    fn asset_lifecycle(&self) -> &dyn EngineAssetLifecycle {
        match self {
            Self::Onnx(provider) => provider,
            #[cfg(all(
                target_os = "macos",
                target_arch = "aarch64",
                feature = "local-phonon-mlx"
            ))]
            Self::Mlx(provider) => provider,
        }
    }

    pub fn unload(&self) {
        match self {
            Self::Onnx(provider) => provider.unload(),
            #[cfg(all(
                target_os = "macos",
                target_arch = "aarch64",
                feature = "local-phonon-mlx"
            ))]
            Self::Mlx(provider) => provider.unload(),
        }
    }

    pub fn shutdown(&self) {
        match self {
            Self::Onnx(provider) => provider.shutdown(),
            #[cfg(all(
                target_os = "macos",
                target_arch = "aarch64",
                feature = "local-phonon-mlx"
            ))]
            Self::Mlx(provider) => provider.shutdown(),
        }
    }
}

impl EngineTranscriber for PhononProvider {
    fn engine(&self) -> TranscriptionEngine {
        TranscriptionEngine::Phonon
    }
    fn metadata(&self) -> EngineMetadata {
        self.transcriber().metadata()
    }
    fn availability(&self) -> EngineAvailability {
        self.transcriber().availability()
    }
    fn warm_up(&self) -> Result<(), AsrError> {
        match self {
            Self::Onnx(provider) => provider.warm_up(),
            #[cfg(all(
                target_os = "macos",
                target_arch = "aarch64",
                feature = "local-phonon-mlx"
            ))]
            Self::Mlx(provider) => provider.warm_up(),
        }
    }
    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        self.transcriber().transcribe(audio)
    }
}

/// Phonon-2's own bytes, whichever runtime this build selected: MLX installs a
/// private Python runtime and the model through the engine, and ONNX downloads
/// pinned NVIDIA files. Both are the engine's business, and neither is the
/// dictation path's.
impl EngineAssetLifecycle for PhononProvider {
    fn assets(&self) -> EngineAssets {
        self.asset_lifecycle().assets()
    }
    fn can_install_assets(&self) -> bool {
        self.asset_lifecycle().can_install_assets()
    }
    fn install_assets(
        &self,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        self.asset_lifecycle().install_assets(progress)
    }
    fn remove_assets(&self) -> Result<(), String> {
        self.asset_lifecycle().remove_assets()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_selects_the_built_runtime_and_exposes_setup() {
        let root =
            std::env::temp_dir().join(format!("slugtale-phonon-selection-{}", std::process::id()));
        let catalogue = crate::TranscriptionEngineCatalogue::new(Some(root));
        let provider = catalogue
            .provider(&crate::Settings::default(), TranscriptionEngine::Phonon)
            .unwrap();
        let row = crate::EngineView::of(provider.as_ref(), true);
        #[cfg(all(
            target_os = "macos",
            target_arch = "aarch64",
            feature = "local-phonon-mlx"
        ))]
        if mlx::supported_os() {
            assert!(row.metadata.revision.contains("MLX"));
            assert!(row.installable);
            assert!(matches!(
                row.availability,
                EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing { .. })
            ));
            return;
        }
        assert!(row.metadata.revision.contains("Phonon-2-ONNX"));
        assert_eq!(row.installable, cfg!(feature = "local-parakeet-runtime"));
    }
}
