
use crate::{
    ActivationMode, DictationActivation, DictationEvent, DictationKey, DictationLifecycle,
    HotkeyInput,
};

pub fn parse_dictation_ui_event(name: &str) -> Result<DictationEvent, String> {
    match name {
        "start" => Ok(DictationEvent::Start),
        "stop" => Ok(DictationEvent::Stop),
        "cancel" => Ok(DictationEvent::Cancel),
        other => Err(format!("unknown dictation event: {other}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginSkip {
    NotReady,
    AlreadyDictating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalKey {
    escape: bool,
    modifiers: bool,
}

impl GlobalKey {
    pub fn new(escape: bool, modifiers: bool) -> Self {
        Self { escape, modifiers }
    }
}

pub fn dictation_key_for(key: GlobalKey) -> DictationKey {
    match (key.escape, key.modifiers) {
        (true, false) => DictationKey::Escape,
        _ => DictationKey::Hotkey,
    }
}

pub struct DictationControl {
    lifecycle: Option<DictationLifecycle>,
}

impl Default for DictationControl {
    fn default() -> Self {
        Self { lifecycle: None }
    }
}

impl DictationControl {
    pub fn new(mode: ActivationMode) -> Self {
        Self {
            lifecycle: Some(DictationLifecycle::new(mode)),
        }
    }

    pub fn is_dictating(&self) -> bool {
        self.lifecycle
            .as_ref()
            .map(DictationLifecycle::is_dictating)
            .unwrap_or(false)
    }

    pub fn begin(&mut self, dictation_available: bool) -> Result<DictationEvent, BeginSkip> {
        if !dictation_available {
            return Err(BeginSkip::NotReady);
        }
        self.lifecycle
            .as_mut()
            .and_then(DictationLifecycle::start)
            .ok_or(BeginSkip::AlreadyDictating)
    }

    pub fn abandon_begin(&mut self) {
        if let Some(lifecycle) = self.lifecycle.as_mut() {
            let _ = lifecycle.stop();
        }
    }

    pub fn on_hotkey(&mut self, input: HotkeyInput) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.on_hotkey(input)
    }

    pub fn on_key(&mut self, key: DictationKey, input: HotkeyInput) -> Option<DictationEvent> {
        match (key, input) {
            (DictationKey::Hotkey, input) => self.on_hotkey(input),
            (DictationKey::Escape, HotkeyInput::Pressed) => self.cancel(),
            (DictationKey::Escape, HotkeyInput::Released) => None,
        }
    }

    pub fn cancel(&mut self) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.cancel()
    }

    pub fn stop(&mut self) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.stop()
    }
}

/// The access is short-lived on purpose. The control is shared with the key
/// worker's other transitions and with the Dictation Bar's Stop and Cancel, so
/// the sequence must never hold it across an effect that arms a key or starts a
/// recording — a blocked recording would otherwise stall every other input.
pub trait BeginHost {
    fn typing_challenge_is_open(&self) -> bool;

    fn readiness_snapshot(&mut self) -> DictationActivation;

    /// Run `step` with the shared control, releasing it before returning.
    /// `None` when the host could not reach its control at all.
    fn with_control<R>(&mut self, step: &mut dyn FnMut(&mut DictationControl) -> R) -> Option<R>;

    fn set_escape(&mut self, armed: bool) -> Result<(), String>;

    fn start(
        &mut self,
        event: DictationEvent,
        activation: DictationActivation,
    ) -> Result<(), String>;
}

pub fn begin_activation<H: BeginHost>(host: &mut H) -> Result<(), String> {
    if host.typing_challenge_is_open() {
        return Ok(());
    }

    let activation = host.readiness_snapshot();
    let event =
        match host.with_control(&mut |control| control.begin(activation.dictation_available())) {
            Some(Ok(event)) => event,
            Some(Err(_skip)) => return Ok(()),
            None => return Err("the dictation control is unavailable".to_string()),
        };

    if let Err(error) = host.set_escape(true) {
        host.with_control(&mut |control| control.abandon_begin());
        return Err(format!("global Escape could not be registered: {error}"));
    }

    if let Err(error) = host.start(event, activation) {
        host.with_control(&mut |control| control.abandon_begin());
        let _ = host.set_escape(false);
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActivationMode, HotkeyInput};

    fn control(mode: ActivationMode) -> DictationControl {
        DictationControl::new(mode)
    }

    #[test]
    fn a_begin_that_fails_readiness_moves_nothing() {
        let mut control = control(ActivationMode::Toggle);

        assert_eq!(control.begin(false), Err(BeginSkip::NotReady));
        assert!(!control.is_dictating());
    }

    #[test]
    fn a_begin_starts_an_idle_dictation_and_a_second_one_is_already_dictating() {
        let mut control = control(ActivationMode::Toggle);

        assert_eq!(control.begin(true), Ok(DictationEvent::Start));
        assert!(control.is_dictating());

        assert_eq!(control.begin(true), Err(BeginSkip::AlreadyDictating));
    }

    #[test]
    fn an_abandoned_begin_leaves_the_next_activation_free_to_start() {
        let mut control = control(ActivationMode::Toggle);

        control.begin(true).unwrap();
        control.abandon_begin();

        assert!(
            !control.is_dictating(),
            "a discarded dictation must not stay marked active"
        );
        assert_eq!(control.begin(true), Ok(DictationEvent::Start));
    }

    #[test]
    fn a_toggle_hotkey_stops_and_the_next_press_begins_again() {
        let mut control = control(ActivationMode::Toggle);
        control.begin(true).unwrap();

        assert_eq!(
            control.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Stop)
        );
        assert!(!control.is_dictating());
        assert_eq!(control.begin(true), Ok(DictationEvent::Start));
    }

    #[test]
    fn a_hold_release_after_an_abandoned_begin_does_not_stop_anything_twice() {
        let mut control = control(ActivationMode::Hold);
        control.begin(true).unwrap();
        control.abandon_begin();

        assert_eq!(control.on_hotkey(HotkeyInput::Released), None);
        assert!(!control.is_dictating());
    }

    #[test]
    fn dictation_ui_events_parse_to_lifecycle_events() {
        assert_eq!(
            parse_dictation_ui_event("start"),
            Ok(DictationEvent::Start)
        );
        assert_eq!(parse_dictation_ui_event("stop"), Ok(DictationEvent::Stop));
        assert_eq!(
            parse_dictation_ui_event("cancel"),
            Ok(DictationEvent::Cancel)
        );
        assert!(parse_dictation_ui_event("pause").is_err());
    }

    #[test]
    fn a_bare_escape_is_the_dictation_escape_key_and_a_modified_one_is_not() {
        assert_eq!(
            dictation_key_for(GlobalKey::new(true, false)),
            DictationKey::Escape
        );
        assert_eq!(
            dictation_key_for(GlobalKey::new(true, true)),
            DictationKey::Hotkey,
            "a modified Escape is the application's own shortcut, not Slugtale's"
        );
        assert_eq!(
            dictation_key_for(GlobalKey::new(false, false)),
            DictationKey::Hotkey
        );
    }

    #[test]
    fn an_escape_press_cancels_and_its_release_means_nothing() {
        let mut control = control(ActivationMode::Hold);
        control.begin(true).unwrap();

        assert_eq!(
            control.on_key(DictationKey::Escape, HotkeyInput::Pressed),
            Some(DictationEvent::Cancel)
        );
        assert!(!control.is_dictating());
        assert_eq!(
            control.on_key(DictationKey::Escape, HotkeyInput::Released),
            None,
            "an Escape release is not a hold-to-dictate ending"
        );
    }

    #[test]
    fn a_hotkey_pressed_while_idle_asks_the_host_to_begin() {
        let mut control = control(ActivationMode::Toggle);

        assert_eq!(
            control.on_key(DictationKey::Hotkey, HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
    }

    #[derive(Debug, PartialEq)]
    enum Effect {
        Escape { armed: bool, dictating: bool },
        Start { dictating: bool },
    }

    #[derive(Default)]
    struct ScriptedHost {
        control: DictationControl,
        control_held: bool,
        effects: Vec<Effect>,
        started: Vec<DictationEvent>,
        snapshots_built: usize,
        typing_challenge_open: bool,
        unready: bool,
        arm_fails: bool,
        start_fails: bool,
    }

    impl BeginHost for ScriptedHost {
        fn typing_challenge_is_open(&self) -> bool {
            self.typing_challenge_open
        }

        fn readiness_snapshot(&mut self) -> DictationActivation {
            self.snapshots_built += 1;
            let mut activation = ready_activation();
            activation.report.dictation_available = !self.unready;
            activation
        }

        fn with_control<R>(
            &mut self,
            step: &mut dyn FnMut(&mut DictationControl) -> R,
        ) -> Option<R> {
            self.control_held = true;
            let result = step(&mut self.control);
            self.control_held = false;
            Some(result)
        }

        fn set_escape(&mut self, armed: bool) -> Result<(), String> {
            assert!(
                !self.control_held,
                "arming Escape must not hold the shared control"
            );
            self.effects.push(Effect::Escape {
                armed,
                dictating: self.control.is_dictating(),
            });
            if self.arm_fails {
                return Err("the OS refused the registration".to_string());
            }
            Ok(())
        }

        fn start(
            &mut self,
            event: DictationEvent,
            _activation: DictationActivation,
        ) -> Result<(), String> {
            assert!(
                !self.control_held,
                "starting the recording must not hold the shared control"
            );
            self.effects.push(Effect::Start {
                dictating: self.control.is_dictating(),
            });
            self.started.push(event);
            if self.start_fails {
                return Err("the microphone refused".to_string());
            }
            Ok(())
        }
    }

    fn host() -> ScriptedHost {
        ScriptedHost {
            control: control(ActivationMode::Toggle),
            ..ScriptedHost::default()
        }
    }

    fn ready_activation() -> DictationActivation {
        DictationActivation {
            settings: crate::Settings::default(),
            report: crate::SettingsReadinessReport {
                dictation_available: true,
                items: Vec::new(),
            },
        }
    }

    #[test]
    fn a_successful_begin_moves_the_lifecycle_then_arms_escape_then_starts_the_recording() {
        let mut host = host();

        begin_activation(&mut host).unwrap();

        assert_eq!(
            host.effects,
            [
                Effect::Escape {
                    armed: true,
                    dictating: true
                },
                Effect::Start { dictating: true },
            ]
        );
        assert_eq!(host.started, [DictationEvent::Start]);
        assert!(host.control.is_dictating());
    }

    #[test]
    fn an_open_typing_challenge_stops_the_begin_before_any_readiness_snapshot() {
        let mut host = host();
        host.typing_challenge_open = true;

        begin_activation(&mut host).unwrap();

        assert_eq!(host.snapshots_built, 0);
        assert!(host.effects.is_empty());
        assert!(!host.control.is_dictating());
    }

    #[test]
    fn an_unready_begin_starts_nothing_and_arms_nothing() {
        let mut host = host();
        host.unready = true;

        begin_activation(&mut host).unwrap();

        assert!(host.started.is_empty());
        assert!(host.effects.is_empty());
        assert!(!host.control.is_dictating());
    }

    #[test]
    fn a_second_begin_while_dictating_starts_nothing() {
        let mut host = host();
        begin_activation(&mut host).unwrap();

        begin_activation(&mut host).unwrap();

        assert_eq!(host.started, [DictationEvent::Start]);
        assert!(host.control.is_dictating());
    }

    #[test]
    fn a_recording_that_fails_to_start_rolls_the_begin_back_and_disarms_escape() {
        let mut host = host();
        host.start_fails = true;

        begin_activation(&mut host).unwrap_err();

        assert_eq!(
            host.effects,
            [
                Effect::Escape {
                    armed: true,
                    dictating: true
                },
                Effect::Start { dictating: true },
                Effect::Escape {
                    armed: false,
                    dictating: false
                },
            ],
            "the begin is abandoned before the armed Escape is disarmed as the rollback's last step"
        );
        assert_eq!(host.started, [DictationEvent::Start]);
        assert!(
            !host.control.is_dictating(),
            "a discarded dictation must not stay marked active"
        );
        assert_eq!(
            host.control.begin(true),
            Ok(DictationEvent::Start),
            "the next activation is free to start"
        );
    }

    #[test]
    fn an_escape_that_cannot_be_armed_rolls_the_begin_back_before_recording() {
        let mut host = host();
        host.arm_fails = true;

        let error = begin_activation(&mut host).unwrap_err();

        assert!(
            error.contains("global Escape could not be registered"),
            "{error}"
        );
        assert!(
            host.started.is_empty(),
            "the recording must not start without a cancellable dictation"
        );
        assert!(!host.control.is_dictating());
        assert_eq!(
            host.control.begin(true),
            Ok(DictationEvent::Start),
            "the next activation is free to start"
        );
    }
}
