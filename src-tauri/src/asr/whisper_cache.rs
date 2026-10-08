//! The cached Whisper runtime: one loaded model per model path, shared by every
//! caller whose Settings name that path.
//!
//! The cache exists because reading and parsing the model file is expensive and
//! must happen once, not on every dictation. It also owns two lifetimes the
//! engine catalogue needs to be able to end:
//!
//! - [`WhisperRuntimeCache::release`] drops the reference without ending the
//!   cache, so a warm-up can start again later.
//! - [`WhisperRuntimeCache::shutdown`] stops accepting work and releases the
//!   loaded context now. Tauri's default `run` path ends in `process::exit`,
//!   which skips Rust destructors, so this is the only place ggml's Metal
//!   globals are torn down in a determinate order (slugtale-p1u).

use super::LocalWhisperRuntime;
use crate::LocalModelRef;
use std::sync::{Arc, Mutex};

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

#[cfg(test)]
mod tests {
    use super::super::unique_test_dir;
    use super::*;
    use crate::DEFAULT_MODEL_FILENAME;

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
}
