//! Dictation Control (CONTEXT.md): the activation policy every way of starting
//! a dictation shares — Hotkey press, Voice Activation wake phrase, and the
//! Dictation Bar controls.
//!
//! This module owns the decisions and only the decisions: whether a begin
//! request may start, which lifecycle transition it means, and how to undo one
//! whose host steps failed. Its host (the Tauri tier) owns the effects —
//! readiness probes, global-Escape arming, audio capture — and must run them
//! in the order this module implies: transition, arm Escape, record. Any
//! failure undoes in reverse through [`DictationControl::abandon_begin`].

use crate::{
    ActivationMode, DictationActivation, DictationEvent, DictationKey, DictationLifecycle,
    HotkeyInput,
};

/// Parse a Dictation Bar / frontend lifecycle string into a [`DictationEvent`].
pub fn parse_dictation_ui_event(name: &str) -> Result<DictationEvent, String> {
    match name {
        "start" => Ok(DictationEvent::Start),
        "stop" => Ok(DictationEvent::Stop),
        "cancel" => Ok(DictationEvent::Cancel),
        other => Err(format!("unknown dictation event: {other}")),
    }
}

/// Why a begin request left the dictation idle. The host has usually already
/// told the user why — these are for the host's own tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginSkip {
    /// Dictation Readiness failed. The host builds the report and shows it.
    NotReady,
    /// A dictation is already active, so this input changes nothing.
    AlreadyDictating,
}

/// A globally observed key, as the OS adapter reports it: which key it was and
/// whether any modifier was held with it. The adapter translates the shortcut
/// it was handed into these two facts and forwards them; which dictation key
/// they mean is decided here, next to the transitions that act on the answer.
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

/// Which dictation key a globally observed key means.
///
/// Escape is the only key besides the configured hotkey that Slugtale ever
/// registers globally, and only while it is bare: with a modifier held the user
/// is typing that application's own shortcut, so it is not Slugtale's to act
/// on. Stated here rather than in the plugin adapter so the rule is tested
/// where the transitions that act on it are.
pub fn dictation_key_for(key: GlobalKey) -> DictationKey {
    match (key.escape, key.modifiers) {
        (true, false) => DictationKey::Escape,
        _ => DictationKey::Hotkey,
    }
}

/// One resident dictation's lifecycle plus its begin policy. Lives in the
/// hotkey registration state so every activation input drives the same one.
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

    /// Decide a begin request: the readiness and idleness guards run here, in
    /// that order, and only a request that passes both moves the lifecycle.
    /// The Typing Challenge guard is not one of them: it must run before any
    /// readiness snapshot is paid for, so it stays the host's job. Returns the
    /// event the host must now carry out — arming Escape, then starting the
    /// recording — or why nothing will happen.
    pub fn begin(&mut self, dictation_available: bool) -> Result<DictationEvent, BeginSkip> {
        if !dictation_available {
            return Err(BeginSkip::NotReady);
        }
        self.lifecycle
            .as_mut()
            .and_then(DictationLifecycle::start)
            .ok_or(BeginSkip::AlreadyDictating)
    }

    /// Undo a begun activation whose host steps failed — Escape could not be
    /// armed, or the recording refused to start. Without this, toggle/hold
    /// state would believe a discarded dictation is still active and the next
    /// activation would be silently ignored.
    pub fn abandon_begin(&mut self) {
        if let Some(lifecycle) = self.lifecycle.as_mut() {
            let _ = lifecycle.stop();
        }
    }

    /// The ordinary hotkey transitions: hold-to-dictate release, toggle flip.
    pub fn on_hotkey(&mut self, input: HotkeyInput) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.on_hotkey(input)
    }

    /// One key transition from any activation input: the whole transition
    /// table, in one place, so the hotkey worker, the Dictation Bar and Escape
    /// cannot each grow their own reading of it.
    ///
    /// Every transition of the configured hotkey is the lifecycle's own
    /// hold/toggle behaviour. A bare Escape abandons the active dictation on
    /// its press edge, and its release means nothing — it is not the hotkey,
    /// so a release must not read as a hold-to-dictate ending.
    pub fn on_key(&mut self, key: DictationKey, input: HotkeyInput) -> Option<DictationEvent> {
        match (key, input) {
            (DictationKey::Hotkey, input) => self.on_hotkey(input),
            (DictationKey::Escape, HotkeyInput::Pressed) => self.cancel(),
            (DictationKey::Escape, HotkeyInput::Released) => None,
        }
    }

    /// A bare Escape press discards the active dictation.
    pub fn cancel(&mut self) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.cancel()
    }

    /// The user asked to stop; finish the dictation normally.
    pub fn stop(&mut self) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.stop()
    }
}

/// The host a begin sequence needs: short-lived access to the shared control,
/// plus the two effects the sequence runs between the transitions it makes.
///
/// The access is short-lived on purpose. The control is shared with the key
/// worker's other transitions and with the Dictation Bar's Stop and Cancel, so
/// the sequence must never hold it across an effect that arms a key or starts a
/// recording — a blocked recording would otherwise stall every other input.
pub trait BeginHost {
    /// Run `step` with the shared control, releasing it before returning.
    /// `None` when the host could not reach its control at all.
    fn with_control<R>(&mut self, step: &mut dyn FnMut(&mut DictationControl) -> R) -> Option<R>;

    /// Arm (`true`) or disarm (`false`) bare Escape at the OS, so an active
    /// dictation is never left uncancellable.
    fn set_escape(&mut self, armed: bool) -> Result<(), String>;

    /// Start the recording the transition just decided on.
    fn start(
        &mut self,
        event: DictationEvent,
        activation: DictationActivation,
    ) -> Result<(), String>;
}

/// Carry out one begin request the host has already settled the readiness of.
///
/// The order and the rollback live here, not at each caller: transition, arm
/// Escape, then record. Every caller ran a private copy of that dance before,
/// and the copies had already drifted on the rollback and on the Typing
/// Challenge guard. A request that fails readiness or arrives while a dictation
/// is already active moves nothing and reports nothing — the host has already
/// told the user why. A step that fails is undone in reverse: the recording is
/// rolled back, Escape is disarmed, and the lifecycle is abandoned, so a
/// discarded activation cannot leave toggle/hold state believing a dictation is
/// still running.
pub fn begin_activation<H: BeginHost>(
    host: &mut H,
    activation: DictationActivation,
) -> Result<(), String> {
    let event =
        match host.with_control(&mut |control| control.begin(activation.dictation_available())) {
            Some(Ok(event)) => event,
            Some(Err(_skip)) => return Ok(()),
            None => return Err("the dictation control is unavailable".to_string()),
        };

    // Recording has not started yet; arming Escape here keeps the window where
    // the lifecycle says dictating but Escape is not global down to nothing.
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

    /// The host a begin sequence reaches: the control it is handed, a scripted
    /// Escape arm, and a scripted recording, each recorded as it ran.
    #[derive(Default)]
    struct ScriptedHost {
        control: DictationControl,
        escape_armed: Vec<bool>,
        started: Vec<DictationEvent>,
        arm_fails: bool,
        start_fails: bool,
    }

    impl BeginHost for ScriptedHost {
        fn with_control<R>(
            &mut self,
            step: &mut dyn FnMut(&mut DictationControl) -> R,
        ) -> Option<R> {
            Some(step(&mut self.control))
        }

        fn set_escape(&mut self, armed: bool) -> Result<(), String> {
            self.escape_armed.push(armed);
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
                // The item list is the readiness report's own business
                // (readiness.rs); a begin sequence only reads the answer.
                items: Vec::new(),
            },
        }
    }

    #[test]
    fn a_successful_begin_arms_escape_before_the_recording_starts() {
        let mut host = host();

        begin_activation(&mut host, ready_activation()).unwrap();

        assert_eq!(host.started, [DictationEvent::Start]);
        assert_eq!(
            host.escape_armed,
            [true],
            "Escape must be armed before the recording starts"
        );
        assert!(host.control.is_dictating());
    }

    #[test]
    fn an_unready_begin_starts_nothing_and_arms_nothing() {
        let mut host = host();
        let mut activation = ready_activation();
        activation.report.dictation_available = false;

        begin_activation(&mut host, activation).unwrap();

        assert!(host.started.is_empty());
        assert!(host.escape_armed.is_empty());
        assert!(!host.control.is_dictating());
    }

    #[test]
    fn a_second_begin_while_dictating_starts_nothing() {
        let mut host = host();
        begin_activation(&mut host, ready_activation()).unwrap();

        begin_activation(&mut host, ready_activation()).unwrap();

        assert_eq!(host.started, [DictationEvent::Start]);
        assert!(host.control.is_dictating());
    }

    #[test]
    fn a_recording_that_fails_to_start_rolls_the_begin_back_and_disarms_escape() {
        let mut host = host();
        host.start_fails = true;

        begin_activation(&mut host, ready_activation()).unwrap_err();

        assert_eq!(host.started, [DictationEvent::Start]);
        assert_eq!(
            host.escape_armed,
            [true, false],
            "the armed Escape must be disarmed as the rollback's last step"
        );
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

        let error = begin_activation(&mut host, ready_activation()).unwrap_err();

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
