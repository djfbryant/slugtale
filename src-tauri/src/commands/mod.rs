
pub(crate) mod dictation;
pub(crate) mod engines;
pub(crate) mod platform;
pub(crate) mod settings;
pub(crate) mod updates;
pub(crate) mod usage;

use tauri::Manager;

use slugtale_lib::{AppFiles, TypingChallengeOpen};

pub(crate) fn app_files(app: &tauri::AppHandle) -> AppFiles {
    app.state::<AppFiles>().inner().clone()
}

/// Read the current Settings. Every command that *changes* one goes through the
/// store's transaction instead — `update_settings` or `update_settings_and_apply`
/// on [`AppFiles`] — because this returns a snapshot, and saving a snapshot read
/// before another writer changed it is how a background model install used to
/// overwrite a newer choice.
pub(crate) fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    app_files(app).settings()
}

pub(crate) fn update_current_settings(
    app: &tauri::AppHandle,
    change: impl FnOnce(&mut slugtale_lib::Settings) -> Result<(), String>,
) -> Result<slugtale_lib::Settings, String> {
    app_files(app).update_settings(change)
}

pub(crate) fn model_manager(
    app: &tauri::AppHandle,
) -> Result<slugtale_lib::LocalModelManager, String> {
    app_files(app).model_manager()
}

pub(crate) fn record_diagnostic_event(
    app: &tauri::AppHandle,
    event: slugtale_lib::DiagnosticEvent,
) {
    app_files(app).record_diagnostic_event(event);
}

pub(crate) fn record_stage_timing(app: &tauri::AppHandle, timing: &slugtale_lib::StageTiming) {
    app_files(app)
        .transcription_timing_log()
        .record_stages(timing);
}

pub(crate) fn warm_effective_primary_engine(app: &tauri::AppHandle) {
    let settings = load_current_settings(app);
    let catalogue = app.state::<slugtale_lib::TranscriptionEngineCatalogue>();
    let mut timer = slugtale_lib::StageTimer::start("engine-warm-up");
    timer.note("preferred_engine", settings.primary_engine.id());
    if let Some(reason) = catalogue.preferred_engine_unavailable(&settings) {
        timer.note("preferred_unavailable", reason);
    }
    let Some(warm_up) = catalogue.begin_primary_warm_up(&settings) else {
        timer.mark("choose_engine");
        timer.note("result", "no engine can run");
        record_stage_timing(app, &timer.finish());
        return;
    };
    timer.mark("choose_engine");
    timer.note("engine", warm_up.engine().id());
    let (generation, expected) = warm_up.generation();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = warm_up.run();
        let stale = generation.load(std::sync::atomic::Ordering::SeqCst) != expected;
        timer.mark("load_model");
        timer.note(
            "result",
            match result {
                Ok(()) if stale => "skipped: a newer warm-up replaced it".to_string(),
                Ok(()) => "ok".to_string(),
                Err(error) => error.to_string(),
            },
        );
        record_stage_timing(&app, &timer.finish());
    });
}

pub(crate) fn dictation_input_is_inert(app: &tauri::AppHandle) -> bool {
    app.state::<TypingChallengeOpen>().get()
}
