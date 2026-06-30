use crate::Settings;
use serde::{Deserialize, Serialize};

/// Platform Adapter boundary (ADR-0021) for the OS-specific facts that gate
/// dictation: microphone permission and text insertion permission.
pub trait PlatformReadiness {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
}

/// Dictation Readiness (ADR-0013): dictation is only available once microphone
/// permission, text insertion permission, a configured hotkey, and a local model
/// are all ready.
pub fn dictation_ready(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
    local_model_ready: bool,
) -> bool {
    settings.hotkey.is_some()
        && platform.microphone_granted()
        && platform.insertion_granted()
        && local_model_ready
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessItem {
    pub id: String,
    pub label: String,
    pub ready: bool,
    pub required: bool,
}

impl ReadinessItem {
    pub fn ready(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: true,
            required,
        }
    }

    pub fn missing(id: &str, label: &str, required: bool) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            ready: false,
            required,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsReadinessReport {
    pub dictation_available: bool,
    pub items: Vec<ReadinessItem>,
}

pub fn settings_readiness_report(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
    local_model_ready: bool,
) -> SettingsReadinessReport {
    SettingsReadinessReport {
        dictation_available: dictation_ready(settings, platform, local_model_ready),
        items: vec![
            readiness_item(
                "microphone",
                "Microphone permission",
                true,
                platform.microphone_granted(),
            ),
            readiness_item(
                "text_insertion",
                "Text insertion permission",
                true,
                platform.insertion_granted(),
            ),
            readiness_item("hotkey", "Hotkey", true, settings.hotkey.is_some()),
            readiness_item("local_model", "Local model", true, local_model_ready),
            readiness_item("launch_at_login", "Launch at login", false, true),
        ],
    }
}

fn readiness_item(id: &str, label: &str, required: bool, ready: bool) -> ReadinessItem {
    if ready {
        ReadinessItem::ready(id, label, required)
    } else {
        ReadinessItem::missing(id, label, required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_is_not_ready_when_nothing_is_ready() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        assert!(!dictation_ready(&Settings::default(), &platform, false));
    }
    #[test]
    fn dictation_is_not_ready_without_microphone_permission() {
        let platform = FakePlatform {
            microphone: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(&configured_settings(), &platform, true));
    }
    #[test]
    fn dictation_is_not_ready_without_insertion_permission() {
        let platform = FakePlatform {
            insertion: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(&configured_settings(), &platform, true));
    }
    #[test]
    fn dictation_is_not_ready_without_configured_hotkey() {
        let settings = Settings {
            hotkey: None,
            ..Settings::default()
        };
        assert!(!dictation_ready(
            &settings,
            &FakePlatform::all_ready(),
            true
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_local_model() {
        assert!(!dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false
        ));
    }
    #[test]
    fn dictation_is_ready_when_all_requirements_are_met() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true
        ));
    }
    #[test]
    fn settings_readiness_report_shows_missing_required_items() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        let report = settings_readiness_report(&Settings::default(), &platform, false);

        assert!(!report.dictation_available);
        assert_eq!(
            report.items,
            vec![
                ReadinessItem::missing("microphone", "Microphone permission", true),
                ReadinessItem::missing("text_insertion", "Text insertion permission", true),
                ReadinessItem::missing("hotkey", "Hotkey", true),
                ReadinessItem::missing("local_model", "Local model", true),
                ReadinessItem::ready("launch_at_login", "Launch at login", false),
            ]
        );
    }
    #[test]
    fn settings_readiness_report_allows_dictation_when_required_items_are_ready() {
        let report =
            settings_readiness_report(&configured_settings(), &FakePlatform::all_ready(), true);

        assert!(report.dictation_available);
        assert!(report
            .items
            .iter()
            .filter(|item| item.required)
            .all(|item| item.ready));
    }
    #[test]
    fn model_readiness_is_supplied_outside_the_platform_adapter() {
        let report =
            settings_readiness_report(&configured_settings(), &FakePlatform::all_ready(), false);
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == "local_model")
            .unwrap();

        assert!(!report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing("local_model", "Local model", true)
        );
    }

    struct FakePlatform {
        microphone: bool,
        insertion: bool,
    }

    impl FakePlatform {
        fn all_ready() -> Self {
            Self {
                microphone: true,
                insertion: true,
            }
        }
    }

    impl PlatformReadiness for FakePlatform {
        fn microphone_granted(&self) -> bool {
            self.microphone
        }
        fn insertion_granted(&self) -> bool {
            self.insertion
        }
    }

    fn configured_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        }
    }
}
