//! App update commands: the manual check against the fixed GitHub Release
//! endpoint, and opening the compiled-in release page. There is deliberately no
//! install command — the app never replaces itself.

#[tauri::command]
pub(crate) async fn check_for_app_update(
    app: tauri::AppHandle,
) -> Result<slugtale_lib::AppUpdateView, String> {
    slugtale_lib::check_for_app_update(&app).await
}

#[tauri::command]
pub(crate) fn open_app_update_release() -> Result<(), String> {
    slugtale_lib::open_app_update_release()
}
