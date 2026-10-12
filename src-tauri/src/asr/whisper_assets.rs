
use super::whisper_engine::WhisperTranscriptionProvider;
use crate::EngineAssetLifecycle;

impl EngineAssetLifecycle for WhisperTranscriptionProvider {
    fn assets(&self) -> crate::EngineAssets {
        match self.model_manager() {
            Some(manager) => {
                let status = manager.status();
                crate::EngineAssets {
                    installed_bytes: status.bytes,
                    present: Some(status.present),
                }
            }
            None => crate::EngineAssets::unmeasured(),
        }
    }

    fn can_install_assets(&self) -> bool {
        self.model_manager().is_some()
    }

    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(crate::DownloadProgress),
    ) -> Result<crate::AssetInstall, String> {
        let installed = self
            .model_manager()
            .ok_or_else(|| {
                crate::transcription_engine::assets_cannot_be_installed(
                    crate::TranscriptionEngine::Whisper,
                )
            })?
            .download_default(&crate::HttpModelDownloader, on_progress)
            .map_err(|error| error.to_string())?;

        Ok(crate::AssetInstall {
            warm_up: installed.present,
        })
    }

    fn remove_assets(&self) -> Result<(), String> {
        self.model_manager()
            .ok_or_else(|| {
                crate::transcription_engine::assets_cannot_be_removed(
                    crate::TranscriptionEngine::Whisper,
                )
            })?
            .delete_default()
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::unique_test_dir;
    use super::*;
    use crate::{LocalModelRef, LocalWhisperRuntime, SpeedProfile, DEFAULT_MODEL_FILENAME};
    use std::sync::Arc;

    #[test]
    fn a_whisper_row_reports_the_local_model_it_would_open() {
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
}
