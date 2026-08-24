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

use crate::{ActivationMode, DictationEvent, DictationLifecycle};

/// Why a begin request left the dictation idle. The host has usually already
/// told the user why — these are for the host's own tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginSkip {
    /// Dictation Readiness failed. The host builds the report and shows it.
    NotReady,
    /// A dictation is already active, so this input changes nothing.
    AlreadyDictating,
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
    pub fn on_hotkey(&mut self, input: crate::HotkeyInput) -> Option<DictationEvent> {
        self.lifecycle.as_mut()?.on_hotkey(input)
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
}
