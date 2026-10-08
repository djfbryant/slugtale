//! Transcription Engine and Local Model commands: the Settings rows for every
//! engine, engine asset install and removal, and the Whisper Local Model's
//! download, status, delete and reveal actions.

use std::sync::Arc;

use tauri::Manager;

use slugtale_lib::TranscriptionProvider;

use super::{
    load_current_settings, model_manager, update_current_settings, warm_effective_primary_engine,
};

/// The provider for one Transcription Engine, as the Settings surface sees it.
///
/// Every engine question in this module goes through here and through
/// [`slugtale_lib::EngineView`], so no command knows which engines exist.
/// `None` means this build resolved no provider for the engine, and the engine
/// itself words why.
fn engine_provider(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
    engine: slugtale_lib::TranscriptionEngine,
) -> Result<Arc<dyn TranscriptionProvider>, String> {
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .provider(settings, engine)
        .ok_or_else(|| engine.missing_provider_reason().to_string())
}

/// Build one engine's Settings row from its provider. Never re-probes: every
/// fact is read off a provider the catalogue already built, matching how the
/// dictation path itself asks these questions.
fn build_engine_view(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
    engine: slugtale_lib::TranscriptionEngine,
) -> Result<slugtale_lib::EngineView, String> {
    let provider = engine_provider(app, settings, engine)?;
    Ok(slugtale_lib::EngineView::of(
        provider.as_ref(),
        settings.primary_engine == engine,
    ))
}

/// Engine availability for the readiness report, asked of the same providers the
/// dictation path uses so the two cannot disagree.
pub(crate) fn current_engine_availability(
    app: &tauri::AppHandle,
    settings: &slugtale_lib::Settings,
) -> Vec<(
    slugtale_lib::TranscriptionEngine,
    slugtale_lib::EngineAvailability,
)> {
    app.state::<slugtale_lib::TranscriptionEngineCatalogue>()
        .availability(settings)
}

/// Every Transcription Engine Settings can show, in [`slugtale_lib::TranscriptionEngine::ALL`]
/// order. Read-only and non-blocking: see [`build_engine_view`].
#[tauri::command]
pub(crate) fn transcription_engines(
    app: tauri::AppHandle,
) -> Result<Vec<slugtale_lib::EngineView>, String> {
    let settings = load_current_settings(&app);
    slugtale_lib::TranscriptionEngine::ALL
        .into_iter()
        .map(|engine| build_engine_view(&app, &settings, engine))
        .collect()
}

/// Persist the chosen primary engine and Second Opinion mode (slugtale-vjs.4).
/// Mirrors [`save_transcription_settings`]: no check that the chosen engine can
/// actually run, because availability can change after the choice is made and
/// is resolved fresh by `transcription_router` on the next dictation instead
/// (see [`slugtale_lib::apply_engine_settings`]).
#[tauri::command]
pub(crate) fn set_transcription_engines(
    app: tauri::AppHandle,
    primary_engine: slugtale_lib::TranscriptionEngine,
    second_opinion: slugtale_lib::SecondOpinionMode,
) -> Result<slugtale_lib::Settings, String> {
    let settings = update_current_settings(&app, |settings| {
        slugtale_lib::apply_engine_settings(settings, primary_engine, second_opinion);
        Ok(())
    })?;
    // Start warming the newly effective engine now so the first dictation
    // after the change does not pay for a cold model load.
    warm_effective_primary_engine(&app);
    Ok(settings)
}

/// The progress sink every download command forwards to.
///
/// Throttle IPC traffic: the initial update, then one per ~1 MB, plus the final
/// update (slugtale-dtl). Written once because a 64 KiB-chunked download would
/// otherwise flood the channel, and because two copies of this rule is two
/// chances to change one of them.
fn forward_download_progress(
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> impl FnMut(slugtale_lib::DownloadProgress) {
    slugtale_lib::throttled_progress(move |progress| {
        let _ = on_progress.send(progress);
    })
}

/// Install one engine's assets as an explicit user action (slugtale-vjs.4).
///
/// One call, because the mechanism is the engine's own: some engines fetch pinned
/// artefacts over HTTP and report progress here, and one asks the operating
/// system to install assets it owns, which blocks and reports nothing.
#[tauri::command]
pub(crate) async fn install_engine_assets(
    app: tauri::AppHandle,
    engine: slugtale_lib::TranscriptionEngine,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<slugtale_lib::EngineView, String> {
    let settings = load_current_settings(&app);
    let provider = engine_provider(&app, &settings, engine)?;
    let install = tauri::async_runtime::spawn_blocking({
        let mut forward = forward_download_progress(on_progress);
        move || provider.install_assets(&mut forward)
    })
    .await
    .map_err(|error| error.to_string())??;

    if install.warm_up {
        warm_effective_primary_engine(&app);
    }

    build_engine_view(&app, &load_current_settings(&app), engine)
}

/// Remove one engine's installed assets as an explicit user action
/// (slugtale-vjs.4). An engine whose assets the operating system owns refuses in
/// its own words rather than pretending to free space Slugtale never claimed.
#[tauri::command]
pub(crate) fn remove_engine_assets(
    app: tauri::AppHandle,
    engine: slugtale_lib::TranscriptionEngine,
) -> Result<slugtale_lib::EngineView, String> {
    let provider = engine_provider(&app, &load_current_settings(&app), engine)?;
    provider.remove_assets()?;

    build_engine_view(&app, &load_current_settings(&app), engine)
}

#[tauri::command]
pub(crate) fn get_local_model_status(
    app: tauri::AppHandle,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    Ok(model_manager(&app)?.status())
}

#[tauri::command]
pub(crate) async fn download_local_model(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    let manager = model_manager(&app)?;
    let status = tauri::async_runtime::spawn_blocking({
        let mut forward = forward_download_progress(on_progress);
        move || {
            manager
                .download_default(&slugtale_lib::HttpModelDownloader, &mut forward)
                .map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    if status.present {
        warm_effective_primary_engine(&app);
    }
    Ok(status)
}

#[tauri::command]
pub(crate) fn delete_local_model(
    app: tauri::AppHandle,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    model_manager(&app)?
        .delete_default()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn reveal_model_location(app: tauri::AppHandle) -> Result<(), String> {
    model_manager(&app)?
        .open_in_file_manager()
        .map_err(|error| error.to_string())
}
