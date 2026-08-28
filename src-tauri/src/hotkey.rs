use crate::ActivationMode;

/// A raw hotkey signal delivered by the OS hotkey adapter (ADR-0021). The
/// adapter only reports key transitions; interpreting them into dictation
/// lifecycle events is the job of [`DictationLifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyInput {
    Pressed,
    Released,
}

/// Which globally observed key produced a transition. The configured Hotkey
/// follows the user's hold/toggle setting; Escape always abandons an active
/// dictation on its press edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationKey {
    Hotkey,
    Escape,
}

/// An explicit dictation lifecycle event handed to the dictation pipeline.
/// `Stop` ends dictation and keeps the resulting transcription; `Cancel`
/// abandons the dictation and discards it (CONTEXT.md: Dictation Bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationEvent {
    Start,
    Stop,
    Cancel,
}

/// The configurable hotkey lifecycle (ADR-0004). Translates raw [`HotkeyInput`]
/// transitions into [`DictationEvent`]s according to the active
/// [`ActivationMode`], so the dictation pipeline receives explicit start, stop,
/// and cancel events without knowing how the hotkey behaves.
pub struct DictationLifecycle {
    mode: ActivationMode,
    dictating: bool,
    hotkey_down: bool,
}

impl DictationLifecycle {
    pub fn new(mode: ActivationMode) -> Self {
        Self {
            mode,
            dictating: false,
            hotkey_down: false,
        }
    }

    pub fn is_dictating(&self) -> bool {
        self.dictating
    }

    /// Start a dictation from a source that has no press/release edges, such as
    /// Voice Activation. The next real hotkey press remains usable because this
    /// does not leave the hotkey marked as down.
    pub fn start(&mut self) -> Option<DictationEvent> {
        if self.dictating {
            return None;
        }
        self.hotkey_down = false;
        self.dictating = true;
        Some(DictationEvent::Start)
    }

    /// Stop a dictation from a control such as the Dictation Bar.
    pub fn stop(&mut self) -> Option<DictationEvent> {
        if !self.dictating {
            return None;
        }
        self.hotkey_down = false;
        self.dictating = false;
        Some(DictationEvent::Stop)
    }

    pub fn on_hotkey(&mut self, input: HotkeyInput) -> Option<DictationEvent> {
        match input {
            HotkeyInput::Pressed if self.hotkey_down => None,
            HotkeyInput::Pressed if self.mode == ActivationMode::Toggle && self.dictating => {
                self.hotkey_down = true;
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Pressed if self.dictating => {
                // A non-key source started a Hold-mode dictation. Claim this
                // physical press so its release can stop the active dictation.
                self.hotkey_down = true;
                None
            }
            HotkeyInput::Pressed => {
                self.hotkey_down = true;
                self.dictating = true;
                Some(DictationEvent::Start)
            }
            HotkeyInput::Released if self.mode == ActivationMode::Hold && self.dictating => {
                self.hotkey_down = false;
                self.dictating = false;
                Some(DictationEvent::Stop)
            }
            HotkeyInput::Released => {
                self.hotkey_down = false;
                None
            }
        }
    }

    pub fn cancel(&mut self) -> Option<DictationEvent> {
        if self.dictating {
            self.hotkey_down = false;
            self.dictating = false;
            Some(DictationEvent::Cancel)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_mode_press_starts_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert!(lifecycle.is_dictating());
    }
    #[test]
    fn hold_mode_release_stops_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Released),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }
    #[test]
    fn hold_mode_release_while_idle_does_nothing() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert!(!lifecycle.is_dictating());
    }
    #[test]
    fn toggle_mode_second_press_stops_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }
    #[test]
    fn toggle_mode_ignores_repeated_press_until_key_release() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Start)
        );
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Pressed), None);
        assert!(lifecycle.is_dictating());
    }
    #[test]
    fn toggle_mode_release_is_ignored() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Released), None);
        assert!(
            lifecycle.is_dictating(),
            "holding the key must not stop toggle dictation"
        );
    }
    #[test]
    fn cancel_while_dictating_emits_cancel_and_returns_to_idle() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        lifecycle.on_hotkey(HotkeyInput::Pressed);
        assert_eq!(lifecycle.cancel(), Some(DictationEvent::Cancel));
        assert!(!lifecycle.is_dictating());
    }
    #[test]
    fn cancel_while_idle_does_nothing() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(lifecycle.cancel(), None);
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn non_key_start_does_not_leave_toggle_hotkey_down() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        assert_eq!(lifecycle.start(), Some(DictationEvent::Start));
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Pressed),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn hold_hotkey_can_stop_a_non_key_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Hold);
        assert_eq!(lifecycle.start(), Some(DictationEvent::Start));
        assert_eq!(lifecycle.on_hotkey(HotkeyInput::Pressed), None);
        assert_eq!(
            lifecycle.on_hotkey(HotkeyInput::Released),
            Some(DictationEvent::Stop)
        );
        assert!(!lifecycle.is_dictating());
    }

    #[test]
    fn bar_stop_resets_a_non_key_dictation() {
        let mut lifecycle = DictationLifecycle::new(ActivationMode::Toggle);
        lifecycle.start();
        assert_eq!(lifecycle.stop(), Some(DictationEvent::Stop));
        assert_eq!(lifecycle.start(), Some(DictationEvent::Start));
    }
}
