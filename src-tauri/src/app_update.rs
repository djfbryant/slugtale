use tauri_plugin_updater::UpdaterExt;

pub const APP_UPDATE_RELEASE_URL: &str = "https://github.com/djfbryant/slugtale/releases/latest";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppUpdateView {
    Current,
    Available { version: String },
}

pub async fn check_for_app_update(app: &tauri::AppHandle) -> Result<AppUpdateView, String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater.check().await.map_err(|error| error.to_string())?;
    Ok(match update {
        Some(update) => AppUpdateView::Available {
            version: update.version,
        },
        None => AppUpdateView::Current,
    })
}

pub fn open_app_update_release() -> Result<(), String> {
    open::that(APP_UPDATE_RELEASE_URL).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_results_have_stable_frontend_statuses() {
        assert_eq!(
            serde_json::to_value(AppUpdateView::Current).expect("serialize current update status"),
            serde_json::json!({ "status": "current" })
        );
        assert_eq!(
            serde_json::to_value(AppUpdateView::Available {
                version: "0.2.0".to_string(),
            })
            .expect("serialize available update status"),
            serde_json::json!({ "status": "available", "version": "0.2.0" })
        );
    }

    #[test]
    fn the_release_page_is_fixed_to_the_slugtale_repository() {
        assert_eq!(
            APP_UPDATE_RELEASE_URL,
            "https://github.com/djfbryant/slugtale/releases/latest"
        );
    }
}
