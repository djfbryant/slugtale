use crate::{
    engine_blocked_reason, engine_that_can_run, EngineAvailability, LocalModelRef, Settings,
    TranscriptionEngine,
};
use serde::{Deserialize, Serialize};

pub trait ReadinessProbes {
    fn settings(&self) -> Settings;
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
    fn local_model(&self, settings: &Settings) -> Option<LocalModelRef>;
    fn engine_availability(
        &self,
        settings: &Settings,
    ) -> Vec<(TranscriptionEngine, EngineAvailability)>;
}

pub fn readiness_snapshot(
    probes: &dyn ReadinessProbes,
    input: impl FnOnce(&Settings) -> DictationInput,
) -> DictationActivation {
    let settings = probes.settings();
    let engines = probes.engine_availability(&settings);
    let chosen_input = input(&settings);
    let permissions = Permissions {
        microphone: probes.microphone_granted(),
        insertion: probes.insertion_granted(),
    };
    let local_model_present = probes
        .local_model(&settings)
        .is_some_and(|model| model.is_present());

    DictationActivation {
        report: readiness_report(
            &settings,
            &permissions,
            local_model_present,
            &engines,
            chosen_input,
        ),
        settings,
    }
}

struct Permissions {
    microphone: bool,
    insertion: bool,
}

pub fn missing_required_items(report: &SettingsReadinessReport) -> Vec<ReadinessItem> {
    report
        .items
        .iter()
        .filter(|item| item.required && !item.ready)
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationInput {
    Hotkey,
    VoiceActivation,
}

impl DictationInput {
    fn hotkey_required(self) -> bool {
        self == Self::Hotkey
    }
}

fn engine_in_play(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> TranscriptionEngine {
    engine_that_can_run(settings.primary_engine, engines).unwrap_or(settings.primary_engine)
}

fn whisper_model_is_required(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    engine_in_play(settings, engines) == TranscriptionEngine::Whisper
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessItemId {
    Microphone,
    TextInsertion,
    Hotkey,
    LocalModel,
    TranscriptionEngine,
    LaunchAtLogin,
}

impl ReadinessItemId {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadinessItemId::Microphone => "microphone",
            ReadinessItemId::TextInsertion => "text_insertion",
            ReadinessItemId::Hotkey => "hotkey",
            ReadinessItemId::LocalModel => "local_model",
            ReadinessItemId::TranscriptionEngine => "transcription_engine",
            ReadinessItemId::LaunchAtLogin => "launch_at_login",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ReadinessItemId::Microphone => "Microphone permission",
            ReadinessItemId::TextInsertion => "Text insertion permission",
            ReadinessItemId::Hotkey => "Hotkey",
            ReadinessItemId::LocalModel => "Local model",
            ReadinessItemId::TranscriptionEngine => "Transcription engine",
            ReadinessItemId::LaunchAtLogin => "Launch at login",
        }
    }

    pub fn pane(self) -> ReadinessPane {
        match self {
            ReadinessItemId::Hotkey => ReadinessPane::Shortcut,
            ReadinessItemId::LocalModel | ReadinessItemId::TranscriptionEngine => {
                ReadinessPane::Transcription
            }
            ReadinessItemId::Microphone | ReadinessItemId::TextInsertion => ReadinessPane::Privacy,
            ReadinessItemId::LaunchAtLogin => ReadinessPane::General,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessPane {
    Shortcut,
    Transcription,
    Privacy,
    General,
}

impl ReadinessPane {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadinessPane::Shortcut => "shortcut",
            ReadinessPane::Transcription => "transcription",
            ReadinessPane::Privacy => "privacy",
            ReadinessPane::General => "general",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessItem {
    pub id: ReadinessItemId,
    pub label: String,
    pub ready: bool,
    pub required: bool,
    pub pane: ReadinessPane,
    pub detail: Option<String>,
}

impl ReadinessItem {
    fn ready(id: ReadinessItemId, required: bool) -> Self {
        Self {
            id,
            label: id.label().to_string(),
            ready: true,
            required,
            pane: id.pane(),
            detail: None,
        }
    }

    pub fn missing(id: ReadinessItemId, required: bool) -> Self {
        Self {
            id,
            label: id.label().to_string(),
            ready: false,
            required,
            pane: id.pane(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    pub fn find(report: &SettingsReadinessReport, id: ReadinessItemId) -> Option<&ReadinessItem> {
        report.items.iter().find(|item| item.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsReadinessReport {
    pub dictation_available: bool,
    pub items: Vec<ReadinessItem>,
}

struct ReadinessTerm {
    id: ReadinessItemId,
    required: bool,
    ready: bool,
    detail: Option<String>,
}

impl ReadinessTerm {
    fn new(id: ReadinessItemId, required: bool, ready: bool) -> Self {
        Self {
            id,
            required,
            ready,
            detail: None,
        }
    }

    fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

fn readiness_report(
    settings: &Settings,
    permissions: &Permissions,
    local_model_present: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
    input: DictationInput,
) -> SettingsReadinessReport {
    let engine_blocker = engine_blocked_reason(settings.primary_engine, engines);
    let whisper_model_required = whisper_model_is_required(settings, engines);

    let terms = vec![
        ReadinessTerm::new(ReadinessItemId::Microphone, true, permissions.microphone),
        ReadinessTerm::new(ReadinessItemId::TextInsertion, true, permissions.insertion),
        ReadinessTerm::new(
            ReadinessItemId::Hotkey,
            input.hotkey_required(),
            !input.hotkey_required() || settings.hotkey.is_some(),
        ),
        ReadinessTerm::new(
            ReadinessItemId::LocalModel,
            whisper_model_required,
            local_model_present,
        )
        .with_detail(if whisper_model_required {
            None
        } else {
            Some(format!(
                "Not needed: {} transcribes without the Whisper model.",
                engine_in_play(settings, engines).display_name()
            ))
        }),
        ReadinessTerm::new(
            ReadinessItemId::TranscriptionEngine,
            true,
            engine_blocker.is_none(),
        )
        .with_detail(engine_blocker),
        ReadinessTerm::new(ReadinessItemId::LaunchAtLogin, false, true),
    ];

    SettingsReadinessReport {
        dictation_available: terms
            .iter()
            .filter(|term| term.required)
            .all(|term| term.ready),
        items: terms
            .iter()
            .map(|term| {
                readiness_item(term.id, term.required, term.ready).with_detail(term.detail.clone())
            })
            .collect(),
    }
}

fn readiness_item(id: ReadinessItemId, required: bool, ready: bool) -> ReadinessItem {
    if ready {
        ReadinessItem::ready(id, required)
    } else {
        ReadinessItem::missing(id, required)
    }
}

/// Built once, by [`readiness_snapshot`], at the activation entry point: one
/// Settings value, one probe per OS permission, one local-model answer, and the
/// report derived from those same values. Consumers in the activation read this
/// instead of re-reading global state, so they cannot disagree with each other
/// or with the start decision even if the Settings File changes mid-activation.
/// It is request-scoped by construction: a later Hotkey builds a fresh one and
/// therefore sees current OS permission state, honouring ADR-0013's
/// live-readiness rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationActivation {
    pub settings: Settings,
    pub report: SettingsReadinessReport,
}

impl DictationActivation {
    pub fn dictation_available(&self) -> bool {
        self.report.dictation_available
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeProbes {
        settings: Settings,
        microphone: bool,
        insertion: bool,
        model: Option<LocalModelRef>,
        engines: Vec<(TranscriptionEngine, EngineAvailability)>,
    }

    impl FakeProbes {
        fn all_ready() -> Self {
            Self {
                settings: configured_settings(),
                microphone: true,
                insertion: true,
                model: Some(present_model()),
                engines: whisper_available(),
            }
        }
    }

    impl ReadinessProbes for FakeProbes {
        fn settings(&self) -> Settings {
            self.settings.clone()
        }

        fn microphone_granted(&self) -> bool {
            self.microphone
        }

        fn insertion_granted(&self) -> bool {
            self.insertion
        }

        fn local_model(&self, _settings: &Settings) -> Option<LocalModelRef> {
            self.model.clone()
        }

        fn engine_availability(
            &self,
            _settings: &Settings,
        ) -> Vec<(TranscriptionEngine, EngineAvailability)> {
            self.engines.clone()
        }
    }

    fn present_model() -> LocalModelRef {
        LocalModelRef::at(std::env::temp_dir())
    }

    fn absent_model() -> LocalModelRef {
        LocalModelRef::at(unique_test_dir("no-such-model"))
    }

    struct CountingProbes {
        inner: FakeProbes,
        settings_loads: RefCell<usize>,
        mic_probes: RefCell<usize>,
        insertion_probes: RefCell<usize>,
        engine_probes: RefCell<usize>,
    }

    impl CountingProbes {
        fn all_ready() -> Self {
            Self {
                inner: FakeProbes::all_ready(),
                settings_loads: RefCell::new(0),
                mic_probes: RefCell::new(0),
                insertion_probes: RefCell::new(0),
                engine_probes: RefCell::new(0),
            }
        }
    }

    impl ReadinessProbes for CountingProbes {
        fn settings(&self) -> Settings {
            *self.settings_loads.borrow_mut() += 1;
            self.inner.settings()
        }

        fn microphone_granted(&self) -> bool {
            *self.mic_probes.borrow_mut() += 1;
            self.inner.microphone_granted()
        }

        fn insertion_granted(&self) -> bool {
            *self.insertion_probes.borrow_mut() += 1;
            self.inner.insertion_granted()
        }

        fn local_model(&self, settings: &Settings) -> Option<LocalModelRef> {
            self.inner.local_model(settings)
        }

        fn engine_availability(
            &self,
            settings: &Settings,
        ) -> Vec<(TranscriptionEngine, EngineAvailability)> {
            *self.engine_probes.borrow_mut() += 1;
            self.inner.engine_availability(settings)
        }
    }

    #[test]
    fn readiness_item_as_str_is_the_serde_wire_name_the_settings_window_reads() {
        let ids = [
            (ReadinessItemId::Microphone, "microphone"),
            (ReadinessItemId::TextInsertion, "text_insertion"),
            (ReadinessItemId::Hotkey, "hotkey"),
            (ReadinessItemId::LocalModel, "local_model"),
            (ReadinessItemId::TranscriptionEngine, "transcription_engine"),
            (ReadinessItemId::LaunchAtLogin, "launch_at_login"),
        ];

        for (id, wire) in ids {
            assert_eq!(id.as_str(), wire);
            let json = serde_json::to_value(id).expect("a readiness id serialises");
            assert_eq!(json, serde_json::Value::String(wire.to_string()));
            assert_eq!(
                serde_json::from_value::<ReadinessItemId>(json).ok(),
                Some(id),
                "{wire} does not come back as the same id"
            );
        }
    }

    #[test]
    fn readiness_pane_as_str_is_the_serde_wire_name_the_settings_window_reads() {
        let panes = [
            (ReadinessPane::Shortcut, "shortcut"),
            (ReadinessPane::Transcription, "transcription"),
            (ReadinessPane::Privacy, "privacy"),
            (ReadinessPane::General, "general"),
        ];

        for (pane, wire) in panes {
            assert_eq!(pane.as_str(), wire);
            let json = serde_json::to_value(pane).expect("a readiness pane serialises");
            assert_eq!(json, serde_json::Value::String(wire.to_string()));
            assert_eq!(
                serde_json::from_value::<ReadinessPane>(json).ok(),
                Some(pane),
                "{wire} does not come back as the same pane"
            );
        }
    }

    #[test]
    fn an_items_name_and_pane_come_from_its_id_and_cannot_disagree_with_it() {
        let probes = FakeProbes::all_ready();
        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;

        for item in &report.items {
            assert_eq!(item.label, item.id.label());
            assert_eq!(item.pane, item.id.pane());
        }
    }

    #[test]
    fn the_two_os_permissions_are_shown_on_the_privacy_pane() {
        assert_eq!(ReadinessItemId::Microphone.pane(), ReadinessPane::Privacy);
        assert_eq!(ReadinessItemId::TextInsertion.pane(), ReadinessPane::Privacy);
    }

    #[test]
    fn the_answer_is_exactly_every_required_item_being_ready() {
        let unready_by_term = [
            (
                ReadinessItemId::Microphone,
                FakeProbes {
                    microphone: false,
                    ..FakeProbes::all_ready()
                },
            ),
            (
                ReadinessItemId::TextInsertion,
                FakeProbes {
                    insertion: false,
                    ..FakeProbes::all_ready()
                },
            ),
            (
                ReadinessItemId::Hotkey,
                FakeProbes {
                    settings: Settings::default(),
                    ..FakeProbes::all_ready()
                },
            ),
            (
                ReadinessItemId::LocalModel,
                FakeProbes {
                    model: Some(absent_model()),
                    ..FakeProbes::all_ready()
                },
            ),
            (
                ReadinessItemId::TranscriptionEngine,
                FakeProbes {
                    engines: Vec::new(),
                    ..FakeProbes::all_ready()
                },
            ),
        ];

        for (term, probes) in unready_by_term {
            let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;

            assert!(
                !report.dictation_available,
                "{term:?} not ready must make dictation unavailable"
            );
            assert!(
                missing_required_items(&report)
                    .iter()
                    .any(|item| item.id == term),
                "{term:?} must be named among the required items that are missing"
            );
            assert_eq!(
                report.dictation_available,
                report
                    .items
                    .iter()
                    .filter(|item| item.required)
                    .all(|item| item.ready),
                "the answer and the item list must come from one computation"
            );
        }
    }

    #[test]
    fn an_item_is_found_by_its_id_rather_than_by_matching_a_string() {
        let probes = FakeProbes::all_ready();
        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;

        let local_model =
            ReadinessItem::find(&report, ReadinessItemId::LocalModel).expect("the item exists");
        assert!(local_model.ready);
        assert_eq!(
            ReadinessItem::find(&report, ReadinessItemId::Microphone)
                .expect("the item exists")
                .label,
            "Microphone permission"
        );
    }

    #[test]
    fn one_snapshot_probes_every_fact_exactly_once() {
        let probes = CountingProbes::all_ready();

        let snapshot = readiness_snapshot(&probes, |_| DictationInput::Hotkey);

        assert!(snapshot.report.dictation_available);
        assert_eq!(*probes.settings_loads.borrow(), 1);
        assert_eq!(*probes.mic_probes.borrow(), 1);
        assert_eq!(*probes.insertion_probes.borrow(), 1);
        assert_eq!(*probes.engine_probes.borrow(), 1);
    }

    #[test]
    fn each_readiness_term_flips_availability_and_names_itself() {
        struct Term {
            term: &'static str,
            probes: FakeProbes,
            dictation_available: bool,
            item: ReadinessItemId,
            item_ready: bool,
            item_required: bool,
        }

        let terms = [
            Term {
                term: "every term met",
                probes: FakeProbes::all_ready(),
                dictation_available: true,
                item: ReadinessItemId::LaunchAtLogin,
                item_ready: true,
                item_required: false,
            },
            Term {
                term: "no configured hotkey",
                probes: FakeProbes {
                    settings: Settings::default(),
                    ..FakeProbes::all_ready()
                },
                dictation_available: false,
                item: ReadinessItemId::Hotkey,
                item_ready: false,
                item_required: true,
            },
            Term {
                term: "microphone permission denied",
                probes: FakeProbes {
                    microphone: false,
                    ..FakeProbes::all_ready()
                },
                dictation_available: false,
                item: ReadinessItemId::Microphone,
                item_ready: false,
                item_required: true,
            },
            Term {
                term: "text insertion permission denied",
                probes: FakeProbes {
                    insertion: false,
                    ..FakeProbes::all_ready()
                },
                dictation_available: false,
                item: ReadinessItemId::TextInsertion,
                item_ready: false,
                item_required: true,
            },
            Term {
                term: "no Local Model for the engine in play",
                probes: FakeProbes {
                    model: Some(absent_model()),
                    ..FakeProbes::all_ready()
                },
                dictation_available: false,
                item: ReadinessItemId::LocalModel,
                item_ready: false,
                item_required: true,
            },
            Term {
                term: "no engine can run",
                probes: FakeProbes {
                    engines: whisper_runtime_not_built(),
                    ..FakeProbes::all_ready()
                },
                dictation_available: false,
                item: ReadinessItemId::TranscriptionEngine,
                item_ready: false,
                item_required: true,
            },
            Term {
                term: "the engine in play needs no Whisper model",
                probes: FakeProbes {
                    settings: parakeet_settings(),
                    model: Some(absent_model()),
                    engines: parakeet_available(),
                    ..FakeProbes::all_ready()
                },
                dictation_available: true,
                item: ReadinessItemId::LocalModel,
                item_ready: false,
                item_required: false,
            },
        ];

        for term in terms {
            let activation = readiness_snapshot(&term.probes, |_| DictationInput::Hotkey);
            let item = activation
                .report
                .items
                .iter()
                .find(|item| item.id == term.item)
                .unwrap_or_else(|| {
                    panic!(
                        "{}: the report names no {} item",
                        term.term,
                        term.item.as_str()
                    )
                });

            assert_eq!(
                item.ready,
                term.item_ready,
                "{}: {} readiness",
                term.term,
                term.item.as_str()
            );
            assert_eq!(
                item.required,
                term.item_required,
                "{}: {} required",
                term.term,
                term.item.as_str()
            );
            assert_eq!(
                activation.dictation_available(),
                term.dictation_available,
                "{}: dictation availability",
                term.term
            );
        }
    }

    #[test]
    fn missing_required_items_lists_only_unmet_requirements() {
        let probes = FakeProbes {
            microphone: false,
            model: Some(absent_model()),
            ..FakeProbes::all_ready()
        };
        let mut report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;
        for item in report.items.iter_mut() {
            if item.id == ReadinessItemId::LaunchAtLogin {
                item.ready = false;
            }
        }

        let ids = missing_required_items(&report)
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [ReadinessItemId::Microphone, ReadinessItemId::LocalModel]
        );
    }

    #[test]
    fn launch_at_login_is_informational_and_never_blocks_dictation() {
        let mut settings = configured_settings();
        settings.launch_at_login = false;
        let probes = FakeProbes {
            settings,
            ..FakeProbes::all_ready()
        };

        let activation = readiness_snapshot(&probes, |_| DictationInput::Hotkey);

        assert!(
            activation.dictation_available(),
            "a disabled Launch at Login preference must not block dictation"
        );
        let item = activation
            .report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::LaunchAtLogin)
            .expect("the report still names Launch at login");
        assert!(!item.required, "Launch at login is never a requirement");
        assert!(item.ready, "a disabled preference is not a problem to fix");
    }

    #[test]
    fn a_report_names_every_missing_required_item_with_its_reason() {
        let probes = FakeProbes {
            settings: Settings::default(),
            microphone: false,
            insertion: false,
            model: Some(absent_model()),
            engines: whisper_runtime_not_built(),
            ..FakeProbes::all_ready()
        };

        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;

        assert!(!report.dictation_available);
        assert_eq!(
            report.items,
            vec![
                ReadinessItem::missing(ReadinessItemId::Microphone, true),
                ReadinessItem::missing(ReadinessItemId::TextInsertion, true),
                ReadinessItem::missing(ReadinessItemId::Hotkey, true),
                ReadinessItem::missing(ReadinessItemId::LocalModel, true),
                ReadinessItem::missing(ReadinessItemId::TranscriptionEngine, true)
                    .with_detail(Some(
                        "Whisper base.en cannot run: this build was compiled without support for this engine"
                            .to_string(),
                    )),
                ReadinessItem::ready(ReadinessItemId::LaunchAtLogin, false),
            ]
        );
    }

    #[test]
    fn a_permission_denial_fails_the_activation_and_names_the_missing_item() {
        let probes = FakeProbes {
            microphone: false,
            ..FakeProbes::all_ready()
        };

        let activation = readiness_snapshot(&probes, |_| DictationInput::Hotkey);

        assert!(!activation.dictation_available());
        assert_eq!(
            activation.report.dictation_available,
            activation.dictation_available()
        );
        assert!(activation
            .report
            .items
            .iter()
            .any(|item| item.id == ReadinessItemId::Microphone && item.required && !item.ready));
    }

    #[test]
    fn every_consumer_sees_one_consistent_snapshot_even_if_settings_change_midway() {
        let settings = configured_settings();
        let probes = FakeProbes {
            settings: settings.clone(),
            ..FakeProbes::all_ready()
        };

        let activation = readiness_snapshot(&probes, |_| DictationInput::Hotkey);

        let mut changed = settings.clone();
        changed.hotkey = None;

        assert_eq!(activation.settings.hotkey, settings.hotkey);
        assert_ne!(activation.settings.hotkey, changed.hotkey);
        assert!(activation.dictation_available());
    }

    #[test]
    fn voice_activation_is_an_input_when_no_hotkey_is_configured() {
        let probes = FakeProbes {
            settings: Settings::default(),
            ..FakeProbes::all_ready()
        };

        let activation = readiness_snapshot(&probes, |_| DictationInput::VoiceActivation);

        assert!(activation.dictation_available());
        let hotkey = activation
            .report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::Hotkey)
            .unwrap();
        assert!(!hotkey.required);
        assert!(hotkey.ready);
    }

    #[test]
    fn a_build_without_the_whisper_runtime_reports_why_rather_than_ready() {
        let probes = FakeProbes {
            engines: whisper_runtime_not_built(),
            ..FakeProbes::all_ready()
        };

        let activation = readiness_snapshot(&probes, |_| DictationInput::Hotkey);
        let engine = activation
            .report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::TranscriptionEngine)
            .unwrap();

        assert!(!activation.dictation_available());
        assert!(!engine.ready);
        assert!(engine.required);
        assert_eq!(
            engine.detail.as_deref(),
            Some(
                "Whisper base.en cannot run: this build was compiled without support for this engine"
            )
        );
    }

    #[test]
    fn a_whisper_only_build_that_can_transcribe_reports_no_engine_blocker() {
        let activation = readiness_snapshot(&FakeProbes::all_ready(), |_| DictationInput::Hotkey);
        let engine = activation
            .report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::TranscriptionEngine)
            .unwrap();

        assert!(activation.dictation_available());
        assert_eq!(
            engine,
            &ReadinessItem::ready(ReadinessItemId::TranscriptionEngine, true)
        );
    }

    #[test]
    fn the_local_model_is_optional_and_says_why_when_another_engine_runs() {
        let probes = FakeProbes {
            settings: parakeet_settings(),
            model: Some(absent_model()),
            engines: parakeet_available(),
            ..FakeProbes::all_ready()
        };

        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::LocalModel)
            .unwrap();

        assert!(report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing(ReadinessItemId::LocalModel, false).with_detail(Some(
                "Not needed: Parakeet TDT v2 transcribes without the Whisper model.".to_string()
            ))
        );
    }

    #[test]
    fn the_whisper_model_is_required_again_when_the_fallback_is_whisper() {
        let probes = FakeProbes {
            settings: parakeet_settings(),
            model: Some(absent_model()),
            engines: vec![
                (
                    TranscriptionEngine::Parakeet,
                    EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                        detail: "Parakeet assets are not installed.".to_string(),
                    }),
                ),
                (TranscriptionEngine::Whisper, EngineAvailability::Available),
            ],
            ..FakeProbes::all_ready()
        };

        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::LocalModel)
            .unwrap();

        assert!(!report.dictation_available);
        assert_eq!(
            local_model,
            &ReadinessItem::missing(ReadinessItemId::LocalModel, true)
        );
    }

    #[test]
    fn the_whisper_model_stays_required_when_nothing_can_run_for_a_whisper_user() {
        let probes = FakeProbes {
            model: Some(absent_model()),
            engines: whisper_runtime_not_built(),
            ..FakeProbes::all_ready()
        };

        let report = readiness_snapshot(&probes, |_| DictationInput::Hotkey).report;
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::LocalModel)
            .unwrap();

        assert!(!report.dictation_available);
        assert!(local_model.required);
    }

    struct CatalogueProbes {
        catalogue: crate::TranscriptionEngineCatalogue,
        settings: Settings,
    }

    impl ReadinessProbes for CatalogueProbes {
        fn settings(&self) -> Settings {
            self.settings.clone()
        }

        fn microphone_granted(&self) -> bool {
            true
        }

        fn insertion_granted(&self) -> bool {
            true
        }

        fn local_model(&self, settings: &Settings) -> Option<LocalModelRef> {
            self.catalogue.local_model(settings)
        }

        fn engine_availability(
            &self,
            settings: &Settings,
        ) -> Vec<(TranscriptionEngine, EngineAvailability)> {
            self.catalogue.availability(settings)
        }
    }

    fn readiness_report_over(
        settings: &Settings,
        model_dir: &std::path::Path,
    ) -> Vec<ReadinessItem> {
        let probes = CatalogueProbes {
            catalogue: crate::TranscriptionEngineCatalogue::new(Some(model_dir.to_path_buf())),
            settings: settings.clone(),
        };

        readiness_snapshot(&probes, |_| DictationInput::Hotkey)
            .report
            .items
    }

    fn local_model_item(items: &[ReadinessItem]) -> &ReadinessItem {
        items
            .iter()
            .find(|item| item.id == ReadinessItemId::LocalModel)
            .expect("every report names the Local Model")
    }

    #[test]
    fn readiness_uses_the_default_local_model_when_settings_model_is_unset() {
        let model_dir = unique_test_dir("readiness-default-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(crate::default_model_path(&model_dir), b"model").unwrap();

        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        };
        let items = readiness_report_over(&settings, &model_dir);

        assert!(local_model_item(&items).ready);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn readiness_uses_the_settings_model_when_it_is_the_one_that_exists() {
        let model_dir = unique_test_dir("readiness-custom-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        let custom = model_dir.join("custom-model.bin");
        std::fs::write(&custom, b"model").unwrap();

        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            model: Some(custom.to_string_lossy().to_string()),
            ..Settings::default()
        };
        let items = readiness_report_over(&settings, &model_dir);

        assert!(local_model_item(&items).ready);

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn readiness_reports_a_stale_settings_model_as_missing_rather_than_falling_back() {
        let model_dir = unique_test_dir("readiness-stale-model-setting");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(crate::default_model_path(&model_dir), b"model").unwrap();

        let settings = Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            model: Some(
                model_dir
                    .join("missing-custom-model.bin")
                    .to_string_lossy()
                    .to_string(),
            ),
            ..Settings::default()
        };
        let items = readiness_report_over(&settings, &model_dir);

        assert_eq!(
            local_model_item(&items),
            &ReadinessItem::missing(ReadinessItemId::LocalModel, true)
        );

        std::fs::remove_dir_all(&model_dir).ok();
    }

    #[test]
    fn readiness_and_engine_availability_agree_about_the_same_file() {
        for settings_model in [None, Some("custom-present.bin"), Some("custom-deleted.bin")] {
            for default_present in [false, true] {
                let model_dir = unique_test_dir("model-agreement");
                std::fs::create_dir_all(&model_dir).unwrap();
                if default_present {
                    std::fs::write(crate::default_model_path(&model_dir), b"model").unwrap();
                }
                std::fs::write(model_dir.join("custom-present.bin"), b"model").unwrap();

                let settings = Settings {
                    hotkey: Some("cmd+shift+d".to_string()),
                    model: settings_model
                        .map(|name| model_dir.join(name).to_string_lossy().to_string()),
                    ..Settings::default()
                };
                let items = readiness_report_over(&settings, &model_dir);
                let present = crate::TranscriptionEngineCatalogue::new(Some(model_dir.clone()))
                    .local_model(&settings)
                    .expect("a model directory always resolves a model path")
                    .is_present();
                let availability =
                    crate::TranscriptionEngineCatalogue::new(Some(model_dir.clone()))
                        .whisper_provider(&settings)
                        .expect("a model directory always resolves a model path")
                        .availability();

                assert_eq!(
                    local_model_item(&items).ready,
                    present,
                    "settings.model={settings_model:?} default_present={default_present}"
                );
                assert_eq!(
                    availability.is_available(),
                    present && cfg!(feature = "local-whisper-runtime"),
                    "settings.model={settings_model:?} default_present={default_present}"
                );

                std::fs::remove_dir_all(&model_dir).ok();
            }
        }
    }

    fn parakeet_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            primary_engine: crate::TranscriptionEngine::Parakeet,
            ..Settings::default()
        }
    }

    fn parakeet_available() -> Vec<(TranscriptionEngine, EngineAvailability)> {
        vec![
            (
                TranscriptionEngine::Whisper,
                EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                    detail: "The Whisper model has not been downloaded yet.".to_string(),
                }),
            ),
            (TranscriptionEngine::Parakeet, EngineAvailability::Available),
        ]
    }

    fn whisper_available() -> Vec<(TranscriptionEngine, EngineAvailability)> {
        vec![(TranscriptionEngine::Whisper, EngineAvailability::Available)]
    }

    fn whisper_runtime_not_built() -> Vec<(TranscriptionEngine, EngineAvailability)> {
        vec![(
            TranscriptionEngine::Whisper,
            EngineAvailability::Unavailable(crate::EngineUnavailable::RuntimeNotBuilt),
        )]
    }

    fn configured_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            ..Settings::default()
        }
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-readiness-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
