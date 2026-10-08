//! Select Fermion MLX on supported Apple silicon Macs; retain portable ONNX.
use crate::{
    AsrError, AssetInstall, CapturedAudio, DownloadProgress, EngineAssets, EngineAvailability,
    EngineMetadata, EngineTranscription, ParakeetProvider, TranscriptionEngine,
    TranscriptionProvider, PHONON_2,
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

    fn provider(&self) -> &dyn TranscriptionProvider {
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

impl TranscriptionProvider for PhononProvider {
    fn engine(&self) -> TranscriptionEngine {
        TranscriptionEngine::Phonon
    }
    fn metadata(&self) -> EngineMetadata {
        self.provider().metadata()
    }
    fn availability(&self) -> EngineAvailability {
        self.provider().availability()
    }
    fn assets(&self) -> EngineAssets {
        self.provider().assets()
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
        self.provider().transcribe(audio)
    }
    fn can_install_assets(&self) -> bool {
        self.provider().can_install_assets()
    }
    fn install_assets(
        &self,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        self.provider().install_assets(progress)
    }
    fn remove_assets(&self) -> Result<(), String> {
        self.provider().remove_assets()
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
