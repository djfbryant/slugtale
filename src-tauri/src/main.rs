#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;

#[derive(Default)]
struct RecordingFeedbackState(Mutex<slugtale_lib::RecordingFeedback>);

#[tauri::command]
fn show_settings(app: tauri::AppHandle) {
    slugtale_lib::show_settings(app);
}

/// Drive the recording surface (ADR-0014) from a dictation lifecycle event:
/// play the start/stop sound and show or hide the Dictation Bar. The bar's Stop
/// and Cancel controls and its Escape key all route here; the hotkey lifecycle
/// (slugtale-h8z.3) will route `start` and `stop` here too once wired.
#[tauri::command]
fn dictation_event(
    app: tauri::AppHandle,
    feedback: tauri::State<'_, RecordingFeedbackState>,
    event: String,
) -> Result<(), String> {
    let event = match event.as_str() {
        "start" => slugtale_lib::DictationEvent::Start,
        "stop" => slugtale_lib::DictationEvent::Stop,
        "cancel" => slugtale_lib::DictationEvent::Cancel,
        other => return Err(format!("unknown dictation event: {other}")),
    };

    let effect = {
        let mut guard = feedback
            .0
            .lock()
            .map_err(|_| "recording feedback mutex poisoned".to_string())?;
        guard.on_event(event)
    };

    if let Some(sound) = effect.sound {
        let _ = slugtale_lib::play_dictation_sound(sound);
    }

    if effect.bar_visible {
        show_dictation_bar(&app);
    } else {
        hide_dictation_bar(&app);
    }

    Ok(())
}

fn show_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        position_bottom_center(&window);
        let _ = window.show();
        // Focus so the Dictation Bar can receive Escape-to-cancel while another
        // app holds the text target.
        let _ = window.set_focus();
    }
}

fn hide_dictation_bar(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("dictation-bar") {
        let _ = window.hide();
    }
}

/// Place the Dictation Bar near the bottom-center of the active display, above
/// the Dock, matching the resident dictation pills users know from FluidVoice
/// and Wispr Flow.
fn position_bottom_center(window: &tauri::WebviewWindow) {
    let monitor = match window.current_monitor() {
        Ok(Some(monitor)) => monitor,
        _ => match window.primary_monitor() {
            Ok(Some(monitor)) => monitor,
            _ => return,
        },
    };

    let Ok(size) = window.outer_size() else {
        return;
    };

    let screen = monitor.size();
    let origin = monitor.position();
    // ~96pt of breathing room above the bottom edge, scaled to the display.
    let margin = (96.0 * monitor.scale_factor()) as i32;
    let x = origin.x + (screen.width as i32 - size.width as i32) / 2;
    let y = origin.y + screen.height as i32 - size.height as i32 - margin;
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

#[tauri::command]
fn get_settings_readiness(app: tauri::AppHandle) -> slugtale_lib::SettingsReadinessReport {
    let settings = load_current_settings(&app);
    let platform = CurrentPlatform::new(settings.model.as_deref().map(PathBuf::from));
    slugtale_lib::settings_readiness_report(&settings, &platform)
}

#[tauri::command]
fn get_local_model_status(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    Ok(slugtale_lib::local_model_status(&model_dir(&app)?))
}

#[tauri::command]
async fn download_local_model(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<slugtale_lib::DownloadProgress>,
) -> Result<slugtale_lib::LocalModelStatus, String> {
    let dir = model_dir(&app)?;
    let status = tauri::async_runtime::spawn_blocking(move || {
        // Throttle IPC traffic: forward progress after ~1 MB of new data, plus
        // the initial and final updates, so the bar stays smooth without
        // flooding the channel with thousands of tiny messages.
        let mut last_sent = 0u64;
        let mut forward = move |progress: slugtale_lib::DownloadProgress| {
            let complete = progress
                .total
                .is_some_and(|total| progress.downloaded >= total);
            if progress.downloaded == 0 || complete || progress.downloaded - last_sent >= 1_048_576
            {
                last_sent = progress.downloaded;
                let _ = on_progress.send(progress);
            }
        };
        slugtale_lib::ensure_default_model(&dir, &slugtale_lib::HttpModelDownloader, &mut forward)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    save_model_setting(&app, status.present.then(|| status.path.clone()))?;
    Ok(status)
}

#[tauri::command]
fn delete_local_model(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    let status =
        slugtale_lib::delete_default_model(&model_dir(&app)?).map_err(|error| error.to_string())?;
    save_model_setting(&app, None)?;
    Ok(status)
}

#[tauri::command]
fn reveal_model_location(app: tauri::AppHandle) -> Result<(), String> {
    let location = slugtale_lib::reveal_location(&model_dir(&app)?);
    slugtale_lib::open_in_file_manager(&location).map_err(|error| error.to_string())
}

#[tauri::command]
async fn transcribe_captured_audio(
    app: tauri::AppHandle,
    cache: tauri::State<'_, WhisperRuntimeCache>,
    sample_rate_hz: u32,
    samples: Vec<f32>,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let model_path = settings
        .model
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(&app)?));
    let runtime = cache.runtime_for(&model_path);
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    tauri::async_runtime::spawn_blocking(move || {
        slugtale_lib::transcribe_captured_audio(&*runtime, audio).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Caches the loaded Whisper runtime across transcriptions so the model file is
/// read from disk once rather than on every call. The runtime is rebuilt only
/// when the configured model path changes.
#[derive(Default)]
struct WhisperRuntimeCache(Mutex<Option<Arc<slugtale_lib::LocalWhisperRuntime>>>);

impl WhisperRuntimeCache {
    fn runtime_for(&self, model_path: &Path) -> Arc<slugtale_lib::LocalWhisperRuntime> {
        let mut guard = self.0.lock().expect("whisper runtime cache mutex poisoned");
        if let Some(existing) = guard.as_ref() {
            if existing.model_path() == model_path {
                return existing.clone();
            }
        }

        let runtime = Arc::new(slugtale_lib::LocalWhisperRuntime::new(
            model_path.to_path_buf(),
        ));
        *guard = Some(runtime.clone());
        runtime
    }
}

#[derive(Default)]
struct CurrentPlatform {
    model_path: Option<PathBuf>,
}

impl CurrentPlatform {
    fn new(model_path: Option<PathBuf>) -> Self {
        Self { model_path }
    }

    #[cfg(target_os = "macos")]
    fn macos_platform(&self) -> slugtale_lib::MacosPlatform {
        slugtale_lib::MacosPlatform::new(self.model_path.clone().unwrap_or_default())
    }
}

fn settings_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

fn model_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("models"))
        .map_err(|error| error.to_string())
}

fn load_current_settings(app: &tauri::AppHandle) -> slugtale_lib::Settings {
    settings_path(app)
        .map(|path| slugtale_lib::load_settings(&path))
        .unwrap_or_default()
}

fn save_model_setting(app: &tauri::AppHandle, model_path: Option<PathBuf>) -> Result<(), String> {
    let path = settings_path(app).ok_or_else(|| "could not resolve settings path".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let mut settings = slugtale_lib::load_settings(&path);
    settings.model = model_path.map(|path| path.to_string_lossy().to_string());
    slugtale_lib::save_settings(&path, &settings).map_err(|error| error.to_string())
}

impl slugtale_lib::PlatformReadiness for CurrentPlatform {
    fn microphone_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().microphone_granted();
        }

        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn insertion_granted(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            return self.macos_platform().insertion_granted();
        }

        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn local_model_present(&self) -> bool {
        self.model_path.as_ref().is_some_and(|path| path.exists())
    }
}

fn main() {
    tauri::Builder::default()
        .manage(WhisperRuntimeCache::default())
        .manage(RecordingFeedbackState::default())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            slugtale_lib::setup_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if slugtale_lib::hides_on_close(window.label()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            show_settings,
            get_settings_readiness,
            get_local_model_status,
            download_local_model,
            delete_local_model,
            reveal_model_location,
            transcribe_captured_audio,
            dictation_event
        ])
        .run(tauri::generate_context!())
        .expect("error while running Slugtale");
}
