#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn show_settings(app: tauri::AppHandle) {
    slugtale_lib::show_settings(app);
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            slugtale_lib::setup_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![show_settings])
        .run(tauri::generate_context!())
        .expect("error while running Slugtale");
}
