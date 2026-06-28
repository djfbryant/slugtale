#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use tauri::Manager;

#[tauri::command]
fn show_settings(app: tauri::AppHandle) {
    slugtale_lib::show_settings(app);
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
fn download_local_model(app: tauri::AppHandle) -> Result<slugtale_lib::LocalModelStatus, String> {
    let status =
        slugtale_lib::ensure_default_model(&model_dir(&app)?, &slugtale_lib::HttpModelDownloader)
            .map_err(|error| error.to_string())?;
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
fn transcribe_captured_audio(
    app: tauri::AppHandle,
    sample_rate_hz: u32,
    samples: Vec<f32>,
) -> Result<slugtale_lib::FinalTranscription, String> {
    let settings = load_current_settings(&app);
    let model_path = settings
        .model
        .map(PathBuf::from)
        .unwrap_or(slugtale_lib::default_model_path(&model_dir(&app)?));
    let runtime = slugtale_lib::LocalWhisperRuntime::new(model_path);
    let audio = slugtale_lib::CapturedAudio {
        sample_rate_hz,
        samples,
    };

    slugtale_lib::transcribe_captured_audio(&runtime, audio).map_err(|error| error.to_string())
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
            transcribe_captured_audio
        ])
        .run(tauri::generate_context!())
        .expect("error while running Slugtale");
}
