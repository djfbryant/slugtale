use crate::{
    engine_blocked_reason, engine_that_can_run, EngineAvailability, LocalModelRef, Settings,
    TranscriptionEngine,
};
use serde::{Deserialize, Serialize};

/// Platform Adapter boundary (ADR-0021) for the OS-specific facts that gate
/// dictation: microphone permission and text insertion permission.
pub trait PlatformReadiness {
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
}

/// The five facts every readiness snapshot is built from. Both snapshot paths —
/// the Settings pane's report and an activation's snapshot — probe through this
/// one interface so their answers cannot drift apart (slugtale-g1o.6: each
/// probe is paid for exactly once per snapshot).
pub trait ReadinessProbes {
    /// The Settings value this snapshot sees. Loaded once and shared.
    fn settings(&self) -> Settings;
    fn microphone_granted(&self) -> bool;
    fn insertion_granted(&self) -> bool;
    /// The Local Model this dictation would open, resolved from the same
    /// Settings and the same model directory the engines resolve it from. The
    /// answer is [`LocalModelRef::is_present`], not a second opinion about the
    /// default path, so readiness and Engine Availability cannot disagree.
    fn local_model(&self, settings: &Settings) -> Option<LocalModelRef>;
    /// Asked of the same providers the dictation path uses, so the report and
    /// the engine decision cannot disagree.
    fn engine_availability(
        &self,
        settings: &Settings,
    ) -> Vec<(TranscriptionEngine, EngineAvailability)>;
}

/// One readiness snapshot over any probe source. Every consumer reads the same
/// Settings value, permission answers, model answer, and engine table.
pub fn readiness_snapshot(
    probes: &dyn ReadinessProbes,
    input: impl FnOnce(&Settings) -> DictationInput,
) -> DictationActivation {
    let settings = probes.settings();
    let engines = probes.engine_availability(&settings);
    let chosen_input = input(&settings);
    let permissions = ProbedPermissions {
        microphone: probes.microphone_granted(),
        insertion: probes.insertion_granted(),
    };
    let local_model_present = probes
        .local_model(&settings)
        .is_some_and(|model| model.is_present());
    DictationActivation::build_for_input(
        settings,
        &permissions,
        local_model_present,
        engines,
        chosen_input,
    )
}

/// Permission answers already collected, so [`readiness_snapshot`] can hand
/// [`DictationActivation`] a [`PlatformReadiness`] without re-probing.
struct ProbedPermissions {
    microphone: bool,
    insertion: bool,
}

impl PlatformReadiness for ProbedPermissions {
    fn microphone_granted(&self) -> bool {
        self.microphone
    }

    fn insertion_granted(&self) -> bool {
        self.insertion
    }
}

/// The required items of a report that are not ready. Written once here so
/// the notification path and the diagnostic path cannot disagree about what
/// "missing" means.
pub fn missing_required_items(report: &SettingsReadinessReport) -> Vec<ReadinessItem> {
    report
        .items
        .iter()
        .filter(|item| item.required && !item.ready)
        .cloned()
        .collect()
}

/// The user input that starts one dictation. Voice Activation does not need a
/// configured hotkey; every other readiness requirement is shared.
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

/// Dictation Readiness (ADR-0013): dictation is only available once microphone
/// permission, text insertion permission, a configured hotkey, the assets for
/// the engine that will run, and a Transcription Engine that can actually run
/// are all ready.
///
/// The engine check is separate from the model check on purpose. A downloaded
/// model says only that the weights are on disk; whether anything in *this
/// binary* can decode them is a fact about the build, and a build compiled
/// without `local-whisper-runtime` has the file and no runtime (slugtale-bre).
pub fn dictation_ready(
    settings: &Settings,
    platform: &dyn PlatformReadiness,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    dictation_ready_checked(
        settings,
        platform.microphone_granted(),
        platform.insertion_granted(),
        local_model_ready,
        engines,
    )
}

/// [`dictation_ready`] with the external permission answers already collected,
/// so one activation can probe each OS permission exactly once and share the
/// results (slugtale-g1o.6).
pub fn dictation_ready_checked(
    settings: &Settings,
    microphone_granted: bool,
    insertion_granted: bool,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    dictation_ready_checked_for_input(
        settings,
        microphone_granted,
        insertion_granted,
        local_model_ready,
        engines,
        DictationInput::Hotkey,
    )
}

fn dictation_ready_checked_for_input(
    settings: &Settings,
    microphone_granted: bool,
    insertion_granted: bool,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
    input: DictationInput,
) -> bool {
    (!input.hotkey_required() || settings.hotkey.is_some())
        && microphone_granted
        && insertion_granted
        && (local_model_ready || !whisper_model_is_required(settings, engines))
        && engine_that_can_run(settings.primary_engine, engines).is_some()
}

/// Which engine a dictation started right now would actually be transcribed by,
/// falling back to the user's choice when nothing can run so the report still
/// talks about the engine they picked.
fn engine_in_play(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> TranscriptionEngine {
    engine_that_can_run(settings.primary_engine, engines).unwrap_or(settings.primary_engine)
}

/// Whether the Whisper ggml file on disk gates dictation on this machine.
///
/// "Local model" meant one thing when Whisper was the only engine. Now that
/// engines are plural it means *the assets for the engine that will actually
/// run*, and every other engine already reports its own assets through
/// [`EngineAvailability`] — Apple SpeechTranscriber's are system-managed and
/// Parakeet's are installed from Settings. So the Whisper download is required
/// only when Whisper is the engine in play, and a Parakeet-primary machine is
/// no longer blocked on a file it will never open (slugtale-y4m).
fn whisper_model_is_required(
    settings: &Settings,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> bool {
    engine_in_play(settings, engines) == TranscriptionEngine::Whisper
}

/// One of the facts Dictation Readiness (CONTEXT.md) is built from.
///
/// The name is the wire string: it is what the settings window keys its
/// checklist, its banner and its pane badges on, and what the Local Diagnostic
/// Log names an unmet item by. Typing it once here means a rename is a change in
/// one place, and the seam test in `tests/frontend-seam.test.mjs` fails if the
/// settings window's list of ids does not move with it.
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

    /// What the settings window calls this item. Stated here so the settings
    /// window's own fallback copy and this report cannot drift apart.
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

    /// The settings pane whose control settles this item, when settling it is a
    /// matter of changing a setting rather than of granting an OS permission.
    /// `None` means there is no such pane: the two permissions are answered in
    /// system settings, which the report names as a command instead.
    pub fn pane(self) -> Option<ReadinessPane> {
        match self {
            // Engine choice lives on the Dictation pane, next to the engine list
            // that states why each one is unavailable.
            ReadinessItemId::Hotkey | ReadinessItemId::TranscriptionEngine => {
                Some(ReadinessPane::Dictation)
            }
            ReadinessItemId::LocalModel => Some(ReadinessPane::Model),
            ReadinessItemId::LaunchAtLogin => Some(ReadinessPane::General),
            ReadinessItemId::Microphone | ReadinessItemId::TextInsertion => None,
        }
    }
}

/// One of the settings window's panes, by the id the window routes on. A subset
/// of the window's sections: the panes a readiness item can send the user to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessPane {
    Dictation,
    Model,
    General,
}

impl ReadinessPane {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadinessPane::Dictation => "dictation",
            ReadinessPane::Model => "model",
            ReadinessPane::General => "general",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessItem {
    pub id: ReadinessItemId,
    /// Derived from `id` and never set independently, so an item cannot claim a
    /// name the backend does not know or a pane that does not exist.
    pub label: String,
    pub ready: bool,
    pub required: bool,
    /// The settings pane that settles this item, when it has one. `None` for the
    /// two OS permissions, which are settled in system settings.
    pub pane: Option<ReadinessPane>,
    /// Why this item is not ready, when the reason is specific to this machine
    /// or this build rather than fixed guidance the settings window already
    /// knows. `None` means the static copy for `id` is the whole story.
    pub detail: Option<String>,
}

impl ReadinessItem {
    pub fn ready(id: ReadinessItemId, required: bool) -> Self {
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

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    /// This item, if the report carries one with this id.
    pub fn find(report: &SettingsReadinessReport, id: ReadinessItemId) -> Option<&ReadinessItem> {
        report.items.iter().find(|item| item.id == id)
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
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> SettingsReadinessReport {
    settings_readiness_report_checked(
        settings,
        platform.microphone_granted(),
        platform.insertion_granted(),
        local_model_ready,
        engines,
    )
}

/// [`settings_readiness_report`] with the external permission answers already
/// collected (slugtale-g1o.6).
pub fn settings_readiness_report_checked(
    settings: &Settings,
    microphone_granted: bool,
    insertion_granted: bool,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
) -> SettingsReadinessReport {
    settings_readiness_report_checked_for_input(
        settings,
        microphone_granted,
        insertion_granted,
        local_model_ready,
        engines,
        DictationInput::Hotkey,
    )
}

/// Build a readiness report for the activation inputs available in this app
/// build. Voice Activation can make a hotkey optional, while every other
/// readiness check stays the same.
pub fn settings_readiness_report_checked_for_input(
    settings: &Settings,
    microphone_granted: bool,
    insertion_granted: bool,
    local_model_ready: bool,
    engines: &[(TranscriptionEngine, EngineAvailability)],
    input: DictationInput,
) -> SettingsReadinessReport {
    let engine_blocker = engine_blocked_reason(settings.primary_engine, engines);
    let whisper_model_required = whisper_model_is_required(settings, engines);

    SettingsReadinessReport {
        dictation_available: dictation_ready_checked_for_input(
            settings,
            microphone_granted,
            insertion_granted,
            local_model_ready,
            engines,
            input,
        ),
        items: vec![
            readiness_item(ReadinessItemId::Microphone, true, microphone_granted),
            readiness_item(ReadinessItemId::TextInsertion, true, insertion_granted),
            readiness_item(
                ReadinessItemId::Hotkey,
                input.hotkey_required(),
                !input.hotkey_required() || settings.hotkey.is_some(),
            ),
            readiness_item(
                ReadinessItemId::LocalModel,
                whisper_model_required,
                local_model_ready,
            )
            .with_detail(if whisper_model_required {
                None
            } else {
                Some(format!(
                    "Not needed: {} transcribes without the Whisper model.",
                    engine_in_play(settings, engines).display_name()
                ))
            }),
            readiness_item(
                ReadinessItemId::TranscriptionEngine,
                true,
                engine_blocker.is_none(),
            )
            .with_detail(engine_blocker),
            readiness_item(ReadinessItemId::LaunchAtLogin, false, true),
        ],
    }
}

fn readiness_item(id: ReadinessItemId, required: bool, ready: bool) -> ReadinessItem {
    if ready {
        ReadinessItem::ready(id, required)
    } else {
        ReadinessItem::missing(id, required)
    }
}

/// One Hotkey activation's immutable view of everything outside the audio and
/// transcription engines themselves (slugtale-g1o.6).
///
/// Built once at the activation entry point: one Settings value, one external
/// probe per OS permission, one local-model answer, and the derived readiness
/// report and engine decision. Every consumer in the activation reads this
/// snapshot instead of re-reading global state, so they cannot disagree with
/// each other or with the start decision — even if the Settings File changes
/// mid-activation. It is request-scoped by construction: a later Hotkey builds
/// a fresh one and therefore sees current OS permission state, honouring
/// ADR-0013's live-readiness rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationActivation {
    pub settings: Settings,
    pub microphone_granted: bool,
    pub insertion_granted: bool,
    pub local_model_ready: bool,
    /// The engines' availability as seen when the activation started.
    pub engines: Vec<(TranscriptionEngine, EngineAvailability)>,
    /// Which engine this activation's dictations would be transcribed by.
    pub engine_in_play: Option<TranscriptionEngine>,
    pub report: SettingsReadinessReport,
}

impl DictationActivation {
    /// Probe every external fact exactly once and derive the rest. `engines`
    /// is asked of the Engine Catalogue once by the caller and shared between
    /// the readiness report and the engine decision.
    pub fn build(
        settings: Settings,
        platform: &dyn PlatformReadiness,
        local_model_ready: bool,
        engines: Vec<(TranscriptionEngine, EngineAvailability)>,
    ) -> Self {
        Self::build_for_input(
            settings,
            platform,
            local_model_ready,
            engines,
            DictationInput::Hotkey,
        )
    }

    pub fn build_for_input(
        settings: Settings,
        platform: &dyn PlatformReadiness,
        local_model_ready: bool,
        engines: Vec<(TranscriptionEngine, EngineAvailability)>,
        input: DictationInput,
    ) -> Self {
        let microphone_granted = platform.microphone_granted();
        let insertion_granted = platform.insertion_granted();
        let report = settings_readiness_report_checked_for_input(
            &settings,
            microphone_granted,
            insertion_granted,
            local_model_ready,
            &engines,
            input,
        );
        let engine_in_play = engine_that_can_run(settings.primary_engine, &engines);

        Self {
            settings,
            microphone_granted,
            insertion_granted,
            local_model_ready,
            engine_in_play,
            engines,
            report,
        }
    }

    pub fn dictation_available(&self) -> bool {
        self.report.dictation_available
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A probe source that counts how often each fact is asked for, so tests
    /// can hold snapshots to the "probe exactly once" contract.
    struct CountingProbes {
        settings: Settings,
        microphone: bool,
        insertion: bool,
        model: Option<LocalModelRef>,
        settings_loads: RefCell<usize>,
        mic_probes: RefCell<usize>,
        insertion_probes: RefCell<usize>,
        engine_probes: RefCell<usize>,
    }

    impl CountingProbes {
        fn all_ready(settings: Settings) -> Self {
            Self {
                settings,
                microphone: true,
                insertion: true,
                model: Some(an_existing_path()),
                settings_loads: RefCell::new(0),
                mic_probes: RefCell::new(0),
                insertion_probes: RefCell::new(0),
                engine_probes: RefCell::new(0),
            }
        }
    }

    /// The model probe only ever asks `is_present`, so pointing it at the
    /// platform temp directory answers true without the test writing a file or
    /// leaking one.
    fn an_existing_path() -> LocalModelRef {
        LocalModelRef::at(std::env::temp_dir())
    }

    impl ReadinessProbes for CountingProbes {
        fn settings(&self) -> Settings {
            *self.settings_loads.borrow_mut() += 1;
            self.settings.clone()
        }

        fn microphone_granted(&self) -> bool {
            *self.mic_probes.borrow_mut() += 1;
            self.microphone
        }

        fn insertion_granted(&self) -> bool {
            *self.insertion_probes.borrow_mut() += 1;
            self.insertion
        }

        fn local_model(&self, _settings: &Settings) -> Option<LocalModelRef> {
            self.model.clone()
        }

        fn engine_availability(
            &self,
            _settings: &Settings,
        ) -> Vec<(TranscriptionEngine, EngineAvailability)> {
            *self.engine_probes.borrow_mut() += 1;
            whisper_available()
        }
    }

    #[test]
    fn every_readiness_id_still_travels_to_the_settings_window_under_its_old_name() {
        // The settings window keys four separate tables off these names. Changing
        // one here is safe for the compiler and invisible everywhere else, so the
        // old names are pinned: a rename has to be a deliberate change to both
        // sides at once, and tests/frontend-seam.test.mjs is what enforces that.
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
    fn an_items_name_and_pane_come_from_its_id_and_cannot_disagree_with_it() {
        // A report is built by naming ids and nothing else, so an item cannot
        // claim a name the backend does not know or point the settings window at
        // a pane that does not exist.
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_available(),
        );

        for item in &report.items {
            assert_eq!(item.label, item.id.label());
            assert_eq!(item.pane, item.id.pane());
        }
    }

    #[test]
    fn the_two_os_permissions_name_no_pane_because_no_setting_settles_them() {
        assert_eq!(ReadinessItemId::Microphone.pane(), None);
        assert_eq!(ReadinessItemId::TextInsertion.pane(), None);
    }

    #[test]
    fn an_item_is_found_by_its_id_rather_than_by_matching_a_string() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        );

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
        let probes = CountingProbes::all_ready(configured_settings());

        let snapshot = readiness_snapshot(&probes, |_| DictationInput::Hotkey);

        assert!(snapshot.report.dictation_available);
        assert_eq!(*probes.settings_loads.borrow(), 1);
        assert_eq!(*probes.mic_probes.borrow(), 1);
        assert_eq!(*probes.insertion_probes.borrow(), 1);
        assert_eq!(*probes.engine_probes.borrow(), 1);
    }

    #[test]
    fn the_snapshot_and_the_checked_report_answer_alike() {
        let settings = configured_settings();
        let direct = settings_readiness_report_checked_for_input(
            &settings,
            true,
            true,
            true,
            &whisper_available(),
            DictationInput::Hotkey,
        );
        let probes = CountingProbes::all_ready(settings);

        assert_eq!(
            readiness_snapshot(&probes, |_| DictationInput::Hotkey).report,
            direct
        );
    }

    #[test]
    fn missing_required_items_lists_only_unmet_requirements() {
        let mut report = settings_readiness_report_checked_for_input(
            &configured_settings(),
            false, // microphone missing and required
            true,
            false, // local model missing; required for Whisper
            &whisper_available(),
            DictationInput::Hotkey,
        );
        // launch_at_login is not ready=false here by default; force an optional
        // item to be unready so the filter must skip it.
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
    fn dictation_is_not_ready_when_nothing_is_ready() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        assert!(!dictation_ready(
            &Settings::default(),
            &platform,
            false,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_microphone_permission() {
        let platform = FakePlatform {
            microphone: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(
            &configured_settings(),
            &platform,
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_insertion_permission() {
        let platform = FakePlatform {
            insertion: false,
            ..FakePlatform::all_ready()
        };
        assert!(!dictation_ready(
            &configured_settings(),
            &platform,
            true,
            &whisper_available()
        ));
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
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_not_ready_without_local_model() {
        assert!(!dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_available()
        ));
    }
    #[test]
    fn dictation_is_ready_when_all_requirements_are_met() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available()
        ));
    }
    #[test]
    fn settings_readiness_report_shows_missing_required_items() {
        let platform = FakePlatform {
            microphone: false,
            insertion: false,
        };
        let report = settings_readiness_report(
            &Settings::default(),
            &platform,
            false,
            &whisper_runtime_not_built(),
        );

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
    fn settings_readiness_report_allows_dictation_when_required_items_are_ready() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        );

        assert!(report.dictation_available);
        assert!(report
            .items
            .iter()
            .filter(|item| item.required)
            .all(|item| item.ready));
    }
    #[test]
    fn model_readiness_is_supplied_outside_the_platform_adapter() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_available(),
        );
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

    /// The production wiring's own Local Model answer, over a real model
    /// directory: the probe asks the engine catalogue, exactly as
    /// `AppReadinessProbes` does in the Tauri tier.
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

    /// A readiness report built the way the app builds one: from Settings and a
    /// model directory, with the probe reading the model through the engine
    /// catalogue rather than computing the answer itself.
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
        // Settings name no model, so the managed default in the model directory
        // is the file a dictation would open.
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
        // The defect: readiness looked only at the default path, so a user who
        // chose a custom model in Settings was told dictation could not start
        // and warm-up was suppressed, while the engine reported that very model
        // available and ready to decode.
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
        // The Settings File still names a model the user has since deleted, and
        // the engine resolves that same stale path. Falling back to the default
        // here is what let the report say "ready" about a file the engine would
        // never open.
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

    /// Every combination of a Settings override and a model directory's
    /// contents. The report's Local Model item and the Whisper engine must be
    /// talking about one file: whatever the Settings name is what both open.
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
                // A build without the Whisper runtime cannot decode the file
                // whatever is on disk; that half is the report's own
                // transcription_engine item, not the Local Model one.
                assert_eq!(
                    availability.is_available(),
                    present && cfg!(feature = "local-whisper-runtime"),
                    "settings.model={settings_model:?} default_present={default_present}"
                );

                std::fs::remove_dir_all(&model_dir).ok();
            }
        }
    }

    #[test]
    fn dictation_is_not_ready_when_no_engine_can_run() {
        // slugtale-bre: a default-feature build compiles no Whisper runtime. The
        // model file on disk says nothing about whether anything can decode it,
        // so readiness must not be satisfied by the download alone.
        assert!(!dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_runtime_not_built(),
        ));
    }

    #[test]
    fn dictation_is_ready_on_a_whisper_only_build_with_the_model_downloaded() {
        assert!(dictation_ready(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        ));
    }

    #[test]
    fn a_build_without_the_whisper_runtime_reports_why_rather_than_ready() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_runtime_not_built(),
        );
        let engine = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::TranscriptionEngine)
            .unwrap();

        assert!(!report.dictation_available);
        assert!(!engine.ready);
        assert!(engine.required);
        // The user is told what is actually wrong with the binary, not sent to
        // re-download a model they already have.
        assert_eq!(
            engine.detail.as_deref(),
            Some(
                "Whisper base.en cannot run: this build was compiled without support for this engine"
            )
        );
    }

    #[test]
    fn a_whisper_only_build_that_can_transcribe_reports_no_engine_blocker() {
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            true,
            &whisper_available(),
        );
        let engine = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::TranscriptionEngine)
            .unwrap();

        assert!(report.dictation_available);
        assert_eq!(
            engine,
            &ReadinessItem::ready(ReadinessItemId::TranscriptionEngine, true)
        );
    }

    #[test]
    fn a_machine_whose_engine_needs_no_whisper_model_is_ready_without_one() {
        // slugtale-y4m: Parakeet decodes its own installed assets, so blocking
        // dictation on a 148 MB Whisper download the user will never open is
        // over-blocking, not safety.
        assert!(dictation_ready(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &parakeet_available(),
        ));
    }

    #[test]
    fn the_local_model_is_optional_and_says_why_when_another_engine_runs() {
        let report = settings_readiness_report(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &parakeet_available(),
        );
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
        // The chosen engine lost its assets, so the router falls back to Whisper
        // — which means the Whisper download is once more the thing standing
        // between this machine and a transcription.
        let engines = [
            (
                crate::TranscriptionEngine::Parakeet,
                crate::EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                    detail: "Parakeet assets are not installed.".to_string(),
                }),
            ),
            (
                crate::TranscriptionEngine::Whisper,
                crate::EngineAvailability::Available,
            ),
        ];
        let report = settings_readiness_report(
            &parakeet_settings(),
            &FakePlatform::all_ready(),
            false,
            &engines,
        );
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
        // Nothing runs, so there is no engine in play to defer to; the user
        // chose Whisper, so the report keeps describing Whisper's requirements.
        let report = settings_readiness_report(
            &configured_settings(),
            &FakePlatform::all_ready(),
            false,
            &whisper_runtime_not_built(),
        );
        let local_model = report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::LocalModel)
            .unwrap();

        assert!(!report.dictation_available);
        assert!(local_model.required);
    }

    fn parakeet_settings() -> Settings {
        Settings {
            hotkey: Some("cmd+shift+d".to_string()),
            primary_engine: crate::TranscriptionEngine::Parakeet,
            ..Settings::default()
        }
    }

    fn parakeet_available() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![
            (
                crate::TranscriptionEngine::Whisper,
                crate::EngineAvailability::Unavailable(crate::EngineUnavailable::AssetsMissing {
                    detail: "The Whisper model has not been downloaded yet.".to_string(),
                }),
            ),
            (
                crate::TranscriptionEngine::Parakeet,
                crate::EngineAvailability::Available,
            ),
        ]
    }

    fn whisper_available() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![(
            crate::TranscriptionEngine::Whisper,
            crate::EngineAvailability::Available,
        )]
    }

    fn whisper_runtime_not_built() -> Vec<(crate::TranscriptionEngine, crate::EngineAvailability)> {
        vec![(
            crate::TranscriptionEngine::Whisper,
            crate::EngineAvailability::Unavailable(crate::EngineUnavailable::RuntimeNotBuilt),
        )]
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

    /// A platform fake that counts its external probes, so tests can prove
    /// one activation queries each permission exactly once.
    struct CountingPlatform {
        inner: FakePlatform,
        microphone_calls: std::cell::Cell<usize>,
        insertion_calls: std::cell::Cell<usize>,
    }

    impl CountingPlatform {
        fn all_ready() -> Self {
            Self {
                inner: FakePlatform::all_ready(),
                microphone_calls: std::cell::Cell::new(0),
                insertion_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl PlatformReadiness for CountingPlatform {
        fn microphone_granted(&self) -> bool {
            self.microphone_calls.set(self.microphone_calls.get() + 1);
            self.inner.microphone
        }
        fn insertion_granted(&self) -> bool {
            self.insertion_calls.set(self.insertion_calls.get() + 1);
            self.inner.insertion
        }
    }

    #[test]
    fn one_activation_probes_each_os_permission_exactly_once() {
        let platform = CountingPlatform::all_ready();

        let activation =
            DictationActivation::build(configured_settings(), &platform, true, whisper_available());

        assert!(activation.dictation_available());
        assert_eq!(platform.microphone_calls.get(), 1);
        assert_eq!(platform.insertion_calls.get(), 1);
    }

    #[test]
    fn a_permission_denial_fails_the_activation_and_names_the_missing_item() {
        // This is the fact the Settings-window fallback is driven from: the
        // report must list the denied permission as a missing required item.
        let platform = CountingPlatform {
            inner: FakePlatform {
                microphone: false,
                insertion: true,
            },
            microphone_calls: std::cell::Cell::new(0),
            insertion_calls: std::cell::Cell::new(0),
        };

        let activation =
            DictationActivation::build(configured_settings(), &platform, true, whisper_available());

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
        assert_eq!(platform.microphone_calls.get(), 1);
    }

    #[test]
    fn every_consumer_sees_one_consistent_snapshot_even_if_settings_change_midway() {
        let platform = CountingPlatform::all_ready();
        let settings = configured_settings();
        let engines = whisper_available();

        let activation = DictationActivation::build(settings.clone(), &platform, true, engines);

        // A later Settings save lands in storage; the in-flight activation was
        // built from the value it captured and must not shift under it.
        let mut changed = settings.clone();
        changed.hotkey = None;

        assert_eq!(activation.settings.hotkey, settings.hotkey);
        assert_ne!(activation.settings.hotkey, changed.hotkey);
        assert!(activation.dictation_available());
    }

    #[test]
    fn voice_activation_is_an_input_when_no_hotkey_is_configured() {
        let platform = CountingPlatform::all_ready();
        let settings = Settings::default();

        let activation = DictationActivation::build_for_input(
            settings,
            &platform,
            true,
            whisper_available(),
            DictationInput::VoiceActivation,
        );

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
    fn the_engine_decision_is_resolved_once_from_the_shared_availability() {
        let platform = CountingPlatform::all_ready();
        let mut settings = configured_settings();
        settings.primary_engine = crate::TranscriptionEngine::Parakeet;
        // Parakeet cannot run in this build; the decision must fall back.

        let activation =
            DictationActivation::build(settings.clone(), &platform, true, whisper_available());

        assert_eq!(
            activation.engine_in_play,
            Some(crate::TranscriptionEngine::Whisper)
        );
        // The report's engine item agrees with the snapshot's own decision.
        let engine_item = activation
            .report
            .items
            .iter()
            .find(|item| item.id == ReadinessItemId::TranscriptionEngine)
            .unwrap();
        assert!(engine_item.ready);
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
