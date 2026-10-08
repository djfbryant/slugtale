//! Bare-Escape arming (ADR-0014, ADR-0004): the invariant that there is never
//! an active but uncancellable Dictation.
//!
//! Bare Escape may only be global while a dictation is active — otherwise
//! Slugtale steals Escape from whatever the user is doing. Every activation
//! path (Hotkey press, Voice Activation wake phrase) arms Escape *before*
//! recording starts and disarms it the moment the lifecycle leaves dictating,
//! including on rollback when a later step fails. This module owns that
//! decision: one arbiter holds the armed fact and answers each request by
//! running exactly the one OS change (if any) that satisfies it. Callers never
//! touch the flag.
//!
//! The armed fact is only recorded once the OS has accepted the change
//! ([`EscapeArbiter::apply`]). A failed registration is not a decision, it is a
//! request the OS refused: leaving the fact behind it would make the next Arm
//! look already satisfied, and the dictation after that would run with no global
//! Escape at all (slugtale-7kxk).

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

    /// Whether Escape currently has an armed registration. Test-only: production
    /// changes arming through `apply` and never asks whether it already holds.
    #[cfg(test)]
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// What the OS must do for `command`, without recording that it did.
    /// `None` means the request is already satisfied and the OS must not be
    /// touched.
    fn pending_registration(&self, command: EscapeCommand) -> Option<bool> {
        let should_arm = match command {
            EscapeCommand::Arm => true,
            EscapeCommand::Disarm => false,
            EscapeCommand::MatchDictation(dictating) => dictating,
        };
        (should_arm != self.armed).then_some(should_arm)
    }

    /// Resolve `command`, run `change` for the single OS registration change it
    /// names, and commit the new armed state only once the OS accepted it.
    ///
    /// A `change` that fails leaves the arbiter exactly where it was, so the
    /// same request stays retryable: the next Arm after a refused registration
    /// reaches the OS again instead of finding the fact already set.
    pub fn apply(
        &mut self,
        command: EscapeCommand,
        change: impl FnOnce(bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(should_register) = self.pending_registration(command) else {
            return Ok(());
        };

        change(should_register)?;
        self.armed = should_register;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// An operating system that answers whatever the test scripted, and records
    /// what it was asked — the injected outcome, so a failed registration is
    /// tested without a desktop session.
    #[derive(Clone, Default)]
    struct FakeOs {
        calls: Arc<Mutex<Vec<bool>>>,
        fail: Arc<Mutex<bool>>,
    }

    impl FakeOs {
        fn refusing() -> Self {
            Self {
                fail: Arc::new(Mutex::new(true)),
                ..Default::default()
            }
        }

        fn accepting(&self) -> Vec<bool> {
            self.calls.lock().unwrap().clone()
        }

        fn change(&self) -> impl FnOnce(bool) -> Result<(), String> {
            let calls = Arc::clone(&self.calls);
            let fail = Arc::clone(&self.fail);
            move |should_register| {
                calls.lock().unwrap().push(should_register);
                if *fail.lock().unwrap() {
                    Err("the OS refused the registration".to_string())
                } else {
                    Ok(())
                }
            }
        }
    }

    #[test]
    fn arming_registers_exactly_once() {
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(true), os.change()),
            Ok(())
        );

        assert_eq!(os.accepting(), [true]);
        assert!(arbiter.is_armed());
    }

    #[test]
    fn disarming_when_idle_touches_nothing() {
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(arbiter.apply(EscapeCommand::Disarm, os.change()), Ok(()));
        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(false), os.change()),
            Ok(())
        );

        assert!(os.accepting().is_empty());
        assert!(!arbiter.is_armed());
    }

    #[test]
    fn a_failed_begin_rolls_back_to_one_register_and_one_unregister() {
        // Begin arms before recording starts; the failed step rolls back. The
        // OS sees exactly one of each, whatever order the requests arrive in.
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(arbiter.apply(EscapeCommand::Disarm, os.change()), Ok(()));
        assert_eq!(arbiter.apply(EscapeCommand::Disarm, os.change()), Ok(()));

        assert_eq!(os.accepting(), [true, false]);
        assert!(!arbiter.is_armed());
    }

    #[test]
    fn matching_dictation_follows_the_lifecycle_both_ways() {
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(true), os.change()),
            Ok(())
        );
        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(true), os.change()),
            Ok(())
        );
        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(false), os.change()),
            Ok(())
        );
        assert_eq!(
            arbiter.apply(EscapeCommand::MatchDictation(false), os.change()),
            Ok(())
        );

        assert_eq!(os.accepting(), [true, false]);
    }

    #[test]
    fn cancel_and_restart_within_one_lifecycle_stays_consistent() {
        // Stop ends one dictation and Start begins the next before the worker
        // drains: the last command decides, without duplicate registrations.
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        for dictating in [true, false, true] {
            assert_eq!(
                arbiter.apply(EscapeCommand::MatchDictation(dictating), os.change()),
                Ok(())
            );
        }

        assert_eq!(os.accepting(), [true, false, true]);
        assert!(arbiter.is_armed());
    }

    #[test]
    fn a_refused_registration_leaves_the_next_arm_retryable() {
        // The bug: the armed fact was written before the OS was asked, so a
        // registration that failed looked done. The next Start then skipped the
        // registration and recorded with no global Escape (slugtale-7kxk).
        let mut arbiter = EscapeArbiter::new();
        let refusing = FakeOs::refusing();

        let error = arbiter
            .apply(EscapeCommand::Arm, refusing.change())
            .expect_err("the OS refuses the first registration");

        assert_eq!(error, "the OS refused the registration");
        assert!(
            !arbiter.is_armed(),
            "a refused registration is not a decision the arbiter may keep"
        );
        assert_eq!(refusing.accepting(), [true]);

        // The next Arm reaches the OS again rather than looking satisfied.
        let os = FakeOs::default();
        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(os.accepting(), [true]);
        assert!(arbiter.is_armed());
    }

    #[test]
    fn a_refused_unregistration_stays_retryable_and_keeps_the_key_global() {
        // The other direction: while the fact says armed, a refusal must not
        // leave the arbiter believing Escape is local when the OS still has it
        // global.
        let mut arbiter = EscapeArbiter::new();
        let accepting = FakeOs::default();
        assert_eq!(arbiter.apply(EscapeCommand::Arm, accepting.change()), Ok(()));

        let refusing = FakeOs::refusing();
        assert!(arbiter
            .apply(EscapeCommand::Disarm, refusing.change())
            .is_err());

        assert!(
            arbiter.is_armed(),
            "the OS still holds the registration, so the fact must still say so"
        );
        assert_eq!(refusing.accepting(), [false]);
        assert_eq!(
            accepting.accepting(),
            [true],
            "the refused unregistration must not have asked for anything else"
        );

        let retry = FakeOs::default();
        assert_eq!(arbiter.apply(EscapeCommand::Disarm, retry.change()), Ok(()));
        assert_eq!(retry.accepting(), [false]);
        assert!(!arbiter.is_armed());
    }

    #[test]
    fn a_refused_registration_is_repeated_rather_than_skipped() {
        // Two Arm requests after one refusal: the arbiter is still unarmed, so
        // neither is already satisfied and both reach the OS.
        let mut arbiter = EscapeArbiter::new();
        let refusing = FakeOs::refusing();
        assert!(arbiter.apply(EscapeCommand::Arm, refusing.change()).is_err());

        assert!(arbiter.apply(EscapeCommand::Arm, refusing.change()).is_err());
        assert_eq!(refusing.accepting(), [true, true]);
    }

    #[test]
    fn a_satisfied_request_still_runs_no_os_change() {
        // The commit-after-success rule must not invent work: once a request has
        // been satisfied by a committed change, repeating it touches nothing.
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(os.accepting(), [true]);
    }
}