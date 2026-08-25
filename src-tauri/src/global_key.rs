//! Bare-Escape arming (ADR-0014, ADR-0004): the invariant that there is never
//! an active but uncancellable Dictation.
//!
//! Bare Escape may only be global while a dictation is active — otherwise
//! Slugtale steals Escape from whatever the user is doing. Every activation
//! path (Hotkey press, Voice Activation wake phrase) arms Escape *before*
//! recording starts and disarms it the moment the lifecycle leaves dictating,
//! including on rollback when a later step fails. This module owns that
//! decision: one arbiter holds the armed fact and answers each request with
//! the one OS change (if any) that satisfies it. Callers never touch the flag.

/// A request to change bare Escape's global registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeCommand {
    /// Make Escape global now — recording is about to start.
    Arm,
    /// Make Escape local again — dictation ended or its begin rolled back.
    Disarm,
    /// Bring the registration in line with whether a dictation is active.
    MatchDictation(bool),
}

/// Whether bare Escape is currently global. The single writer for the whole
/// app; the OS change it names is applied elsewhere, once per real transition.
#[derive(Default)]
pub struct EscapeArbiter {
    armed: bool,
}

impl EscapeArbiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Resolve a command against the armed state. `Some(true)` registers the
    /// key, `Some(false)` unregisters it, `None` means the request is already
    /// satisfied and the OS must not be touched.
    pub fn resolve(&mut self, command: EscapeCommand) -> Option<bool> {
        let should_arm = match command {
            EscapeCommand::Arm => true,
            EscapeCommand::Disarm => false,
            EscapeCommand::MatchDictation(dictating) => dictating,
        };
        if should_arm == self.armed {
            return None;
        }
        self.armed = should_arm;
        Some(should_arm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arming_registers_exactly_once() {
        let mut arbiter = EscapeArbiter::new();
        assert_eq!(arbiter.resolve(EscapeCommand::Arm), Some(true));
        assert_eq!(arbiter.resolve(EscapeCommand::Arm), None);
        assert_eq!(arbiter.resolve(EscapeCommand::MatchDictation(true)), None);
    }

    #[test]
    fn disarming_when_idle_touches_nothing() {
        let mut arbiter = EscapeArbiter::new();
        assert_eq!(arbiter.resolve(EscapeCommand::Disarm), None);
        assert_eq!(arbiter.resolve(EscapeCommand::MatchDictation(false)), None);
        assert!(!arbiter.is_armed());
    }

    #[test]
    fn a_failed_begin_rolls_back_to_one_register_and_one_unregister() {
        // Begin arms before recording starts; the failed step rolls back. The
        // OS sees exactly one of each, whatever order the requests arrive in.
        let mut arbiter = EscapeArbiter::new();
        assert_eq!(arbiter.resolve(EscapeCommand::Arm), Some(true));
        assert_eq!(arbiter.resolve(EscapeCommand::Disarm), Some(false));
        assert_eq!(arbiter.resolve(EscapeCommand::Disarm), None);
        assert!(!arbiter.is_armed());
    }

    #[test]
    fn matching_dictation_follows_the_lifecycle_both_ways() {
        let mut arbiter = EscapeArbiter::new();
        assert_eq!(
            arbiter.resolve(EscapeCommand::MatchDictation(true)),
            Some(true)
        );
        assert_eq!(arbiter.resolve(EscapeCommand::MatchDictation(true)), None);
        assert_eq!(
            arbiter.resolve(EscapeCommand::MatchDictation(false)),
            Some(false)
        );
        assert_eq!(arbiter.resolve(EscapeCommand::MatchDictation(false)), None);
    }

    #[test]
    fn cancel_and_restart_within_one_lifecycle_stays_consistent() {
        // Stop ends one dictation and Start begins the next before the worker
        // drains: the last command decides, without duplicate registrations.
        let mut arbiter = EscapeArbiter::new();
        assert_eq!(
            arbiter.resolve(EscapeCommand::MatchDictation(true)),
            Some(true)
        );
        assert_eq!(
            arbiter.resolve(EscapeCommand::MatchDictation(false)),
            Some(false)
        );
        assert_eq!(
            arbiter.resolve(EscapeCommand::MatchDictation(true)),
            Some(true)
        );
        assert!(arbiter.is_armed());
    }
}
