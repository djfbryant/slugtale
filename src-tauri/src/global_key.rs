
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeCommand {
    Arm,
    Disarm,
    MatchDictation(bool),
}

#[derive(Default)]
pub struct EscapeArbiter {
    armed: bool,
}

impl EscapeArbiter {
    pub fn new() -> Self {
        Self::default()
    }

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

    /// Resolve `command`, run `ask_os` for the single OS registration change it
    /// names, and commit the new armed state only once the OS accepted it.
    ///
    /// An `ask_os` that fails leaves the arbiter exactly where it was, so the
    /// same request stays retryable: the next Arm after a refused registration
    /// reaches the OS again instead of finding the fact already set.
    pub fn apply(
        &mut self,
        command: EscapeCommand,
        ask_os: impl FnOnce(bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(should_register) = self.pending_registration(command) else {
            return Ok(());
        };

        ask_os(should_register)?;
        self.record_os_accepted(should_register);
        Ok(())
    }

    fn record_os_accepted(&mut self, armed: bool) {
        self.armed = armed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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

        let os = FakeOs::default();
        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(os.accepting(), [true]);
        assert!(arbiter.is_armed());
    }

    #[test]
    fn a_refused_unregistration_stays_retryable_and_keeps_the_key_global() {
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
        let mut arbiter = EscapeArbiter::new();
        let refusing = FakeOs::refusing();
        assert!(arbiter.apply(EscapeCommand::Arm, refusing.change()).is_err());

        assert!(arbiter.apply(EscapeCommand::Arm, refusing.change()).is_err());
        assert_eq!(refusing.accepting(), [true, true]);
    }

    #[test]
    fn a_satisfied_request_still_runs_no_os_change() {
        let mut arbiter = EscapeArbiter::new();
        let os = FakeOs::default();

        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(arbiter.apply(EscapeCommand::Arm, os.change()), Ok(()));
        assert_eq!(os.accepting(), [true]);
    }
}